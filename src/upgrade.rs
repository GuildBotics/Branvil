//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 7 → 8: every commit runs its own pipeline, and a CI run no longer records
//! where its pipeline was read (`source`). A run of a pull request head held
//! its base's pipeline; each run takes the pipeline its commit's own
//! `.branvil/ci.toml` reads as now (a commit no longer in its repository gets
//! a failing "could not verify" pipeline), and its unfinished attempts of
//! another definition are stopped (`queue::take_pipeline`). Attempts are
//! kept: those of another definition stay as history and never count as
//! checks. Any Git failure fails the whole upgrade, before anything is
//! written.

use crate::ci::pipeline;
use crate::ci::queue::{self, PipelineState, Run};
use crate::config::Layout;
use crate::error::AppError;
use crate::git::Git;
use crate::store::{self, Rd, Upgraded, Upgrading};
use crate::util;

/// The schema version this module upgrades from.
pub const FROM: u64 = 7;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

const NOT_VERIFIED: &str = "Branvil could not verify it: the commit was not in its repository when the data was upgraded to schema version 8.";
const STOPPED: &str = "Stopped by the upgrade to schema version 8: the commit's pipeline does not define this job so.";

/// Upgrades the database of `layout`, reading the pipelines from its
/// repositories with `git`. Returns `None` when it already has this build's
/// version.
pub async fn upgrade(layout: &Layout, git: &Git) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
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
        let (total, mut changed, mut unverified, mut stopped) = (runs.len(), 0, 0, 0);
        for (mut run, pipeline) in runs {
            changed += usize::from(run.pipeline != pipeline);
            unverified += usize::from(
                matches!(&pipeline, PipelineState::Invalid { message } if message == NOT_VERIFIED),
            );
            let old = run.clone();
            stopped += queue::take_pipeline(w, &mut run, pipeline, STOPPED, &now).map_err(db_err)?;
            // Written even when unchanged: the record loses its source.
            queue::put_run(w, Some(&old), &run).map_err(db_err)?;
        }
        Ok(format!(
            "checked the pipeline of {total} CI run(s) against its commit: {changed} took the commit's own ({unverified} could not be verified: the commit is gone), {stopped} unfinished attempt(s) of another definition were stopped; nothing else changed"
        ))
    })?;
    Ok(Some(upgraded))
}

/// A version-7 run record as a version-8 run (without its `source`).
fn run_of(bytes: &[u8]) -> anyhow::Result<Run> {
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("a CI run is not JSON: {e}"))?;
    let source = value
        .as_object_mut()
        .and_then(|run| run.remove("source"))
        .ok_or_else(|| anyhow::anyhow!("a CI run has no source: {value}"))?;
    if !(source["branch"].is_string() && source["sha"].is_string()) {
        anyhow::bail!("a CI run has an unexpected source {source}: {value}");
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
