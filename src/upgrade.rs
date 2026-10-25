//! 14 → 15: record CI trees and execution reuse metadata, and index runs by
//! repository and tree. Only the immediately preceding schema is supported.

use serde_json::{Value, json};

use crate::ci::queue::{Attempt, Run};
use crate::config::Layout;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 14;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Decode every changed record and read Git before making the backup or
    // writing anything. Only a missing object, not a broken Git invocation,
    // leaves a run without a tree.
    let mut runs = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::CI_RUNS, &[], None, usize::MAX))? {
        let mut value: Value = serde_json::from_slice(&bytes)?;
        value["tree"] = Value::Null;
        let mut run: Run = serde_json::from_value(value)?;
        let out = crate::git::base_command_sync(&layout.git_config())
            .current_dir(layout.repo_dir(&run.repo_path))
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{}^{{tree}}", run.sha),
            ])
            .output()?;
        if out.status.success() {
            let tree = String::from_utf8(out.stdout)?.trim().to_string();
            anyhow::ensure!(
                crate::util::is_sha(&tree),
                "Git returned an invalid tree for {}",
                run.sha
            );
            run.tree = Some(tree);
        } else {
            anyhow::ensure!(
                out.status.code() == Some(1) && out.stderr.is_empty(),
                "cannot read tree of {} in {}: {}",
                run.sha,
                run.repo_path,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        runs.push((key, run));
    }
    let mut attempts = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::CI_ATTEMPTS, &[], None, usize::MAX))? {
        let mut value: Value = serde_json::from_slice(&bytes)?;
        value["reused_from"] = Value::Null;
        value["cache_key"] = Value::Null;
        // Schema 14 did not mark manual retries. Execute every old unfinished
        // attempt once so a queued retry (or its interruption requeue) cannot
        // silently reuse. Old successful executions remain usable sources.
        value["allow_reuse"] = json!(false);
        let attempt: Attempt = serde_json::from_value(value)?;
        attempts.push((key, attempt));
    }
    let err = |e: crate::error::AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, run) in &runs {
            w.put(store::CI_RUNS, key, run).map_err(err)?;
        }
        for (key, attempt) in &attempts {
            w.put(store::CI_ATTEMPTS, key, attempt).map_err(err)?;
        }
        crate::model::rebuild_indexes(w).map_err(err)?;
        Ok(format!("{} CI run(s) and {} attempt(s) upgraded; {} run(s) have no surviving commit; indexes rebuilt",
            runs.len(), attempts.len(), runs.iter().filter(|(_, r)| r.tree.is_none()).count()))
    })?))
}
