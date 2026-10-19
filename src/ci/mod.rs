//! CI. The merge path, the checks endpoint, and ref-change handling talk to CI
//! only through `Ci`; the runner talks to it through the runner API
//! (`api.rs`). See `docs/ci.md`.
//!
//! - The pipeline (`.branvil/ci.toml`) is read from the commit itself, for a
//!   tip of the default branch and for a pull request head alike. What a head
//!   cannot change is the repository's settings: the required checks and
//!   `ci_network`.
//! - Runs and attempts live in redb (`queue.rs`); logs and artifacts are files
//!   under `<data>/ci/` (`files.rs`).
//! - A background task expires leases, fails queued work when no runner shows
//!   up, admits waiting runs, registers every wanted tip again (a run a crash
//!   or a failed registration missed, a run recorded with another pipeline),
//!   and applies retention.

pub mod admin;
pub mod api;
pub mod config;
pub mod files;
pub mod pipeline;
pub mod queue;
pub mod settings;
pub mod supervise;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::app::App;
use crate::config::Layout;
use crate::error::AppResult;
use crate::events::Events;
use crate::git::{Git, ZERO_SHA};
use crate::model::Repo;
use crate::refs::RefChange;
use crate::store::{self, Key, Rd, Store};
use crate::util;
use config::CiConfig;
use queue::{Attempt, PipelineState, Registration};

/// One check of a commit, in the shape of `docs/api.md` "Checks".
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    /// `queued`, `in_progress`, or `completed`.
    pub status: String,
    /// When completed: `success`, `failure`, `cancelled`, `timed_out`,
    /// `skipped`, or `startup_failure`.
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_url: Option<String>,
    /// Why the check is where it is (a failure reason, "superseded", "no
    /// runner", "waiting for room in the queue", ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub artifact_names: Vec<String>,
}

impl Check {
    /// A check that has a recorded attempt (as opposed to a placeholder for a
    /// required check the run does not have yet).
    pub fn recorded(&self) -> bool {
        self.attempt_id.is_some()
    }

    pub fn succeeded(&self) -> bool {
        self.status == "completed" && self.conclusion.as_deref() == Some("success")
    }

    fn placeholder(name: &str, status: &str, conclusion: Option<&str>, details: String) -> Self {
        Check {
            name: name.to_string(),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            attempt_id: None,
            attempt: None,
            details_url: None,
            details: Some(details),
            artifact_names: Vec::new(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Checks {
    pub checks: Vec<Check>,
    /// True when the commit has a pipeline or the repository has required
    /// checks: no checks then means "not registered yet", never "no checks".
    pub checks_expected: bool,
}

/// What the service last heard from a runner (kept in memory: a restarted
/// service learns it again from the runner's next request).
#[derive(Serialize, Clone, Debug)]
pub struct RunnerSeen {
    pub name: String,
    pub last_seen_at: String,
    pub executor: String,
    pub executor_state: String,
    pub executor_detail: Option<String>,
    #[serde(skip)]
    seen: Option<Instant>,
}

#[derive(Clone)]
pub struct Ci {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    git: Git,
    events: Events,
    base_url: String,
    layout: Layout,
    config: CiConfig,
    settings: settings::Control,
    started: Instant,
    runners: Mutex<BTreeMap<String, RunnerSeen>>,
    /// Serializes log appends (one file per attempt, small writes).
    log_lock: tokio::sync::Mutex<()>,
}

impl Ci {
    pub fn new(
        store: Store,
        git: Git,
        events: Events,
        base_url: String,
        layout: Layout,
        config: CiConfig,
        config_file: std::path::PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                git,
                events,
                base_url,
                layout,
                settings: settings::Control::new(config_file, &config),
                config,
                started: Instant::now(),
                runners: Mutex::new(BTreeMap::new()),
                log_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn config(&self) -> &CiConfig {
        &self.inner.config
    }

    /// What the owner changes from the GUI (`settings.rs`).
    pub fn settings(&self) -> &settings::Control {
        &self.inner.settings
    }

    /// The jobs of the pipeline on the tip of `repo`'s default branch (none
    /// without a valid one).
    pub async fn default_jobs(&self, repo: &Repo) -> Vec<pipeline::JobDef> {
        let dir = self.inner.git.repo_dir(&repo.path);
        let tip = format!("refs/heads/{}^{{commit}}", repo.default_branch);
        let sha = match self
            .inner
            .git
            .run(Some(&dir), &["rev-parse", "--verify", "-q", &tip])
            .await
        {
            Ok(out) if out.ok() => out.text().trim().to_string(),
            _ => return Vec::new(),
        };
        match self.read_pipeline(repo, &sha).await {
            Ok(PipelineState::Jobs { jobs }) => jobs,
            _ => Vec::new(),
        }
    }

    fn job_url(&self, repo_path: &str, id: &str) -> String {
        format!("{}/{repo_path}/-/jobs/{id}", self.inner.base_url)
    }

    fn publish(&self, changed: &[(String, String)]) {
        for (path, sha) in changed {
            self.inner.events.publish("check", path, sha.clone());
        }
    }

    fn check_of(&self, a: &Attempt) -> Check {
        Check {
            name: a.job.name.clone(),
            status: a.status.clone(),
            conclusion: a.conclusion.clone(),
            attempt_id: Some(a.id.clone()),
            attempt: Some(a.number),
            details_url: Some(self.job_url(&a.repo_path, &a.id)),
            details: a.details.clone(),
            artifact_names: a.artifacts.iter().map(|x| x.name.clone()).collect(),
        }
    }

    /// The checks of `sha` in `repo`: the latest attempt of every job of the
    /// commit's run, plus an entry for each required check the run does not
    /// have (see `docs/api.md`, "Checks"). Only attempts of the run's current
    /// job definitions count. Nothing here ever reports success without a
    /// successful attempt of that commit.
    pub async fn head_checks(&self, repo: &Repo, sha: &str) -> AppResult<Checks> {
        let (id, s) = (repo.id.clone(), sha.to_string());
        // Attempts of a job the pipeline no longer has, or had with another
        // definition, are history, not checks.
        let (run, latest) = self
            .inner
            .store
            .read(move |r| {
                let run = queue::get_run(r, &id, &s)?;
                let latest = match run.as_ref().map(|x| &x.pipeline) {
                    Some(PipelineState::Jobs { jobs }) => {
                        queue::current_attempts(r, &id, &s, jobs)?
                    }
                    _ => Vec::new(),
                };
                Ok((run, latest))
            })
            .await?;
        let mut checks = Vec::new();
        let Some(run) = run else {
            for name in &repo.required_checks {
                checks.push(Check::placeholder(
                    name,
                    "queued",
                    None,
                    "No CI run is registered for this commit yet.".into(),
                ));
            }
            // An unreadable pipeline is expected rather than "no checks".
            let expected = !repo.required_checks.is_empty()
                || !matches!(
                    self.read_pipeline(repo, sha).await,
                    Ok(PipelineState::Absent)
                );
            return Ok(Checks {
                checks,
                checks_expected: expected,
            });
        };
        if let PipelineState::Invalid { message } = &run.pipeline {
            checks.push(Check::placeholder(
                "pipeline",
                "completed",
                Some("failure"),
                format!(
                    "The {} of {} is invalid: {message}",
                    pipeline::PIPELINE_PATH,
                    run.sha
                ),
            ));
        }
        for a in &latest {
            checks.push(self.check_of(a));
        }
        if let PipelineState::Jobs { jobs } = &run.pipeline {
            for j in jobs
                .iter()
                .filter(|j| !latest.iter().any(|a| a.job.name == j.name))
            {
                checks.push(match &run.superseded_by {
                    Some(by) => Check::placeholder(
                        &j.name,
                        "completed",
                        Some("cancelled"),
                        format!("Superseded by {by} before it was queued."),
                    ),
                    None if run.waiting => Check::placeholder(
                        &j.name,
                        "queued",
                        None,
                        format!(
                            "Waiting for room in the CI queue ({} attempts at most).",
                            self.inner.config.queue_max
                        ),
                    ),
                    None => Check::placeholder(
                        &j.name,
                        "queued",
                        None,
                        "Not queued yet; the reconciler queues it within a minute.".into(),
                    ),
                });
            }
        }
        for name in &repo.required_checks {
            if checks.iter().any(|c| &c.name == name) {
                continue;
            }
            let why = match &run.pipeline {
                PipelineState::Absent => format!(
                    "{} has no {}, so the required check {name} cannot run.",
                    run.sha,
                    pipeline::PIPELINE_PATH
                ),
                PipelineState::Invalid { .. } => {
                    format!("The pipeline is invalid, so the required check {name} cannot run.")
                }
                PipelineState::Jobs { .. } => format!(
                    "The {} of {} defines no job named {name}.",
                    pipeline::PIPELINE_PATH,
                    run.sha
                ),
            };
            checks.push(Check::placeholder(name, "completed", Some("failure"), why));
        }
        Ok(Checks {
            checks,
            checks_expected: run.pipeline != PipelineState::Absent
                || !repo.required_checks.is_empty(),
        })
    }

    /// Reads the pipeline of commit `sha` (`pipeline::read`).
    async fn read_pipeline(&self, repo: &Repo, sha: &str) -> AppResult<PipelineState> {
        let dir = self.inner.git.repo_dir(&repo.path);
        pipeline::read(&self.inner.git, &dir, sha).await
    }

    /// Registers CI of `sha` for `branch` when the branch wants CI: it is the
    /// default branch, or the head of an open pull request. The pipeline is
    /// the commit's own. The caller holds the repository lock.
    async fn consider(&self, repo: &Repo, branch: &str, sha: &str) -> AppResult<bool> {
        if branch != repo.default_branch {
            let (id, b) = (repo.id.clone(), branch.to_string());
            if !self
                .inner
                .store
                .read(move |r| has_open_pull(r, &id, &b))
                .await?
            {
                return Ok(false);
            }
        }
        let reg = Registration {
            repo: repo.id.clone(),
            repo_path: repo.path.clone(),
            sha: sha.to_string(),
            branch: branch.to_string(),
            protected: repo.is_protected(branch),
            pipeline: self.read_pipeline(repo, sha).await?,
            network: repo.ci_network,
        };
        let queue_max = self.inner.config.queue_max;
        let now = util::now();
        self.inner
            .store
            .write(move |w| queue::register(w, reg, queue_max, &now))
            .await
    }

    /// Called once per batch of ref changes Branvil recorded (push, merge,
    /// startup rescan), after the database reflects them, under the
    /// repository lock. Older commits a branch left are superseded; new tips
    /// of the default branch and of pull request heads get a run.
    pub async fn on_ref_changes(&self, repo: &Repo, changes: &[RefChange]) {
        if let Err(e) = self.apply_ref_changes(repo, changes).await {
            // The reconciler registers what this missed.
            tracing::error!("CI registration for {} failed: {e}", repo.path);
        }
    }

    async fn apply_ref_changes(&self, repo: &Repo, changes: &[RefChange]) -> AppResult<()> {
        let mut left = false;
        for c in changes {
            let Some(branch) = c.refname.strip_prefix("refs/heads/") else {
                continue;
            };
            if c.old != ZERO_SHA {
                let (id, old, b) = (repo.id.clone(), c.old.clone(), branch.to_string());
                let by = if c.new == ZERO_SHA {
                    "a deletion of the branch".to_string()
                } else {
                    c.new.clone()
                };
                let now = util::now();
                if self
                    .inner
                    .store
                    .write(move |w| queue::branch_left(w, &id, &old, &b, &by, &now))
                    .await?
                {
                    left = true;
                    self.publish(&[(repo.path.clone(), c.old.clone())]);
                }
            }
            if c.new != ZERO_SHA {
                // A failed registration does not hold up the other changes;
                // the reconciler retries it.
                match self.consider(repo, branch, &c.new).await {
                    Ok(true) => self.publish(&[(repo.path.clone(), c.new.clone())]),
                    Ok(false) => {}
                    Err(e) => tracing::error!(
                        "CI registration of {} {branch} {} failed: {e}",
                        repo.path,
                        c.new
                    ),
                }
            }
        }
        if left {
            self.admit_waiting().await;
        }
        Ok(())
    }

    /// A pull request was opened or reopened: its head gets a run.
    pub async fn on_pull_opened(&self, app: &App, repo: &Repo, head: &str) {
        let lock = app.repo_lock(&repo.id);
        let _guard = lock.lock().await;
        let dir = self.inner.git.repo_dir(&repo.path);
        let result = async {
            if let Some(sha) = self.inner.git.branch_tips(&dir).await?.get(head)
                && self.consider(repo, head, sha).await?
            {
                self.publish(&[(repo.path.clone(), sha.clone())]);
            }
            AppResult::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!("CI registration for {} {head} failed: {e}", repo.path);
        }
    }

    /// Registers the tips of the default branch and of every open pull
    /// request head again: a run that is missing (a crash between a push and
    /// its registration, a failed registration) is created, and a run whose
    /// recorded pipeline is not the commit's own gets it. Returns how many
    /// runs changed.
    pub async fn reconcile(&self, app: &App) -> AppResult<usize> {
        let repos: Vec<Repo> = self
            .inner
            .store
            .read(|r| r.prefix_all(store::REPOS, &[]))
            .await?;
        let mut registered = 0;
        for repo in repos {
            let lock = app.repo_lock(&repo.id);
            let _guard = lock.lock().await;
            let dir = self.inner.git.repo_dir(&repo.path);
            let tips = self.inner.git.branch_tips(&dir).await?;
            let id = repo.id.clone();
            let mut wanted: Vec<String> = self
                .inner
                .store
                .read(move |r| open_pull_heads(r, &id))
                .await?;
            wanted.push(repo.default_branch.clone());
            wanted.sort();
            wanted.dedup();
            for branch in wanted {
                let Some(sha) = tips.get(&branch) else {
                    continue;
                };
                match self.consider(&repo, &branch, sha).await {
                    Ok(true) => {
                        tracing::warn!("CI: reconciled the run of {} {branch} {sha}", repo.path);
                        registered += 1;
                        self.publish(&[(repo.path.clone(), sha.clone())]);
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!(
                        "CI: registering {} {branch} {sha} failed (retried in a minute): {e}",
                        repo.path
                    ),
                }
            }
        }
        Ok(registered)
    }

    async fn admit_waiting(&self) {
        let (queue_max, now) = (self.inner.config.queue_max, util::now());
        match self
            .inner
            .store
            .write(move |w| queue::admit_waiting(w, queue_max, &now))
            .await
        {
            Ok(admitted) => self.publish(&admitted),
            Err(e) => tracing::error!("CI: admitting waiting runs failed: {e}"),
        }
    }

    /// One pass of the periodic work: expired leases, queued work nobody
    /// takes, waiting runs.
    pub async fn sweep(&self) -> AppResult<()> {
        let now = util::now();
        let changed = self
            .inner
            .store
            .write(move |w| queue::expire(w, &now))
            .await?;
        self.publish(&changed);
        let absent = self.inner.config.runner_absent_secs;
        let (last_contact, last_text) = {
            let runners = self.inner.runners.lock().unwrap();
            (
                runners
                    .values()
                    .filter_map(|r| r.seen)
                    .max()
                    .unwrap_or(self.inner.started)
                    .max(self.inner.started),
                runners.values().map(|r| r.last_seen_at.clone()).max(),
            )
        };
        if last_contact.elapsed() > Duration::from_secs(absent) {
            let details = format!(
                "No CI runner has contacted Branvil for {absent} s (last contact: {}). Start `branvil runner` (docs/ci.md) and retry the job.",
                last_text.unwrap_or_else(|| "none since Branvil started".into())
            );
            let now = util::now();
            let before = queue::earlier(&now, absent);
            let changed = self
                .inner
                .store
                .write(move |w| queue::fail_queued(w, &before, &details, &now))
                .await?;
            self.publish(&changed);
        }
        self.admit_waiting().await;
        Ok(())
    }

    /// Forgets a runner that was stopped, so the diagnostics stop counting it.
    pub fn forget_runner(&self, name: &str) {
        self.inner.runners.lock().unwrap().remove(&util::fold(name));
    }

    /// Notes a runner request (for diagnostics and the no-runner rule).
    pub fn saw_runner(&self, name: &str, executor: Option<(String, String, Option<String>)>) {
        let mut runners = self.inner.runners.lock().unwrap();
        let entry = runners
            .entry(util::fold(name))
            .or_insert_with(|| RunnerSeen {
                name: name.to_string(),
                last_seen_at: String::new(),
                executor: String::new(),
                executor_state: "unknown".into(),
                executor_detail: None,
                seen: None,
            });
        entry.last_seen_at = util::now();
        entry.seen = Some(Instant::now());
        if let Some((executor, state, detail)) = executor {
            entry.executor = executor;
            entry.executor_state = state;
            entry.executor_detail = detail;
        }
    }

    /// Diagnostics section for `GET /api/v1/admin/diagnostics`.
    pub async fn diagnostics(&self) -> serde_json::Value {
        let counts = self.inner.store.read(queue::counts).await;
        let runners: Vec<RunnerSeen> = self
            .inner
            .runners
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let absent = Duration::from_secs(self.inner.config.runner_absent_secs);
        let live: Vec<&RunnerSeen> = runners
            .iter()
            .filter(|r| r.seen.is_some_and(|s| s.elapsed() <= absent))
            .collect();
        let (state, detail) = if live.is_empty() {
            (
                "no_runner",
                format!(
                    "No runner has contacted Branvil in the last {} s; queued jobs conclude startup_failure.",
                    absent.as_secs()
                ),
            )
        } else if live.iter().any(|r| r.executor_state == "ready") {
            ("ready", format!("{} runner(s) connected.", live.len()))
        } else {
            (
                "executor_unavailable",
                live.iter()
                    .find_map(|r| r.executor_detail.clone())
                    .unwrap_or_else(|| "The runner's executor is not usable.".into()),
            )
        };
        let queue = match counts {
            Ok((queued, running, waiting)) => serde_json::json!({
                "queued": queued, "in_progress": running, "waiting_runs": waiting,
                "limit": self.inner.config.queue_max,
            }),
            Err(e) => serde_json::json!({"error": e.message}),
        };
        serde_json::json!({
            "state": state,
            "detail": detail,
            "mode": self.inner.settings.mode(),
            "runners": runners,
            "queue": queue,
        })
    }

    /// Starts the periodic work: sweep every 5 s, reconcile every minute,
    /// retention every hour, and one orphan-file collection at start.
    pub fn start(app: crate::app::Shared) {
        tokio::spawn(async move {
            let ci = app.ci.clone();
            match files::collect_orphans(&app).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("CI: removed {n} orphaned log/artifact file(s)"),
                Err(e) => tracing::warn!("CI: orphan collection failed: {e}"),
            }
            let mut tick: u64 = 0;
            loop {
                if tick.is_multiple_of(12)
                    && let Err(e) = ci.reconcile(&app).await
                {
                    tracing::warn!("CI: reconcile failed: {e}");
                }
                if let Err(e) = ci.sweep().await {
                    tracing::warn!("CI: sweep failed: {e}");
                }
                if tick.is_multiple_of(720)
                    && let Err(e) = files::apply_retention(&app).await
                {
                    tracing::warn!("CI: retention failed: {e}");
                }
                tick += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

/// Whether an open pull request has `branch` as its head.
fn has_open_pull(r: &impl Rd, repo: &str, branch: &str) -> AppResult<bool> {
    let prefix = Key::new().s(repo).s(branch).bytes();
    Ok(!r
        .prefix_raw(store::IX_OPEN_PULLS, &prefix, None, 1)?
        .is_empty())
}

/// The head branches of every open pull request of a repository.
fn open_pull_heads(r: &impl Rd, repo: &str) -> AppResult<Vec<String>> {
    let mut heads = Vec::new();
    for (k, _) in r.prefix_raw(
        store::IX_OPEN_PULLS,
        &Key::new().s(repo).bytes(),
        None,
        usize::MAX,
    )? {
        if let Some(p) = store::decode_key(&k, "sss") {
            heads.push(p[1].s().to_string());
        }
    }
    Ok(heads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::pipeline::JobDef;

    fn setup() -> (tempfile::TempDir, Ci, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let store = Store::open(&dir.path().join("t.redb"), 1 << 20, 1).unwrap();
        let git = Git::new(layout.clone(), 2, Duration::from_secs(10));
        let config = CiConfig {
            queue_max: 1,
            ..CiConfig::default()
        };
        let ci = Ci::new(
            store,
            git,
            Events::new(),
            "http://127.0.0.1:7878".into(),
            layout,
            config,
            std::path::PathBuf::from("/nonexistent/branvil.toml"),
        );
        let repo = Repo {
            id: "repo_1".into(),
            path: "acme/demo".into(),
            workspace: "ws_1".into(),
            default_branch: "main".into(),
            protected_branches: vec![],
            required_checks: vec!["test".into()],
            ci_network: false,
            labels: vec![],
            upstream: None,
            revision: 1,
            created_at: String::new(),
        };
        (dir, ci, repo)
    }

    fn job(name: &str) -> JobDef {
        JobDef {
            name: name.into(),
            image: "alpine".into(),
            commands: vec!["true".into()],
            timeout_secs: 60,
            cpus: 1,
            memory_mib: 256,
            artifacts: vec![],
            cache: None,
        }
    }

    #[tokio::test]
    async fn a_commit_without_a_run_is_pending_never_no_checks() {
        let (_d, ci, mut repo) = setup();
        let c = ci.head_checks(&repo, "s1").await.unwrap();
        assert!(c.checks_expected);
        assert_eq!(c.checks.len(), 1);
        assert_eq!(c.checks[0].status, "queued");
        assert!(!c.checks[0].recorded() && !c.checks[0].succeeded());
        assert!(
            c.checks[0]
                .details
                .as_deref()
                .unwrap()
                .contains("No CI run")
        );
        // Without required checks and without a pipeline there is nothing to expect.
        repo.required_checks.clear();
        let c = ci.head_checks(&repo, "s1").await.unwrap();
        assert!(!c.checks_expected && c.checks.is_empty());
    }

    #[tokio::test]
    async fn a_run_takes_its_commits_pipeline_and_counts_only_its_jobs() {
        let (_d, ci, repo) = setup();
        let reg = |jobs: Vec<JobDef>| Registration {
            repo: repo.id.clone(),
            repo_path: repo.path.clone(),
            sha: "s1".into(),
            branch: "feature".into(),
            protected: false,
            pipeline: PipelineState::Jobs { jobs },
            network: false,
        };
        let run = |r: Registration| {
            let now = util::now();
            ci.inner
                .store
                .write(move |w| queue::register(w, r, 10, &now))
        };
        let succeed = || {
            ci.inner.store.write(|w| {
                let now = util::now();
                let c = queue::claim(w, "r", 60, &now)?.unwrap();
                queue::finish(w, &c.attempt.id, &c.lease, "success", None, &now)
            })
        };
        let test = |c: &Checks| c.checks.iter().find(|x| x.name == "test").cloned().unwrap();
        // Recorded with another pipeline (a base's), and its test succeeded.
        assert!(run(reg(vec![job("test")])).await.unwrap());
        succeed().await.unwrap();
        assert!(test(&ci.head_checks(&repo, "s1").await.unwrap()).succeeded());
        // The commit's own pipeline has no test: the old success is not a check.
        assert!(run(reg(vec![job("lint")])).await.unwrap());
        let c = ci.head_checks(&repo, "s1").await.unwrap();
        let t = test(&c);
        assert_eq!(t.conclusion.as_deref(), Some("failure"));
        assert!(!t.recorded(), "{t:?}");
        assert!(c.checks.iter().any(|x| x.name == "lint" && x.recorded()));
        // Its test is another definition: it runs again, and only that counts.
        let mut changed = job("test");
        changed.commands = vec!["echo changed".into()];
        assert!(run(reg(vec![changed.clone(), job("lint")])).await.unwrap());
        let t = test(&ci.head_checks(&repo, "s1").await.unwrap());
        assert_eq!((t.status.as_str(), t.attempt), ("queued", Some(2)));
        // The same pipeline again changes nothing.
        assert!(!run(reg(vec![changed, job("lint")])).await.unwrap());
    }

    #[tokio::test]
    async fn waiting_runs_and_missing_jobs_show_why() {
        let (_d, ci, repo) = setup();
        let now = util::now();
        let reg = |sha: &str, jobs: Vec<JobDef>| Registration {
            repo: repo.id.clone(),
            repo_path: repo.path.clone(),
            sha: sha.into(),
            branch: "feature".into(),
            protected: false,
            pipeline: PipelineState::Jobs { jobs },
            network: false,
        };
        let (r1, r2) = (reg("s1", vec![job("lint")]), reg("s2", vec![job("test")]));
        let n = now.clone();
        ci.inner
            .store
            .write(move |w| {
                queue::register(w, r1, 1, &n)?;
                queue::register(w, r2, 1, &n)
            })
            .await
            .unwrap();
        // s1 took the only slot: s2's job waits, visibly.
        let c = ci.head_checks(&repo, "s2").await.unwrap();
        assert_eq!(c.checks.len(), 1);
        assert_eq!(c.checks[0].status, "queued");
        assert!(
            c.checks[0]
                .details
                .as_deref()
                .unwrap()
                .contains("Waiting for room")
        );
        // s1's pipeline has no job named like the required check.
        let c = ci.head_checks(&repo, "s1").await.unwrap();
        let test = c.checks.iter().find(|x| x.name == "test").unwrap();
        assert_eq!(test.conclusion.as_deref(), Some("failure"));
        assert!(
            test.details
                .as_deref()
                .unwrap()
                .contains("defines no job named test")
        );
        let lint = c.checks.iter().find(|x| x.name == "lint").unwrap();
        assert!(lint.recorded());
        assert_eq!(
            lint.details_url.as_deref().unwrap(),
            format!(
                "http://127.0.0.1:7878/acme/demo/-/jobs/{}",
                lint.attempt_id.as_deref().unwrap()
            )
        );
    }
}
