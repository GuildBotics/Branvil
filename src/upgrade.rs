//! 16 → 17: record protected branches and required-check failure delivery.
//! Only the preceding schema is supported.

use crate::ci::queue::Run;
use crate::config::Layout;
use crate::model::Repo;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 16;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let repos = db.read(|r| r.prefix_all::<Repo>(store::REPOS, &[]))?;
    let mut runs = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::CI_RUNS, &[], None, usize::MAX))? {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let repo = repos.iter().find(|repo| value["repo"] == repo.id);
        let branches: Vec<_> = value["branches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b.as_str())
            .filter(|b| repo.is_some_and(|repo| repo.is_protected(b)))
            .collect();
        value["protected_branches"] = serde_json::to_value(branches)?;
        value["failure_notified_at"] = serde_json::Value::Null;
        value["failure_pending"] = serde_json::Value::Bool(false);
        runs.push((key, serde_json::from_value::<Run>(value)?));
    }
    Ok(Some(db.finish(|w| {
        for (key, run) in &runs {
            w.put(store::CI_RUNS, key, run)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        for (_, run) in &runs {
            crate::ci::queue::put_run(w, run).map_err(|e| anyhow::anyhow!(e.message))?;
        }
        crate::model::rebuild_indexes(w).map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(format!(
            "{} CI run(s) upgraded; failure notifications initialized",
            runs.len()
        ))
    })?))
}
