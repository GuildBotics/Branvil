//! What this build changes in a database of the previous schema version
//! (`store::upgrade`, run by `branvil admin upgrade` and `branvil restore`).
//! It upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised
//! again, this whole module is replaced by the upgrade from the version
//! before the new one.
//!
//! 7 → 8: a CI run no longer records where its pipeline was read
//! (`source`), because every commit runs its own pipeline. Runs whose pipeline
//! was read from a pull request's base are kept with their attempts; the next
//! registration of the commit replaces the pipeline with the commit's own
//! (`Ci::reconcile` registers every open pull request head at start), and the
//! checks count only attempts of the run's current jobs.

use crate::ci::queue::Run;
use crate::store::{self, Rd, Wtx};

/// The schema version this module upgrades from.
pub const FROM: u64 = 7;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Applies the upgrade inside the caller's write transaction. Any record it
/// cannot read as expected fails the whole upgrade. Returns what it changed.
pub fn apply(w: &Wtx) -> anyhow::Result<String> {
    let db = |e: crate::error::AppError| anyhow::anyhow!("{}", e.message);
    let runs = w
        .prefix_raw(store::CI_RUNS, &[], None, usize::MAX)
        .map_err(db)?;
    for (key, bytes) in &runs {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| anyhow::anyhow!("a CI run is not JSON: {e}"))?;
        let source = value
            .as_object_mut()
            .and_then(|run| run.remove("source"))
            .ok_or_else(|| anyhow::anyhow!("a CI run has no source: {value}"))?;
        if !(source["branch"].is_string() && source["sha"].is_string()) {
            anyhow::bail!("a CI run has an unexpected source {source}: {value}");
        }
        let run: Run = serde_json::from_value(value)
            .map_err(|e| anyhow::anyhow!("a CI run does not decode: {e}"))?;
        w.put(store::CI_RUNS, key, &run).map_err(db)?;
    }
    Ok(format!(
        "removed the pipeline source of {} CI run(s); nothing else changed",
        runs.len()
    ))
}
