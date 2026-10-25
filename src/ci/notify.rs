//! Delivery of required-check failures of protected-branch runs. The landing
//! task calls this independently of its landing work, on events and at startup
//! and every minute. No transaction is held while sending.

use super::queue::{self, PipelineState, Run};
use crate::app::App;
use crate::error::AppResult;
use crate::store::{self, Rd};

pub async fn sweep(app: &App) -> AppResult<()> {
    let candidates = app
        .store
        .read(|r| {
            r.prefix_raw(store::IX_CI_UNNOTIFIED, &[], None, usize::MAX)
                .map(|entries| entries.into_iter().map(|(_, key)| key).collect::<Vec<_>>())
        })
        .await?;
    for key in candidates {
        let notice = app
            .store
            .read(move |r| {
                let Some(run) = r.get::<Run>(store::CI_RUNS, &key)? else {
                    return Ok(None);
                };
                if run.failure_notified_at.is_some() || run.protected_branches.is_empty() {
                    return Ok(None);
                }
                let repo = crate::domain::repo_by_id(r, &run.repo)?;
                let latest = match &run.pipeline {
                    PipelineState::Jobs { jobs } => {
                        queue::current_attempts(r, &run.repo, &run.sha, jobs)?
                    }
                    _ => vec![],
                }
                .into_iter()
                .map(|a| {
                    let source = queue::result_attempt(r, &a)?;
                    Ok((a, source))
                })
                .collect::<AppResult<Vec<_>>>()?;
                Ok(Some((repo, run, latest)))
            })
            .await;
        let (repo, run, latest) = match notice {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("CI notification: {e}");
                continue;
            }
        };
        let superseded: Vec<_> = latest
            .iter()
            .filter(|(a, _)| a.superseded)
            .map(|(a, _)| a.job.name.clone())
            .collect();
        let checks = app.ci.run_checks(&repo, &run, latest)?.checks;
        // A pending required job may still succeed (or be retried). Normal
        // supersession is not a failed check and must not page the owner.
        let required: Vec<_> = checks
            .iter()
            .filter(|c| repo.required_checks.contains(&c.name))
            .collect();
        if required.iter().any(|c| c.status != "completed") {
            continue;
        }
        let failed: Vec<_> = required
            .into_iter()
            .filter(|c| {
                !c.succeeded()
                    && !superseded.contains(&c.name)
                    && (c.recorded() || c.conclusion.as_deref() != Some("cancelled"))
            })
            .collect();
        if failed.is_empty() {
            continue;
        }
        let mut text = format!(
            "Required checks failed: {} [{}] {}",
            repo.path,
            run.protected_branches.join(", "),
            run.sha
        );
        for c in failed {
            text.push_str(&format!(
                "\n{}: {}\n{}",
                c.name,
                c.details
                    .as_deref()
                    .unwrap_or_else(|| c.conclusion.as_deref().unwrap_or("failed")),
                c.details_url.as_ref().cloned().unwrap_or_else(|| format!(
                    "{}/{}/-/commits/{}",
                    app.base_url, repo.path, run.sha
                ))
            ));
        }
        if !app.autoland.send(&app.layout, &text).await {
            break;
        }
        app.store
            .write(move |w| {
                if let Some(old) = queue::get_run(w, &run.repo, &run.sha)? {
                    let mut new = old.clone();
                    new.failure_notified_at = Some(crate::util::now());
                    queue::put_run(w, &new)?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::pipeline::JobDef;
    use crate::config::{Config, Layout};
    use crate::model::Repo;
    use std::sync::{Arc, Mutex};

    fn app(path: &std::path::Path) -> App {
        let config = Config::load(None, Some(path), None, None).unwrap();
        config.layout().create().unwrap();
        let store = store::Store::open(&config.layout().db(), 1 << 20, 1).unwrap();
        App::new(
            config,
            "http://127.0.0.1:7878".into(),
            store,
            vec![],
            "http://127.0.0.1:7878".into(),
        )
    }

    fn repo(id: &str) -> Repo {
        Repo {
            id: id.into(),
            path: format!("acme/{id}"),
            workspace: "workspace1".into(),
            default_branch: "main".into(),
            protected_branches: vec![],
            required_checks: vec!["test".into()],
            required_approvals: 0,
            auto_land: false,
            ci_network: false,
            labels: vec![],
            upstream: None,
            revision: 1,
            created_at: crate::util::now(),
        }
    }

    fn register(
        app: &App,
        repo: &Repo,
        sha: &str,
        pipeline: PipelineState,
        protected: bool,
        conclusion: &str,
    ) {
        let now = crate::util::now();
        app.store
            .write_sync(|w| {
                w.put(store::REPOS, &Repo::key(&repo.id), repo)?;
                queue::register(
                    w,
                    queue::Registration {
                        repo: repo.id.clone(),
                        repo_path: repo.path.clone(),
                        sha: sha.into(),
                        tree: None,
                        branch: if protected { "main" } else { "feature" }.into(),
                        protected,
                        pipeline,
                        network: false,
                    },
                    100,
                    &now,
                )?;
                if conclusion != "superseded" {
                    while let Some(claim) = queue::claim(w, "runner1", 60, &now)? {
                        queue::finish(
                            w,
                            &claim.attempt.id,
                            &claim.lease,
                            conclusion,
                            None,
                            queue::ResultInfo::default(),
                            &now,
                        )?;
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    fn jobs() -> PipelineState {
        PipelineState::Jobs {
            jobs: vec![JobDef {
                name: "test".into(),
                image: "alpine".into(),
                commands: vec!["true".into()],
                timeout_secs: None,
                artifacts: vec![],
                cache: None,
            }],
        }
    }

    #[test]
    fn missing_required_job_still_notifies_when_waiting_work_is_superseded() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let mut repo = repo("waiting");
        repo.required_checks.push("lint".into());
        let mut pipeline = jobs();
        if let PipelineState::Jobs { jobs } = &mut pipeline {
            jobs[0].name = "lint".into();
        }
        app.store
            .write_sync(|w| {
                w.put(store::REPOS, &Repo::key(&repo.id), &repo)?;
                let now = crate::util::now();
                queue::register(
                    w,
                    queue::Registration {
                        repo: repo.id.clone(),
                        repo_path: repo.path.clone(),
                        sha: "missing-test".into(),
                        tree: None,
                        branch: "main".into(),
                        protected: true,
                        pipeline,
                        network: false,
                    },
                    0,
                    &now,
                )?;
                assert!(
                    !queue::get_run(w, &repo.id, "missing-test")?
                        .unwrap()
                        .failure_pending
                );
                queue::branch_left(w, &repo.id, "missing-test", "main", "next", &now)?;
                assert!(
                    queue::get_run(w, &repo.id, "missing-test")?
                        .unwrap()
                        .failure_pending
                );
                assert_eq!(w.prefix_keys(store::IX_CI_UNNOTIFIED, &[])?.len(), 1);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn replacing_a_running_definition_removes_the_intermediate_failure_index() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let repo = repo("changed");
        app.store
            .write_sync(|w| {
                w.put(store::REPOS, &Repo::key(&repo.id), &repo)?;
                let mut registration = queue::Registration {
                    repo: repo.id.clone(),
                    repo_path: repo.path.clone(),
                    sha: "same".into(),
                    tree: None,
                    branch: "main".into(),
                    protected: true,
                    pipeline: jobs(),
                    network: false,
                };
                let now = crate::util::now();
                queue::register(w, registration, 10, &now)?;
                queue::claim(w, "runner1", 60, &now)?.unwrap();
                let mut replacement = jobs();
                if let PipelineState::Jobs { jobs } = &mut replacement {
                    jobs[0].commands.push("different".into());
                }
                registration = queue::Registration {
                    repo: repo.id.clone(),
                    repo_path: repo.path.clone(),
                    sha: "same".into(),
                    tree: None,
                    branch: "main".into(),
                    protected: true,
                    pipeline: replacement,
                    network: false,
                };
                queue::register(w, registration, 10, &now)?;
                assert!(
                    !queue::get_run(w, &repo.id, "same")?
                        .unwrap()
                        .failure_pending
                );
                assert!(w.prefix_keys(store::IX_CI_UNNOTIFIED, &[])?.is_empty());
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn failures_in_all_repositories_include_placeholders_retry_delivery_and_survive_restart()
    {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let first = repo("one");
        let second = repo("two");
        register(&app, &first, "failed", jobs(), true, "failure");
        assert_eq!(
            app.store
                .read_sync(|r| Ok(r.prefix_keys(store::IX_CI_UNNOTIFIED, &[])?.len()))
                .unwrap(),
            1,
            "finishing a required attempt indexes the failure before any run or branch change"
        );
        register(
            &app,
            &first,
            "absent",
            PipelineState::Absent,
            true,
            "success",
        );
        register(
            &app,
            &second,
            "invalid",
            PipelineState::Invalid {
                message: "invalid pipeline".into(),
            },
            true,
            "success",
        );
        register(
            &app,
            &second,
            "missing",
            PipelineState::Jobs { jobs: vec![] },
            true,
            "success",
        );
        register(&app, &second, "succeeded", jobs(), true, "success");
        register(&app, &second, "feature", jobs(), false, "failure");
        register(&app, &second, "superseded", jobs(), true, "superseded");
        app.store
            .write_sync(|w| {
                queue::branch_left(
                    w,
                    &second.id,
                    "superseded",
                    "main",
                    "next",
                    &crate::util::now(),
                )
            })
            .unwrap();
        // A branch moving cannot discard an already failed run's delivery.
        app.store
            .write_sync(|w| {
                queue::branch_left(w, &first.id, "failed", "main", "next", &crate::util::now())
            })
            .unwrap();
        sweep(&app).await.unwrap(); // unset webhook: no delivery is recorded
        assert!(
            app.store
                .read_sync(|r| queue::get_run(r, &first.id, "failed"))
                .unwrap()
                .unwrap()
                .failure_notified_at
                .is_none()
        );
        let received = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let copy = received.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/secret-webhook", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/secret-webhook",
            axum::routing::post(move |axum::Json(v): axum::Json<serde_json::Value>| {
                let copy = copy.clone();
                async move {
                    let mut items = copy.lock().unwrap();
                    items.push(v);
                    if items.len() == 1 {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        axum::http::StatusCode::OK
                    }
                }
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        std::fs::write(Layout::new(dir.path()).webhook(), url).unwrap();
        sweep(&app).await.unwrap();
        assert_eq!(received.lock().unwrap().len(), 1);
        sweep(&app).await.unwrap();
        let delivered = received.lock().unwrap().clone();
        assert_eq!(delivered.len(), 5, "one failed delivery plus four failures");
        for sha in ["failed", "absent", "invalid", "missing"] {
            assert!(delivered.iter().any(|v| {
                v["text"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("[main] {sha}"))
            }));
        }
        assert!(
            delivered
                .iter()
                .any(|v| v["text"].as_str().unwrap().contains("/-/jobs/"))
        );
        assert!(delivered.iter().any(|v| {
            v["text"]
                .as_str()
                .unwrap()
                .contains("defines no job named test")
        }));
        assert_eq!(
            app.store
                .read_sync(|r| Ok(r.prefix_keys(store::IX_CI_UNNOTIFIED, &[])?.len()))
                .unwrap(),
            0,
            "only undelivered failures are indexed"
        );
        let before = app.store.read_sync(queue::expected_index).unwrap().len();
        app.store.write_sync(crate::model::rebuild_indexes).unwrap();
        assert_eq!(
            app.store.read_sync(queue::expected_index).unwrap().len(),
            before
        );
        drop(app);
        let restarted = self::app(dir.path());
        sweep(&restarted).await.unwrap();
        assert_eq!(received.lock().unwrap().len(), 5);
        task.abort();
    }
}
