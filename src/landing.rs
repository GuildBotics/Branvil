//! Persistent, serial landing queues, one for each repository and base branch.

use axum::{
    Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;

use crate::app::{App, Shared};
use crate::auth::Caller;
use crate::ci::Check;
use crate::domain::{self, pulls};
use crate::error::AppResult;
use crate::git::{GateCommit, ZERO_SHA};
use crate::model::{Landing, LandingReason, Pull, Repo};
use crate::store::{self, Key, Rd, Wtx};
use crate::util;

pub fn current(r: &impl Rd, repo: &str, number: u64) -> AppResult<Option<Landing>> {
    r.get(store::LANDING, &Pull::key(repo, number))
}

fn all(r: &impl Rd, repo: &str) -> AppResult<Vec<Landing>> {
    let mut entries: Vec<Landing> = r.prefix_all(store::LANDING, &Key::new().s(repo).bytes())?;
    entries.sort_by_key(|e| (e.state == "left", e.sequence));
    Ok(entries)
}

pub fn active(r: &impl Rd, repo: &str) -> AppResult<Vec<Landing>> {
    Ok(all(r, repo)?
        .into_iter()
        .filter(|e| e.state != "left")
        .collect())
}

#[derive(Serialize, Clone, Debug)]
pub struct LandingFirst {
    pub number: u64,
    pub state: String,
    pub state_at: String,
    pub error: Option<String>,
    pub published: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct LandingView {
    #[serde(flatten)]
    pub entry: Landing,
    pub position: Option<u64>,
    pub first: Option<LandingFirst>,
}

fn views(r: &impl Rd, repo: &Repo) -> AppResult<Vec<LandingView>> {
    let entries = all(r, &repo.id)?;
    entries
        .iter()
        .map(|entry| {
            let queue: Vec<_> = entries
                .iter()
                .filter(|e| e.base == entry.base && e.state != "left")
                .collect();
            let position = queue
                .iter()
                .position(|e| e.number == entry.number)
                .map(|i| i as u64 + 1);
            let first = queue.first().map(|e| LandingFirst {
                number: e.number,
                state: e.state.clone(),
                state_at: e.state_at.clone(),
                published: e.published,
                error: e.error.clone(),
            });
            Ok(LandingView {
                entry: entry.clone(),
                position,
                first,
            })
        })
        .collect()
}

pub async fn view(app: &App, repo: &Repo, number: u64) -> AppResult<Option<LandingView>> {
    let repo = repo.clone();
    app.store
        .read(move |r| {
            Ok(views(r, &repo)?
                .into_iter()
                .find(|e| e.entry.number == number))
        })
        .await
}

async fn queue_get(
    State(app): State<Shared>,
    c: Caller,
    Path(path): Path<String>,
) -> AppResult<axum::Json<serde_json::Value>> {
    let repo = domain::repos::visible(&app, &c, &path).await?;
    let items = app.store.read(move |r| views(r, &repo)).await?;
    Ok(axum::Json(serde_json::json!({"items":items})))
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/repos/{repo}/landing", get(queue_get))
}

pub fn mark_notified(w: &Wtx, expected: &Landing, at: &str) -> AppResult<()> {
    if let Some(mut e) = current(w, &expected.repo, expected.number)?
        && e.state == "passed"
        && e.gate == expected.gate
        && e.state_at == expected.state_at
    {
        e.notified_at = Some(at.to_string());
        w.put(store::LANDING, &Pull::key(&e.repo, e.number), &e)?;
    }
    Ok(())
}

fn reason(code: &str, message: impl Into<String>) -> LandingReason {
    LandingReason {
        code: code.into(),
        message: message.into(),
        jobs: Vec::new(),
        files: Vec::new(),
    }
}

async fn eligibility(
    app: &App,
    repo: &Repo,
    p: &Pull,
    head: &str,
    base: &str,
) -> AppResult<Option<LandingReason>> {
    if p.state != "open" {
        return Ok(Some(reason("closed", "The pull request is closed.")));
    }
    if p.draft {
        return Ok(Some(reason("draft", "The pull request is a draft.")));
    }
    if head.is_empty()
        || base.is_empty()
        || app
            .git
            .is_ancestor(&app.git.repo_dir(&repo.path), head, base)
            .await?
    {
        return Ok(Some(reason(
            "ineligible",
            "The head adds nothing to the base.",
        )));
    }
    if let Some(up) = &repo.upstream {
        let author = p.author.clone();
        let provider = up.provider.clone();
        let account = app
            .store
            .read(move |r| {
                Ok(domain::member_by_name(r, &author)?
                    .accounts
                    .iter()
                    .any(|a| a.provider == provider))
            })
            .await?;
        if p.base != repo.default_branch || !account {
            return Ok(Some(reason(
                "ineligible",
                "Publishing requires the default base and an author account on the host.",
            )));
        }
    }
    let checks = app.ci.head_checks(repo, head).await?;
    if !repo.required_checks.iter().all(|name| {
        checks
            .checks
            .iter()
            .any(|c| c.name == *name && c.succeeded())
    }) {
        return Ok(Some(reason(
            "ineligible",
            "Required head checks have not succeeded.",
        )));
    }
    if pulls::approvals(&pulls::read_review_states(app, p, head).await?) < repo.required_approvals {
        return Ok(Some(reason("ineligible", "The head needs more approvals.")));
    }
    Ok(None)
}

fn publication(repo: &Repo, p: &Pull) -> (bool, Option<String>) {
    let Some(x) = p.upstream.as_ref().filter(|x| {
        repo.upstream
            .as_ref()
            .is_some_and(|up| up.same_place(&x.provider, &x.repository))
    }) else {
        return (false, None);
    };
    (x.number.is_some() && x.stopped.is_none(), x.error.clone())
}

fn published_problem(p: &Pull, head: &str, resume: bool) -> Option<LandingReason> {
    let x = p.upstream.as_ref()?;
    if !resume && let Some(stopped) = &x.stopped {
        return Some(reason("publish_stopped", stopped.clone()));
    }
    if x.pushed.as_deref() == Some(head) {
        let failed: Vec<_> = x
            .checks
            .iter()
            .filter(|c| {
                c.status == "completed"
                    && c.conclusion
                        .as_deref()
                        .is_some_and(|v| !["success", "neutral", "skipped"].contains(&v))
            })
            .map(|c| c.name.clone())
            .collect();
        if !failed.is_empty() {
            let mut why = reason("host_failed", "The published head failed host checks.");
            why.jobs = failed;
            return Some(why);
        }
    }
    None
}

async fn save(app: &App, repo: &Repo, e: Landing, comment: Option<String>) -> AppResult<()> {
    let id = repo.id.clone();
    let r2 = repo.clone();
    let number = e.number;
    app.store
        .write(move |w| {
            w.put(store::LANDING, &Pull::key(&id, number), &e)?;
            if let Some(body) = comment {
                pulls::system_comment(w, &r2, number, body)?;
            }
            Ok(())
        })
        .await?;
    app.events.publish("pull", &repo.path, number.to_string());
    Ok(())
}

pub async fn remove_merged_ref(app: &App, repo: &Repo, number: u64) -> AppResult<()> {
    app.git
        .run_ok(
            Some(&app.git.repo_dir(&repo.path)),
            &[
                "update-ref",
                "-d",
                &format!("refs/branvil/failed-gate/{number}"),
            ],
        )
        .await?;
    clear_ref(app, repo, number).await
}

pub fn mark_merged(w: &Wtx, repo: &str, number: u64) -> AppResult<()> {
    if let Some(mut e) = current(w, repo, number)? {
        e.state = "left".into();
        e.state_at = util::now();
        e.reason = Some(reason("merged", "The pull request was merged."));
        w.put(store::LANDING, &Pull::key(repo, number), &e)?;
    }
    Ok(())
}

async fn clear_ref(app: &App, repo: &Repo, number: u64) -> AppResult<()> {
    app.git
        .run_ok(
            Some(&app.git.repo_dir(&repo.path)),
            &["update-ref", "-d", &format!("refs/branvil/gate/{number}")],
        )
        .await?;
    crate::refs::record_refs_locked(app, repo).await?;
    Ok(())
}

async fn leave(
    app: &App,
    repo: &Repo,
    mut e: Landing,
    why: LandingReason,
    checks: &[Check],
) -> AppResult<()> {
    let comment = if ["gate_failed", "conflict"].contains(&why.code.as_str()) {
        let details = if why.code == "conflict" {
            why.files.join("\n")
        } else {
            checks
                .iter()
                .filter(|c| why.jobs.contains(&c.name))
                .map(|c| {
                    format!(
                        "- {}: {}",
                        c.name,
                        c.details_url.as_deref().unwrap_or("no log available")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Some(format!(
            "Landing gate failed.\n\nBase: `{}`\nHead: `{}`\nGate: `{}`\n\n{}\n{}{}",
            e.base_sha,
            e.head_sha,
            e.gate.as_deref().unwrap_or("not created"),
            why.message,
            details,
            if why.code == "gate_failed" {
                "\n\nRetry the failed gate jobs. Once the required jobs succeed, this pull request rejoins the end of the landing queue."
            } else {
                ""
            }
        ))
    } else {
        None
    };
    if why.code == "gate_failed"
        && let Some(gate) = &e.gate
    {
        app.git
            .run_ok(
                Some(&app.git.repo_dir(&repo.path)),
                &[
                    "update-ref",
                    &format!("refs/branvil/failed-gate/{}", e.number),
                    gate,
                ],
            )
            .await?;
    }
    e.state = "left".into();
    e.state_at = util::now();
    e.reason = Some(why);
    save(app, repo, e.clone(), comment).await?;
    clear_ref(app, repo, e.number).await
}

/// Owner intent to replace a stopped publication is distinct from successfully
/// publishing it: the old upstream state remains until its gate passes.
pub async fn resume_publish(
    app: &App,
    repo: &Repo,
    number: u64,
    input: &pulls::MergeInput,
) -> AppResult<()> {
    let lock = app.repo_lock(&repo.id);
    let _guard = lock.lock().await;
    let id = repo.id.clone();
    let r2 = repo.clone();
    let (p, mut e) = app
        .store
        .read(move |r| Ok((pulls::load_pull(r, &r2, number)?, current(r, &id, number)?)))
        .await?;
    let tips = pulls::tips(app, repo).await?;
    let (head, base) = (
        tips.get(&p.head).cloned().unwrap_or_default(),
        tips.get(&p.base).cloned().unwrap_or_default(),
    );
    if head != input.expected_head {
        return Err(crate::error::AppError::conflict_code(
            "head_moved",
            "The pull request head moved.",
        ));
    }
    if base != input.expected_base {
        return Err(crate::error::AppError::conflict_code(
            "base_moved",
            "The pull request base moved.",
        ));
    }
    if p.upstream.as_ref().is_none_or(|x| x.stopped.is_none()) {
        return Err(crate::error::AppError::conflict_code(
            "not_mergeable",
            "The publication is not stopped.",
        ));
    }
    if let Some(why) = eligibility(app, repo, &p, &head, &base).await? {
        return Err(crate::error::AppError::conflict_code(
            "not_mergeable",
            why.message,
        ));
    }
    let mut entry = match e.take() {
        Some(entry) => entry,
        None => {
            let id = repo.id.clone();
            let sequence = app
                .store
                .write(move |w| w.next(&format!("landing:{id}")))
                .await?;
            let now = util::now();
            Landing {
                repo: repo.id.clone(),
                number,
                base: p.base.clone(),
                queued_at: now.clone(),
                sequence,
                state: "waiting".into(),
                state_at: now,
                gate: None,
                base_sha: base,
                head_sha: head,
                reason: None,
                notified_at: None,
                notify: false,
                resume_publish: false,
                published: false,
                error: None,
            }
        }
    };
    entry.resume_publish = true;
    save(app, repo, entry, None).await?;
    reconcile_locked(app, repo).await
}

pub fn clear_resume_publish(w: &Wtx, repo: &str, number: u64) -> AppResult<()> {
    if let Some(mut e) = current(w, repo, number)?
        && e.resume_publish
    {
        e.resume_publish = false;
        w.put(store::LANDING, &Pull::key(repo, number), &e)?;
    }
    Ok(())
}

/// The upstream has just confirmed this pull request is still open. A base
/// containing its entire tree therefore came from another integration; it must
/// no longer block other pulls while waiting for a merge that did not happen.
pub async fn host_observed_open(app: &App, repo: &Repo, number: u64) -> AppResult<()> {
    let lock = app.repo_lock(&repo.id);
    let _guard = lock.lock().await;
    let id = repo.id.clone();
    let Some(mut e) = app.store.read(move |r| current(r, &id, number)).await? else {
        return Ok(());
    };
    if e.state != "passed" {
        return Ok(());
    }
    let tips = pulls::tips(app, repo).await?;
    let Some(base) = tips.get(&e.base) else {
        return Ok(());
    };
    if base == &e.base_sha {
        return Ok(());
    }
    let dir = app.git.repo_dir(&repo.path);
    if let Ok(tree) = app.git.merge_tree(&dir, base, &e.head_sha).await?
        && app.git.tree_oid(&dir, base).await?.as_ref() == Some(&tree)
    {
        e.base_sha = base.clone();
        leave(
            app,
            repo,
            e,
            reason(
                "already_in_base",
                "The base already contains these changes and the host pull request is still open.",
            ),
            &[],
        )
        .await?;
    }
    Ok(())
}

/// Reconciles every queue, including repositories whose automatic landing is off.
pub async fn sweep_repo(app: &App, repo: &Repo) -> AppResult<()> {
    let lock = app.repo_lock(&repo.id);
    let _guard = lock.lock().await;
    reconcile_locked(app, repo).await
}

/// Caller holds the repository lock. Persist gate ownership before updating its
/// ref, so CI recovery can safely recover either side of an interrupted write.
pub async fn reconcile_locked(app: &App, repo: &Repo) -> AppResult<()> {
    let id = repo.id.clone();
    let current_repo = app.store.read(move |r| domain::repo_by_id(r, &id)).await?;
    let repo = &current_repo;
    let tips = pulls::tips(app, repo).await?;
    let id = repo.id.clone();
    let pulls = app
        .store
        .read(move |r| r.prefix_all::<Pull>(store::PULLS, &Key::new().s(&id).bytes()))
        .await?;
    for p in &pulls {
        let (head, base) = (
            tips.get(&p.head).cloned().unwrap_or_default(),
            tips.get(&p.base).cloned().unwrap_or_default(),
        );
        let id = repo.id.clone();
        let number = p.number;
        let mut old = app.store.read(move |r| current(r, &id, number)).await?;
        if let Some(e) = old.as_mut().filter(|e| e.state == "passed") {
            let (published, error) = publication(repo, p);
            if e.published != published || e.error != error {
                e.published = published;
                e.error = error;
                e.state_at = util::now();
                save(app, repo, e.clone(), None).await?;
            }
        }
        // A host merge may already have advanced the base before the poller
        // records mark_merged. Keep the passed gate until that record arrives.
        if let Some(e) = &old
            && p.state == "open"
            && !p.draft
            && !p.merged
            && repo.upstream.as_ref().is_some_and(|up| {
                p.upstream.as_ref().is_some_and(|published| {
                    up.same_place(&published.provider, &published.repository)
                })
            })
            && e.state == "passed"
            && e.head_sha == head
            && e.base_sha != base
            && !base.is_empty()
            && let Ok(tree) = app
                .git
                .merge_tree(&app.git.repo_dir(&repo.path), &base, &head)
                .await?
            && tree
                == app
                    .git
                    .tree_oid(&app.git.repo_dir(&repo.path), &base)
                    .await?
                    .unwrap_or_default()
        {
            continue;
        }
        let ineligible = eligibility(app, repo, p, &head, &base).await?;
        let changed = old
            .as_ref()
            .is_some_and(|e| e.head_sha != head || e.base_sha != base || e.base != p.base);
        let problem = published_problem(p, &head, old.as_ref().is_some_and(|e| e.resume_publish))
            .or(ineligible);
        if let Some(e) = old.as_ref().filter(|e| e.state != "left") {
            if changed || problem.is_some() {
                leave(
                    app,
                    repo,
                    e.clone(),
                    problem.clone().unwrap_or_else(|| {
                        reason(
                            if e.head_sha != head {
                                "head_moved"
                            } else {
                                "base_moved"
                            },
                            "The head or base moved.",
                        )
                    }),
                    &[],
                )
                .await?;
            } else {
                continue;
            }
        }
        if problem.is_some() {
            continue;
        }
        if old.as_ref().is_some_and(|e| {
            e.state == "left"
                && !changed
                && e.reason
                    .as_ref()
                    .is_some_and(|r| r.code == "already_in_base")
        }) {
            continue;
        }
        if let Some(e) = &old
            && e.state == "left"
            && !changed
            && let Some(why) = e.reason.as_ref().filter(|r| r.code == "host_failed")
        {
            let succeeded = p
                .upstream
                .as_ref()
                .filter(|x| x.pushed.as_deref() == Some(head.as_str()))
                .is_some_and(|x| {
                    why.jobs.iter().all(|name| {
                        x.checks.iter().any(|c| {
                            &c.name == name
                                && c.status == "completed"
                                && c.conclusion
                                    .as_deref()
                                    .is_some_and(|v| ["success", "neutral", "skipped"].contains(&v))
                        })
                    })
                });
            if !succeeded {
                continue;
            }
        }
        if let Some(e) = &old
            && e.state == "left"
            && !changed
            && e.reason
                .as_ref()
                .is_some_and(|r| ["gate_failed", "conflict"].contains(&r.code.as_str()))
        {
            if e.reason.as_ref().is_some_and(|r| r.code == "conflict") {
                continue;
            }
            let Some(gate) = &e.gate else { continue };
            let checks = app.ci.head_checks(repo, gate).await?;
            if !repo
                .required_checks
                .iter()
                .all(|n| checks.checks.iter().any(|c| c.name == *n && c.succeeded()))
            {
                continue;
            }
        }
        let now = util::now();
        let id = repo.id.clone();
        let sequence = app
            .store
            .write(move |w| w.next(&format!("landing:{id}")))
            .await?;
        save(
            app,
            repo,
            Landing {
                repo: repo.id.clone(),
                number: p.number,
                base: p.base.clone(),
                queued_at: now.clone(),
                sequence,
                state: "waiting".into(),
                state_at: now,
                gate: old
                    .as_ref()
                    .filter(|e| {
                        e.state == "left"
                            && !changed
                            && e.reason.as_ref().is_some_and(|r| r.code == "gate_failed")
                    })
                    .and_then(|e| e.gate.clone()),
                base_sha: base,
                head_sha: head,
                reason: None,
                notified_at: None,
                notify: false,
                resume_publish: old.as_ref().is_some_and(|e| e.resume_publish),
                published: false,
                error: None,
            },
            None,
        )
        .await?;
    }
    repair_refs_locked(app, repo).await?;
    let id = repo.id.clone();
    let entries = app.store.read(move |r| active(r, &id)).await?;
    let mut bases = std::collections::BTreeSet::new();
    for mut e in entries {
        if !bases.insert(e.base.clone()) {
            continue;
        }
        if e.state == "waiting" {
            let gate = match &e.gate {
                Some(sha) => GateCommit::Ready {
                    sha: sha.clone(),
                    tree: String::new(),
                },
                None => {
                    app.git
                        .gate_commit(&app.git.repo_dir(&repo.path), &e.base_sha, &e.head_sha)
                        .await?
                }
            };
            match gate {
                GateCommit::Conflicts { files } => {
                    let mut why = reason("conflict", "The head conflicts with the base.");
                    why.files = files;
                    leave(app, repo, e, why, &[]).await?;
                    continue;
                }
                GateCommit::Ready { sha, tree } => {
                    if sha != e.head_sha
                        && !tree.is_empty()
                        && app
                            .git
                            .tree_oid(&app.git.repo_dir(&repo.path), &e.base_sha)
                            .await?
                            .as_ref()
                            == Some(&tree)
                    {
                        leave(
                            app,
                            repo,
                            e,
                            reason(
                                "already_in_base",
                                "The base already contains these changes.",
                            ),
                            &[],
                        )
                        .await?;
                        continue;
                    }
                    e.gate = Some(sha);
                    e.state = "gating".into();
                    e.state_at = util::now();
                    save(app, repo, e.clone(), None).await?;
                    app.failpoint("gate_before_update_ref");
                    if e.gate.as_deref() != Some(&e.head_sha) {
                        app.git
                            .update_ref(
                                &app.git.repo_dir(&repo.path),
                                &format!("refs/branvil/gate/{}", e.number),
                                e.gate.as_deref().unwrap(),
                                ZERO_SHA,
                                "branvil: landing gate",
                            )
                            .await?;
                        app.failpoint("gate_after_update_ref");
                        crate::refs::record_refs_locked(app, repo).await?;
                    }
                }
            }
        }
        if !["gating", "passed"].contains(&e.state.as_str()) {
            continue;
        }
        let checks = app.ci.head_checks(repo, e.gate.as_deref().unwrap()).await?;
        let failed: Vec<_> = checks
            .checks
            .iter()
            .filter(|c| {
                repo.required_checks.contains(&c.name) && c.status == "completed" && !c.succeeded()
            })
            .map(|c| c.name.clone())
            .collect();
        if !failed.is_empty() {
            let mut why = reason("gate_failed", "Required landing gate jobs failed.");
            why.jobs = failed;
            leave(app, repo, e, why, &checks.checks).await?;
        } else if repo
            .required_checks
            .iter()
            .all(|n| checks.checks.iter().any(|c| &c.name == n && c.succeeded()))
        {
            if e.state != "passed" {
                e.state = "passed".into();
                e.state_at = util::now();
                e.notify = repo.auto_land;
                if let Some(p) = pulls.iter().find(|p| p.number == e.number) {
                    (e.published, e.error) = publication(repo, p);
                }
                save(app, repo, e, None).await?;
            }
        } else if e.state == "passed" {
            e.state = "gating".into();
            e.state_at = util::now();
            e.notify = false;
            e.notified_at = None;
            e.published = false;
            e.error = None;
            save(app, repo, e, None).await?;
        }
    }
    Ok(())
}

/// Restores only refs implied by committed queue entries. Startup calls this
/// before accepting requests, so even an immediate backup includes their objects.
/// The caller holds the repository lock; no eligibility or new gates are made.
pub async fn repair_refs_locked(app: &App, repo: &Repo) -> AppResult<Vec<crate::refs::RefChange>> {
    let id = repo.id.clone();
    let entries = app.store.read(move |r| active(r, &id)).await?;
    let dir = app.git.repo_dir(&repo.path);
    let actual = app.git.all_refs(&dir).await?;
    for (name, sha) in actual
        .iter()
        .filter(|(name, _)| name.starts_with("refs/branvil/gate/"))
    {
        if !entries.iter().any(|e| {
            ["gating", "passed"].contains(&e.state.as_str())
                && name == &format!("refs/branvil/gate/{}", e.number)
                && e.gate.as_ref() == Some(sha)
        }) {
            app.git
                .run_ok(Some(&dir), &["update-ref", "-d", name])
                .await?;
        }
    }
    for e in entries.iter().filter(|e| {
        ["gating", "passed"].contains(&e.state.as_str()) && e.gate.as_ref() != Some(&e.head_sha)
    }) {
        if let Some(gate) = &e.gate {
            let name = format!("refs/branvil/gate/{}", e.number);
            if actual.get(&name) != Some(gate) {
                app.git
                    .run_ok(Some(&dir), &["update-ref", &name, gate])
                    .await?;
            }
        }
    }
    crate::refs::record_refs_locked(app, repo).await
}

/// Gate checks remain visible after departure, including their retry attempts.
pub async fn checks(app: &App, repo: &Repo, number: u64) -> AppResult<Vec<Check>> {
    let id = repo.id.clone();
    let Some(e) = app.store.read(move |r| current(r, &id, number)).await? else {
        return Ok(Vec::new());
    };
    if let Some(why) = e.reason.as_ref().filter(|r| r.code == "conflict") {
        return Ok(vec![Check {
            name: "gate".into(),
            status: "completed".into(),
            conclusion: Some("failure".into()),
            details: Some(format!("{}\n{}", why.message, why.files.join("\n"))),
            ..Check::default()
        }]);
    }
    let Some(gate) = e.gate else {
        return Ok(Vec::new());
    };
    let mut checks = app.ci.head_checks(repo, &gate).await?.checks;
    for c in &mut checks {
        c.name = format!("gate/{}", c.name);
    }
    Ok(checks)
}
