//! The persistent CI queue: runs (one per repository and commit) and attempts
//! (one per try of a job), and every state change between them. Each public
//! function here is one short write (or read) transaction body; nothing here
//! waits on Git, the network, or a runner.
//!
//! States of an attempt:
//!
//! ```text
//! queued ──claim──▶ in_progress ──finish──▶ completed(conclusion)
//!   │                   │  └─lease expires─▶ completed(cancelled, "interrupted")
//!   │                   │                     └─(first interruption) new attempt queued
//!   └─cancel/supersede─▶ completed(cancelled)     (in_progress: cancel_requested,
//!   └─no runner───────▶ completed(startup_failure)  the runner stops and finishes)
//! ```

use serde::{Deserialize, Serialize};

use super::pipeline::JobDef;
use crate::error::{AppError, AppResult};
use crate::model::{IxEntry, put_indexed};
use crate::store::{self, Key, Rd, Wtx};
use crate::util;

pub const CONCLUSIONS: [&str; 6] = [
    "success",
    "failure",
    "cancelled",
    "timed_out",
    "skipped",
    "startup_failure",
];

const MAX_DETAILS: usize = 4 << 10;

/// What the run's pipeline turned out to be.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PipelineState {
    /// The commit has no `.branvil/ci.toml`.
    Absent,
    /// It has one and it is invalid.
    Invalid {
        message: String,
    },
    Jobs {
        jobs: Vec<JobDef>,
    },
}

/// The CI of one commit of one repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Run {
    pub repo: String,
    pub repo_path: String,
    pub sha: String,
    pub seq: u64,
    pub created_at: String,
    /// Branches whose tip this commit is and that asked for CI of it. When
    /// the last one moves away, the run is superseded.
    pub branches: Vec<String>,
    /// The commit's own pipeline.
    pub pipeline: PipelineState,
    /// `protected` (a protected branch asked) or `branch:<name>`; selects
    /// which dependency caches the jobs may read and write.
    pub cache_scope: String,
    /// The jobs did not fit into the queue yet; admitted in order when room
    /// frees up.
    pub waiting: bool,
    pub superseded_by: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArtifactMeta {
    pub name: String,
    pub bytes: u64,
}

/// One try of one job.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Attempt {
    pub id: String,
    pub repo: String,
    pub repo_path: String,
    pub sha: String,
    pub job: JobDef,
    pub number: u64,
    pub seq: u64,
    pub status: String,
    pub conclusion: Option<String>,
    pub details: Option<String>,
    /// SHA-256 of the current lease (the lease itself is only given to the
    /// runner that claimed the attempt).
    pub lease_hash: Option<String>,
    pub lease_expires_at: Option<String>,
    pub cancel_requested: bool,
    pub runner: Option<String>,
    pub cache_scope: String,
    pub network: bool,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// This attempt was queued because the previous one was interrupted.
    pub after_interrupt: bool,
    /// Ended because a newer commit replaced its commit on every branch.
    pub superseded: bool,
    pub artifacts: Vec<ArtifactMeta>,
    /// Logs and artifacts were deleted by retention.
    pub files_pruned: bool,
}

impl Attempt {
    pub fn key(id: &str) -> Vec<u8> {
        Key::new().s(id).bytes()
    }
}

pub fn run_key(repo: &str, sha: &str) -> Vec<u8> {
    Key::new().s(repo).s(sha).bytes()
}

pub fn run_index(r: &Run) -> Vec<IxEntry> {
    if r.waiting {
        vec![IxEntry {
            table: store::IX_CI_WAITING,
            key: Key::new().u(r.seq).bytes(),
            value: run_key(&r.repo, &r.sha),
        }]
    } else {
        Vec::new()
    }
}

pub fn attempt_index(a: &Attempt) -> Vec<IxEntry> {
    let mut out = vec![IxEntry {
        table: store::IX_CI_SHA,
        key: Key::new()
            .s(&a.repo)
            .s(&a.sha)
            .s(&a.job.name)
            .u(a.number)
            .bytes(),
        value: a.id.as_bytes().to_vec(),
    }];
    match a.status.as_str() {
        "queued" => out.push(IxEntry {
            table: store::IX_CI_PENDING,
            key: Key::new().u(a.seq).bytes(),
            value: a.id.as_bytes().to_vec(),
        }),
        "in_progress" => out.push(IxEntry {
            table: store::IX_CI_ACTIVE,
            key: Key::new().s(&a.id).bytes(),
            value: Vec::new(),
        }),
        _ => {}
    }
    out
}

/// Every CI index entry the records imply (for `branvil admin check`).
pub fn expected_index(r: &impl Rd) -> AppResult<Vec<IxEntry>> {
    let mut out = Vec::new();
    for run in r.prefix_all::<Run>(store::CI_RUNS, &[])? {
        out.extend(run_index(&run));
    }
    for a in r.prefix_all::<Attempt>(store::CI_ATTEMPTS, &[])? {
        out.extend(attempt_index(&a));
    }
    Ok(out)
}

fn put_run(w: &Wtx, old: Option<&Run>, new: &Run) -> AppResult<()> {
    put_indexed(
        w,
        store::CI_RUNS,
        &run_key(&new.repo, &new.sha),
        old.map(run_index),
        new,
        run_index(new),
    )
}

fn put_attempt(w: &Wtx, old: Option<&Attempt>, new: &Attempt) -> AppResult<()> {
    put_indexed(
        w,
        store::CI_ATTEMPTS,
        &Attempt::key(&new.id),
        old.map(attempt_index),
        new,
        attempt_index(new),
    )
}

pub fn get_run(r: &impl Rd, repo: &str, sha: &str) -> AppResult<Option<Run>> {
    r.get(store::CI_RUNS, &run_key(repo, sha))
}

pub fn get_attempt(r: &impl Rd, id: &str) -> AppResult<Option<Attempt>> {
    r.get(store::CI_ATTEMPTS, &Attempt::key(id))
}

/// Every attempt of a commit, in (job, number) order.
pub fn attempts_of(r: &impl Rd, repo: &str, sha: &str) -> AppResult<Vec<Attempt>> {
    let prefix = Key::new().s(repo).s(sha).bytes();
    let mut out = Vec::new();
    for (_, id) in r.prefix_raw(store::IX_CI_SHA, &prefix, None, usize::MAX)? {
        let id = String::from_utf8_lossy(&id).into_owned();
        if let Some(a) = get_attempt(r, &id)? {
            out.push(a);
        }
    }
    Ok(out)
}

/// The latest attempt of every job of `jobs` that ran that very definition;
/// attempts of another definition are history.
pub fn current_attempts(
    r: &impl Rd,
    repo: &str,
    sha: &str,
    jobs: &[JobDef],
) -> AppResult<Vec<Attempt>> {
    let mut out: Vec<Attempt> = Vec::new();
    for a in attempts_of(r, repo, sha)? {
        if !jobs.contains(&a.job) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.job.name == a.job.name => *last = a,
            _ => out.push(a),
        }
    }
    Ok(out)
}

/// The jobs of `run`'s pipeline that have no current attempt, or whose
/// current attempt ended because the commit was replaced.
fn jobs_to_run(r: &impl Rd, run: &Run) -> AppResult<Vec<JobDef>> {
    let PipelineState::Jobs { jobs } = &run.pipeline else {
        return Ok(Vec::new());
    };
    let current = current_attempts(r, &run.repo, &run.sha, jobs)?;
    Ok(jobs
        .iter()
        .filter(|j| {
            current
                .iter()
                .find(|a| a.job.name == j.name)
                .is_none_or(|a| a.superseded)
        })
        .cloned()
        .collect())
}

fn next_number(r: &impl Rd, repo: &str, sha: &str, job: &str) -> AppResult<u64> {
    let prefix = Key::new().s(repo).s(sha).s(job).bytes();
    let last = r
        .prefix_raw(store::IX_CI_SHA, &prefix, None, usize::MAX)?
        .last()
        .and_then(|(k, _)| store::decode_key(k, "sssu"))
        .map(|p| p[3].u())
        .unwrap_or(0);
    Ok(last + 1)
}

/// Queued attempts, up to `limit`.
pub fn pending_count(r: &impl Rd, limit: usize) -> AppResult<usize> {
    Ok(r.prefix_raw(store::IX_CI_PENDING, &[], None, limit)?.len())
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_DETAILS {
        return text.to_string();
    }
    let mut end = MAX_DETAILS;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn new_attempt(w: &Wtx, run: &Run, job: &JobDef, network: bool, now: &str) -> AppResult<Attempt> {
    let number = next_number(w, &run.repo, &run.sha, &job.name)?;
    Ok(Attempt {
        id: util::new_id("att"),
        repo: run.repo.clone(),
        repo_path: run.repo_path.clone(),
        sha: run.sha.clone(),
        job: job.clone(),
        number,
        seq: w.next("ci_seq")?,
        status: "queued".into(),
        conclusion: None,
        details: None,
        lease_hash: None,
        lease_expires_at: None,
        cancel_requested: false,
        runner: None,
        cache_scope: run.cache_scope.clone(),
        network,
        created_at: now.to_string(),
        started_at: None,
        finished_at: None,
        after_interrupt: false,
        superseded: false,
        artifacts: Vec::new(),
        files_pruned: false,
    })
}

fn conclude(a: &mut Attempt, conclusion: &str, details: Option<String>, now: &str) {
    a.status = "completed".into();
    a.conclusion = Some(conclusion.to_string());
    a.details = details.map(|d| cut(&d));
    a.lease_hash = None;
    a.lease_expires_at = None;
    a.finished_at = Some(now.to_string());
}

/// What a registration asks for.
pub struct Registration {
    pub repo: String,
    pub repo_path: String,
    pub sha: String,
    pub branch: String,
    pub protected: bool,
    pub pipeline: PipelineState,
    pub network: bool,
}

/// Records that `branch` wants CI of `sha`. Creates the run (and queues its
/// jobs when they fit) or adds the branch to the existing run; a superseded
/// run that becomes a tip again gets fresh attempts. Returns true when
/// anything visible changed.
pub fn register(w: &Wtx, reg: Registration, queue_max: usize, now: &str) -> AppResult<bool> {
    let scope = if reg.protected {
        "protected".to_string()
    } else {
        format!("branch:{}", reg.branch)
    };
    if let Some(old) = get_run(w, &reg.repo, &reg.sha)? {
        let mut run = old.clone();
        if !run.branches.contains(&reg.branch) {
            run.branches.push(reg.branch.clone());
        }
        if reg.protected {
            run.cache_scope = scope;
        }
        let revived = run.superseded_by.take().is_some();
        // The registration read the commit's own pipeline; a run recorded with
        // another one (by schema version 7, from a pull request's base) takes
        // it. The comparison is of the parsed jobs, so a change to what the
        // parser yields for the same file runs the jobs again.
        let replaced = run.pipeline != reg.pipeline;
        run.pipeline = reg.pipeline;
        if revived || replaced {
            let todo = jobs_to_run(w, &run)?;
            admit_jobs(w, &mut run, &todo, reg.network, queue_max, now)?;
        }
        if run != old {
            put_run(w, Some(&old), &run)?;
            return Ok(true);
        }
        return Ok(false);
    }
    let mut run = Run {
        repo: reg.repo,
        repo_path: reg.repo_path,
        sha: reg.sha,
        seq: w.next("ci_seq")?,
        created_at: now.to_string(),
        branches: vec![reg.branch],
        pipeline: reg.pipeline,
        cache_scope: scope,
        waiting: false,
        superseded_by: None,
    };
    if let PipelineState::Jobs { jobs } = run.pipeline.clone() {
        admit_jobs(w, &mut run, &jobs, reg.network, queue_max, now)?;
    }
    put_run(w, None, &run)?;
    Ok(true)
}

/// Queues `jobs` of `run` when they all fit, else marks the run waiting.
fn admit_jobs(
    w: &Wtx,
    run: &mut Run,
    jobs: &[JobDef],
    network: bool,
    queue_max: usize,
    now: &str,
) -> AppResult<()> {
    if jobs.is_empty() {
        run.waiting = false;
        return Ok(());
    }
    if pending_count(w, queue_max + 1)? + jobs.len() > queue_max {
        run.waiting = true;
        return Ok(());
    }
    for job in jobs {
        let a = new_attempt(w, run, job, network, now)?;
        put_attempt(w, None, &a)?;
    }
    run.waiting = false;
    Ok(())
}

/// Admits waiting runs in registration order while they fit. Returns the
/// (repo path, sha) of every run admitted.
pub fn admit_waiting(w: &Wtx, queue_max: usize, now: &str) -> AppResult<Vec<(String, String)>> {
    let mut admitted = Vec::new();
    for (_, key) in w.prefix_raw(store::IX_CI_WAITING, &[], None, 64)? {
        let Some(old) = w.get::<Run>(store::CI_RUNS, &key)? else {
            continue;
        };
        let mut run = old.clone();
        let network = network_of(w, &run.repo)?;
        let jobs = jobs_to_run(w, &run)?;
        admit_jobs(w, &mut run, &jobs, network, queue_max, now)?;
        if run.waiting {
            break;
        }
        put_run(w, Some(&old), &run)?;
        admitted.push((run.repo_path.clone(), run.sha.clone()));
    }
    Ok(admitted)
}

fn network_of(r: &impl Rd, repo: &str) -> AppResult<bool> {
    Ok(
        r.get::<crate::model::Repo>(store::REPOS, &crate::model::Repo::key(repo))?
            .map(|x| x.ci_network)
            .unwrap_or(false),
    )
}

/// `branch` moved away from `sha` (to `by`, or was deleted). When no branch
/// wants the commit any more, its queued attempts are cancelled and its
/// running ones asked to stop: their results can only ever describe `sha`.
pub fn branch_left(
    w: &Wtx,
    repo: &str,
    sha: &str,
    branch: &str,
    by: &str,
    now: &str,
) -> AppResult<bool> {
    let Some(old) = get_run(w, repo, sha)? else {
        return Ok(false);
    };
    let mut run = old.clone();
    run.branches.retain(|b| b != branch);
    if run.branches.is_empty() && run.superseded_by.is_none() {
        run.superseded_by = Some(by.to_string());
        run.waiting = false;
        let reason = format!("Superseded: {branch} moved from {sha} to {by}.");
        for a in attempts_of(w, repo, sha)? {
            let mut x = a.clone();
            match a.status.as_str() {
                "queued" => {
                    x.superseded = true;
                    conclude(&mut x, "cancelled", Some(reason.clone()), now);
                }
                "in_progress" => {
                    x.superseded = true;
                    x.cancel_requested = true;
                    x.details = Some(reason.clone());
                }
                _ => continue,
            }
            put_attempt(w, Some(&a), &x)?;
        }
    }
    if run == old {
        return Ok(false);
    }
    put_run(w, Some(&old), &run)?;
    Ok(true)
}

/// What a runner receives with a claimed attempt.
#[derive(Serialize, Debug)]
pub struct Claimed {
    pub attempt: Attempt,
    pub lease: String,
}

/// Hands the oldest queued attempt to `runner` under a new lease.
pub fn claim(w: &Wtx, runner: &str, lease_secs: u64, now: &str) -> AppResult<Option<Claimed>> {
    let next = w.prefix_raw(store::IX_CI_PENDING, &[], None, 1)?;
    let Some((_, id)) = next.into_iter().next() else {
        return Ok(None);
    };
    let id = String::from_utf8_lossy(&id).into_owned();
    let old =
        get_attempt(w, &id)?.ok_or_else(|| AppError::internal("pending attempt is missing"))?;
    let lease = format!("bvl_{}", util::random_secret::<24>());
    let mut a = old.clone();
    a.status = "in_progress".into();
    a.lease_hash = Some(lease_hash(&lease));
    a.lease_expires_at = Some(later(now, lease_secs));
    a.runner = Some(runner.to_string());
    a.started_at = Some(now.to_string());
    put_attempt(w, Some(&old), &a)?;
    Ok(Some(Claimed { attempt: a, lease }))
}

fn lease_hash(lease: &str) -> String {
    util::hex(&util::sha256(lease.as_bytes()))
}

pub fn later(now: &str, secs: u64) -> String {
    let t = chrono::DateTime::parse_from_rfc3339(now)
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    util::format_time(t + chrono::Duration::seconds(secs as i64))
}

pub fn earlier(now: &str, secs: u64) -> String {
    let t = chrono::DateTime::parse_from_rfc3339(now)
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    util::format_time(t - chrono::Duration::seconds(secs as i64))
}

/// The attempt, when `lease` is its current, unexpired lease. Anything else
/// (expired, concluded, superseded by a re-queue, another runner's) is
/// `409 conflict`.
pub fn leased(r: &impl Rd, id: &str, lease: &str, now: &str) -> AppResult<Attempt> {
    let stale = || {
        AppError::conflict(format!(
            "The lease is not the current lease of attempt {id} (expired, cancelled, or superseded)."
        ))
    };
    let a = get_attempt(r, id)?.ok_or_else(stale)?;
    let current = a.lease_hash.as_deref().unwrap_or("");
    let sent = lease_hash(lease);
    let same = bool::from(subtle::ConstantTimeEq::ct_eq(
        current.as_bytes(),
        sent.as_bytes(),
    ));
    if a.status != "in_progress" || !same || a.lease_expires_at.as_deref().unwrap_or("") < now {
        return Err(stale());
    }
    Ok(a)
}

/// Renews the lease; tells the runner whether to stop.
pub fn heartbeat(w: &Wtx, id: &str, lease: &str, lease_secs: u64, now: &str) -> AppResult<Attempt> {
    let old = leased(w, id, lease, now)?;
    let mut a = old.clone();
    a.lease_expires_at = Some(later(now, lease_secs));
    put_attempt(w, Some(&old), &a)?;
    Ok(a)
}

/// Concludes a leased attempt with the runner's result. A cancellation that
/// was requested wins over the reported conclusion.
pub fn finish(
    w: &Wtx,
    id: &str,
    lease: &str,
    conclusion: &str,
    details: Option<String>,
    now: &str,
) -> AppResult<Attempt> {
    if !CONCLUSIONS.contains(&conclusion) {
        return Err(AppError::invalid(format!(
            "conclusion must be one of {}.",
            CONCLUSIONS.join(", ")
        )));
    }
    let old = leased(w, id, lease, now)?;
    let mut a = old.clone();
    if a.cancel_requested {
        let why = a.details.clone().unwrap_or_else(|| "Cancelled.".into());
        let reported = details.map(|d| format!(" (the runner reported {conclusion}: {d})"));
        conclude(
            &mut a,
            "cancelled",
            Some(format!("{why}{}", reported.unwrap_or_default())),
            now,
        );
    } else {
        conclude(&mut a, conclusion, details, now);
    }
    put_attempt(w, Some(&old), &a)?;
    Ok(a)
}

/// Records an uploaded artifact (the file is already in place).
pub fn add_artifact(
    w: &Wtx,
    id: &str,
    lease: &str,
    meta: ArtifactMeta,
    now: &str,
) -> AppResult<()> {
    let old = leased(w, id, lease, now)?;
    let mut a = old.clone();
    a.artifacts.retain(|x| x.name != meta.name);
    a.artifacts.push(meta);
    put_attempt(w, Some(&old), &a)
}

/// Cancels a queued attempt now, or asks the runner of a running one to stop.
/// A completed attempt is returned unchanged.
pub fn cancel(w: &Wtx, id: &str, who: &str, now: &str) -> AppResult<Attempt> {
    let old =
        get_attempt(w, id)?.ok_or_else(|| AppError::not_found(format!("No CI attempt {id}.")))?;
    let mut a = old.clone();
    let reason = format!("Cancelled by {who}.");
    match old.status.as_str() {
        "queued" => conclude(&mut a, "cancelled", Some(reason), now),
        "in_progress" if !old.cancel_requested => {
            a.cancel_requested = true;
            a.details = Some(reason);
        }
        _ => return Ok(old),
    }
    put_attempt(w, Some(&old), &a)?;
    Ok(a)
}

/// Queues a new attempt of the same job (with the same recorded definition)
/// after a completed latest attempt.
pub fn retry(w: &Wtx, id: &str, queue_max: usize, now: &str) -> AppResult<Attempt> {
    let old =
        get_attempt(w, id)?.ok_or_else(|| AppError::not_found(format!("No CI attempt {id}.")))?;
    if old.status != "completed" {
        return Err(AppError::conflict(format!(
            "Attempt {id} is {}; only a completed attempt can be retried.",
            old.status
        )));
    }
    if next_number(w, &old.repo, &old.sha, &old.job.name)? != old.number + 1 {
        return Err(AppError::conflict(format!(
            "Attempt {id} is not the latest attempt of job {}.",
            old.job.name
        )));
    }
    if pending_count(w, queue_max + 1)? >= queue_max {
        return Err(AppError::unavailable(format!(
            "The CI queue is full ({queue_max} queued attempts); retry later."
        )));
    }
    let run = get_run(w, &old.repo, &old.sha)?
        .ok_or_else(|| AppError::internal("attempt without a run"))?;
    let network = network_of(w, &old.repo)?;
    let a = new_attempt(w, &run, &old.job, network, now)?;
    put_attempt(w, None, &a)?;
    Ok(a)
}

/// Handles every in-progress attempt whose lease expired before `now` (the
/// runner died, lost the service, or the service was down): the attempt is
/// concluded `cancelled` as interrupted, and the first interruption of a job
/// that was not cancelled or superseded is re-queued as a new attempt.
pub fn expire(w: &Wtx, now: &str) -> AppResult<Vec<(String, String)>> {
    let mut changed = Vec::new();
    for (k, _) in w.prefix_raw(store::IX_CI_ACTIVE, &[], None, usize::MAX)? {
        let Some(parts) = store::decode_key(&k, "s") else {
            continue;
        };
        let Some(old) = get_attempt(w, parts[0].s())? else {
            continue;
        };
        if old.lease_expires_at.as_deref().unwrap_or("") >= now {
            continue;
        }
        let mut a = old.clone();
        let requeue = !old.cancel_requested && !old.superseded && !old.after_interrupt;
        let details = if old.cancel_requested {
            format!(
                "{} The runner stopped renewing its lease before it finished.",
                old.details.clone().unwrap_or_else(|| "Cancelled.".into())
            )
        } else if requeue {
            "Interrupted: the runner stopped renewing its lease (runner lost or Branvil restarted); queued again as a new attempt.".to_string()
        } else {
            "Interrupted twice: the runner stopped renewing its lease again; retry the job."
                .to_string()
        };
        conclude(&mut a, "cancelled", Some(details), now);
        put_attempt(w, Some(&old), &a)?;
        if requeue && let Some(run) = get_run(w, &old.repo, &old.sha)? {
            let mut next = new_attempt(w, &run, &old.job, old.network, now)?;
            next.after_interrupt = true;
            put_attempt(w, None, &next)?;
        }
        changed.push((old.repo_path.clone(), old.sha.clone()));
    }
    Ok(changed)
}

/// Concludes `startup_failure` for every queued attempt created before
/// `before` (no runner took it in time).
pub fn fail_queued(
    w: &Wtx,
    before: &str,
    details: &str,
    now: &str,
) -> AppResult<Vec<(String, String)>> {
    let mut changed = Vec::new();
    for (_, id) in w.prefix_raw(store::IX_CI_PENDING, &[], None, usize::MAX)? {
        let id = String::from_utf8_lossy(&id).into_owned();
        let Some(old) = get_attempt(w, &id)? else {
            continue;
        };
        if old.created_at.as_str() >= before {
            continue;
        }
        let mut a = old.clone();
        conclude(&mut a, "startup_failure", Some(details.to_string()), now);
        put_attempt(w, Some(&old), &a)?;
        changed.push((old.repo_path.clone(), old.sha.clone()));
    }
    Ok(changed)
}

/// Completed attempts finished before `cutoff` whose files still exist.
pub fn prunable(r: &impl Rd, cutoff: &str, limit: usize) -> AppResult<Vec<Attempt>> {
    let mut out = Vec::new();
    for a in r.prefix_all::<Attempt>(store::CI_ATTEMPTS, &[])? {
        if a.status == "completed"
            && !a.files_pruned
            && a.finished_at.as_deref().is_some_and(|f| f < cutoff)
        {
            out.push(a);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

pub fn mark_pruned(w: &Wtx, id: &str) -> AppResult<()> {
    if let Some(old) = get_attempt(w, id)? {
        let mut a = old.clone();
        a.files_pruned = true;
        put_attempt(w, Some(&old), &a)?;
    }
    Ok(())
}

/// Counts for diagnostics: (queued, in progress, waiting runs).
pub fn counts(r: &impl Rd) -> AppResult<(usize, usize, usize)> {
    Ok((
        r.prefix_raw(store::IX_CI_PENDING, &[], None, usize::MAX)?
            .len(),
        r.prefix_raw(store::IX_CI_ACTIVE, &[], None, usize::MAX)?
            .len(),
        r.prefix_raw(store::IX_CI_WAITING, &[], None, usize::MAX)?
            .len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn job(name: &str) -> JobDef {
        JobDef {
            name: name.into(),
            image: "alpine".into(),
            commands: vec!["true".into()],
            timeout_secs: 60,
            cpus: 1,
            memory_mib: 512,
            artifacts: vec![],
            cache: None,
        }
    }

    fn reg(sha: &str, branch: &str, jobs: &[&str]) -> Registration {
        Registration {
            repo: "repo_1".into(),
            repo_path: "acme/demo".into(),
            sha: sha.into(),
            branch: branch.into(),
            protected: branch == "main",
            pipeline: PipelineState::Jobs {
                jobs: jobs.iter().map(|j| job(j)).collect(),
            },
            network: false,
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.redb"), 1 << 20, 1).unwrap();
        (dir, s)
    }

    const T0: &str = "2026-10-14T10:00:00.000Z";
    const T1: &str = "2026-10-14T10:00:30.000Z";
    const T2: &str = "2026-10-14T10:02:00.000Z";
    const T3: &str = "2026-10-14T10:05:00.000Z";

    fn latest_attempts(r: &impl Rd, repo: &str, sha: &str) -> AppResult<Vec<Attempt>> {
        let jobs: Vec<JobDef> = attempts_of(r, repo, sha)?
            .into_iter()
            .map(|a| a.job)
            .collect();
        current_attempts(r, repo, sha, &jobs)
    }

    fn latest(s: &Store, sha: &str) -> Vec<Attempt> {
        s.read_sync(|r| latest_attempts(r, "repo_1", sha)).unwrap()
    }

    #[test]
    fn a_claimed_attempt_finishes_only_with_its_current_lease() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "feature", &["test"]), 10, T0))
            .unwrap();
        let c = s
            .write_sync(|w| claim(w, "runner1", 60, T0))
            .unwrap()
            .unwrap();
        assert_eq!(c.attempt.status, "in_progress");
        assert!(
            s.write_sync(|w| claim(w, "runner1", 60, T0))
                .unwrap()
                .is_none()
        );
        let wrong = s.write_sync(|w| finish(w, &c.attempt.id, "bvl_other", "success", None, T1));
        assert_eq!(wrong.unwrap_err().code, "conflict");
        let done = s
            .write_sync(|w| {
                finish(
                    w,
                    &c.attempt.id,
                    &c.lease,
                    "failure",
                    Some("exit 1".into()),
                    T1,
                )
            })
            .unwrap();
        assert_eq!(done.conclusion.as_deref(), Some("failure"));
        // The lease is gone with the conclusion: a late repeat changes nothing.
        let again = s.write_sync(|w| finish(w, &c.attempt.id, &c.lease, "success", None, T1));
        assert_eq!(again.unwrap_err().code, "conflict");
        assert_eq!(latest(&s, "s1")[0].conclusion.as_deref(), Some("failure"));
        assert!(
            s.read_sync(crate::model::index_problems)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_expired_lease_is_interrupted_and_requeued_once() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "feature", &["test"]), 10, T0))
            .unwrap();
        let c1 = s.write_sync(|w| claim(w, "r", 60, T0)).unwrap().unwrap();
        // Heartbeat after expiry is refused; the sweeper interrupts it.
        assert!(
            s.write_sync(|w| heartbeat(w, &c1.attempt.id, &c1.lease, 60, T2))
                .is_err()
        );
        assert_eq!(s.write_sync(|w| expire(w, T2)).unwrap().len(), 1);
        let l = latest(&s, "s1");
        assert_eq!((l[0].number, l[0].status.as_str()), (2, "queued"));
        assert!(l[0].after_interrupt);
        // The first runner's late result is refused.
        let late = s.write_sync(|w| finish(w, &c1.attempt.id, &c1.lease, "success", None, T2));
        assert_eq!(late.unwrap_err().code, "conflict");
        // The second interruption is final.
        let c2 = s.write_sync(|w| claim(w, "r", 60, T2)).unwrap().unwrap();
        assert_eq!(c2.attempt.number, 2);
        s.write_sync(|w| expire(w, T3)).unwrap();
        let l = latest(&s, "s1");
        assert_eq!(l[0].number, 2);
        assert_eq!(l[0].conclusion.as_deref(), Some("cancelled"));
        assert!(l[0].details.as_deref().unwrap().contains("twice"));
        assert!(
            s.read_sync(crate::model::index_problems)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_new_tip_supersedes_the_old_commit() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "feature", &["build", "test"]), 10, T0))
            .unwrap();
        let running = s.write_sync(|w| claim(w, "r", 60, T0)).unwrap().unwrap();
        s.write_sync(|w| branch_left(w, "repo_1", "s1", "feature", "s2", T1))
            .unwrap();
        s.write_sync(|w| register(w, reg("s2", "feature", &["build", "test"]), 10, T1))
            .unwrap();
        let old = latest(&s, "s1");
        let build = old.iter().find(|a| a.job.name == "build").unwrap();
        let test = old.iter().find(|a| a.job.name == "test").unwrap();
        assert!(build.cancel_requested, "the running job is asked to stop");
        assert_eq!(
            test.conclusion.as_deref(),
            Some("cancelled"),
            "the queued one is dropped"
        );
        // The cancelled runner's success never counts, even for its own commit.
        let fin = s
            .write_sync(|w| finish(w, &running.attempt.id, &running.lease, "success", None, T1))
            .unwrap();
        assert_eq!(fin.conclusion.as_deref(), Some("cancelled"));
        // Only the new commit's attempts are queued.
        let next = s.write_sync(|w| claim(w, "r", 60, T1)).unwrap().unwrap();
        assert_eq!(next.attempt.sha, "s2");
        assert!(
            s.read_sync(crate::model::index_problems)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_commit_still_wanted_by_another_branch_is_not_superseded() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "a", &["test"]), 10, T0))
            .unwrap();
        s.write_sync(|w| register(w, reg("s1", "b", &["test"]), 10, T0))
            .unwrap();
        s.write_sync(|w| branch_left(w, "repo_1", "s1", "a", "s2", T1))
            .unwrap();
        assert_eq!(latest(&s, "s1")[0].status, "queued");
        assert_eq!(latest(&s, "s1").len(), 1, "one attempt for both branches");
    }

    #[test]
    fn a_full_queue_makes_runs_wait_and_admits_them_in_order() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "a", &["x", "y"]), 2, T0))
            .unwrap();
        s.write_sync(|w| register(w, reg("s2", "b", &["x"]), 2, T0))
            .unwrap();
        let run2 = s
            .read_sync(|r| get_run(r, "repo_1", "s2"))
            .unwrap()
            .unwrap();
        assert!(run2.waiting);
        assert!(latest(&s, "s2").is_empty());
        let cancelled = s.write_sync(|w| {
            let a = latest_attempts(w, "repo_1", "s1")?;
            cancel(w, &a[0].id, "ken", T0)?;
            Ok(a[0].id.clone())
        });
        let id = cancelled.unwrap();
        // One slot free: s2 fits now.
        assert_eq!(s.write_sync(|w| admit_waiting(w, 2, T1)).unwrap().len(), 1);
        assert_eq!(latest(&s, "s2")[0].status, "queued");
        // The queue is full again: a retry is refused explicitly.
        let full = s.write_sync(|w| retry(w, &id, 2, T1)).unwrap_err();
        assert_eq!(full.code, "unavailable");
        assert!(
            s.read_sync(crate::model::index_problems)
                .unwrap()
                .is_empty()
        );
    }

    fn with(sha: &str, jobs: Vec<JobDef>) -> Registration {
        let mut r = reg(sha, "feature", &[]);
        r.pipeline = PipelineState::Jobs { jobs };
        r
    }

    fn changed(name: &str) -> JobDef {
        let mut j = job(name);
        j.commands = vec!["echo changed".into()];
        j
    }

    #[test]
    fn a_taken_pipeline_counts_only_attempts_of_its_definitions() {
        let (_d, s) = store();
        // The old definition of test runs when the run takes the new one.
        s.write_sync(|w| register(w, reg("s1", "feature", &["test"]), 10, T0))
            .unwrap();
        s.write_sync(|w| claim(w, "r", 60, T0)).unwrap().unwrap();
        s.write_sync(|w| register(w, with("s1", vec![changed("test")]), 10, T0))
            .unwrap();
        // Interrupted, the old attempt is queued again with its definition;
        // it is the newest, and still not the current attempt.
        s.write_sync(|w| expire(w, T2)).unwrap();
        let all = s.read_sync(|r| attempts_of(r, "repo_1", "s1")).unwrap();
        let shape: Vec<(u64, bool, &str)> = all
            .iter()
            .map(|a| (a.number, a.job == changed("test"), a.status.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (1, false, "completed"),
                (2, true, "queued"),
                (3, false, "queued")
            ]
        );
        let current = s
            .read_sync(|r| current_attempts(r, "repo_1", "s1", &[changed("test")]))
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].number, 2);
        // Leaving the commit ends every unfinished attempt, not only the newest.
        s.write_sync(|w| branch_left(w, "repo_1", "s1", "feature", "s2", T2))
            .unwrap();
        let all = s.read_sync(|r| attempts_of(r, "repo_1", "s1")).unwrap();
        assert!(all.iter().all(|a| a.status == "completed"), "{all:?}");
    }

    #[test]
    fn an_admitted_run_runs_only_jobs_without_a_current_attempt() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "feature", &["a", "b"]), 10, T0))
            .unwrap();
        for _ in 0..2 {
            let c = s.write_sync(|w| claim(w, "r", 60, T0)).unwrap().unwrap();
            s.write_sync(|w| finish(w, &c.attempt.id, &c.lease, "success", None, T0))
                .unwrap();
        }
        // The queue is full when the run takes a pipeline with a new b.
        s.write_sync(|w| register(w, reg("s2", "other", &["x"]), 1, T0))
            .unwrap();
        s.write_sync(|w| register(w, with("s1", vec![job("a"), changed("b")]), 1, T0))
            .unwrap();
        let run = s
            .read_sync(|r| get_run(r, "repo_1", "s1"))
            .unwrap()
            .unwrap();
        assert!(run.waiting);
        s.write_sync(|w| {
            let x = attempts_of(w, "repo_1", "s2")?;
            cancel(w, &x[0].id, "ken", T1)
        })
        .unwrap();
        assert_eq!(s.write_sync(|w| admit_waiting(w, 1, T1)).unwrap().len(), 1);
        let all = s.read_sync(|r| attempts_of(r, "repo_1", "s1")).unwrap();
        let shape: Vec<(&str, u64, &str)> = all
            .iter()
            .map(|a| (a.job.name.as_str(), a.number, a.status.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("a", 1, "completed"),
                ("b", 1, "completed"),
                ("b", 2, "queued")
            ]
        );
    }

    #[test]
    fn queued_attempts_fail_to_start_without_a_runner() {
        let (_d, s) = store();
        s.write_sync(|w| register(w, reg("s1", "a", &["x"]), 10, T0))
            .unwrap();
        assert!(
            s.write_sync(|w| fail_queued(w, T0, "no runner", T1))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            s.write_sync(|w| fail_queued(w, T1, "no runner", T1))
                .unwrap()
                .len(),
            1
        );
        let l = latest(&s, "s1");
        assert_eq!(l[0].conclusion.as_deref(), Some("startup_failure"));
        assert_eq!(l[0].details.as_deref(), Some("no runner"));
        // Retry makes attempt 2.
        let a2 = s.write_sync(|w| retry(w, &l[0].id, 10, T1)).unwrap();
        assert_eq!(a2.number, 2);
        assert_eq!(latest(&s, "s1")[0].id, a2.id);
    }
}
