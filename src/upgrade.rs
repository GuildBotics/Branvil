//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 8 → 9: what a job runs on is the runner's setting. A pipeline names its
//! images by tag only and gives no `cpus` or `memory_mib`, and its
//! `timeout_secs` is optional; a job definition no longer has `cpus` and
//! `memory_mib`, and an attempt records the configuration digest of the image
//! it ran on. Every attempt's job loses `cpus` and `memory_mib` and keeps its
//! time limit, with no image digest. Every run takes the pipeline its commit's
//! own `.branvil/ci.toml` reads as now (one that names a digest or gives
//! `cpus` is invalid, with the reason; a commit no longer in its repository
//! gets a failing "could not verify" pipeline), and its unfinished attempts of
//! another definition are stopped (`queue::take_pipeline`). Attempts are
//! kept: those of another definition stay as history and never count as
//! checks. Any Git failure fails the whole upgrade, before anything is
//! written.

use serde_json::Value;

use crate::ci::pipeline;
use crate::ci::queue::{self, Attempt, PipelineState, Run};
use crate::config::Layout;
use crate::error::AppError;
use crate::git::Git;
use crate::store::{self, Rd, Upgraded, Upgrading};
use crate::util;

/// The schema version this module upgrades from.
pub const FROM: u64 = 8;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

const NOT_VERIFIED: &str = "Branvil could not verify it: the commit was not in its repository when the data was upgraded to schema version 9.";
const STOPPED: &str = "Stopped by the upgrade to schema version 9: the commit's pipeline does not define this job so.";

/// Upgrades the database of `layout`, reading the pipelines from its
/// repositories with `git`. Returns `None` when it already has this build's
/// version.
pub async fn upgrade(layout: &Layout, git: &Git) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let attempts = db
        .read(|r| r.prefix_raw(store::CI_ATTEMPTS, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| Ok((key, attempt_of(&bytes)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let records = db.read(|r| r.prefix_raw(store::CI_RUNS, &[], None, usize::MAX))?;
    let mut runs = Vec::with_capacity(records.len());
    for (_, bytes) in records {
        let run = run_of(&bytes)?;
        let pipeline = commits_pipeline(git, &run).await?;
        runs.push((run, pipeline));
    }
    let now = util::now();
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    let upgraded = db.finish(|w| {
        // Before the runs, whose pipelines are compared with their jobs. What
        // an attempt's index entries are made of does not change.
        for (key, attempt) in &attempts {
            w.put(store::CI_ATTEMPTS, key, attempt).map_err(db_err)?;
        }
        let (total, mut changed, mut unverified, mut stopped) = (runs.len(), 0, 0, 0);
        for (mut run, pipeline) in runs {
            changed += usize::from(run.pipeline != pipeline);
            unverified += usize::from(
                matches!(&pipeline, PipelineState::Invalid { message } if message == NOT_VERIFIED),
            );
            let old = run.clone();
            stopped += queue::take_pipeline(w, &mut run, pipeline, STOPPED, &now).map_err(db_err)?;
            // Written even when unchanged: its jobs lose `cpus` and `memory_mib`.
            queue::put_run(w, Some(&old), &run).map_err(db_err)?;
        }
        Ok(format!(
            "the jobs of {} CI attempt(s) lost their cpus and memory_mib; checked the pipeline of {total} CI run(s) against its commit: {changed} took a new one ({unverified} could not be verified: the commit is gone), {stopped} unfinished attempt(s) of another definition were stopped; nothing else changed",
            attempts.len()
        ))
    })?;
    Ok(Some(upgraded))
}

/// A version-8 job definition, in place, as a version-9 one: without `cpus`
/// and `memory_mib`, its `timeout_secs` kept.
fn upgrade_job(job: &mut Value) -> anyhow::Result<()> {
    let fields = job
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("a CI job is not an object"))?;
    for key in ["cpus", "memory_mib"] {
        if !fields.remove(key).is_some_and(|v| v.is_u64()) {
            anyhow::bail!(
                "CI job {} has no {key}",
                fields.get("name").unwrap_or(&Value::Null)
            );
        }
    }
    Ok(())
}

/// A version-8 attempt record as a version-9 attempt.
fn attempt_of(bytes: &[u8]) -> anyhow::Result<Attempt> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("a CI attempt is not JSON: {e}"))?;
    upgrade_job(&mut value["job"])?;
    value["image_digest"] = Value::Null;
    serde_json::from_value(value).map_err(|e| anyhow::anyhow!("a CI attempt does not decode: {e}"))
}

/// A version-8 run record as a version-9 run.
fn run_of(bytes: &[u8]) -> anyhow::Result<Run> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("a CI run is not JSON: {e}"))?;
    if let Some(jobs) = value.pointer_mut("/pipeline/jobs") {
        let jobs = jobs
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("the jobs of a CI run are not a list"))?;
        for job in jobs {
            upgrade_job(job)?;
        }
    }
    serde_json::from_value(value).map_err(|e| anyhow::anyhow!("a CI run does not decode: {e}"))
}

/// The pipeline of `run`'s commit as the service reads it.
async fn commits_pipeline(git: &Git, run: &Run) -> anyhow::Result<PipelineState> {
    let dir = git.repo_dir(&run.repo_path);
    let failed = |e: AppError| {
        anyhow::anyhow!(
            "reading the pipeline of {} {}: {}",
            run.repo_path,
            run.sha,
            e.message
        )
    };
    // 0: the commit is there; 1: it is not; anything else: Git failed.
    let out = git
        .run(Some(&dir), &["cat-file", "-e", &run.sha])
        .await
        .map_err(failed)?;
    match out.status {
        0 => pipeline::read(git, &dir, &run.sha).await.map_err(failed),
        1 => Ok(PipelineState::Invalid {
            message: NOT_VERIFIED.into(),
        }),
        _ => anyhow::bail!(
            "checking {} {} in {}: {}",
            run.repo_path,
            run.sha,
            dir.display(),
            out.stderr.trim()
        ),
    }
}
