//! 22 → 23: job path filters and the service's PR-head selection snapshots.
//! Only the immediately preceding format is read here.

use crate::ci::queue::{Attempt, Run};
use crate::config::Layout;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 22;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let (runs, attempts) = db.read(|r| {
        Ok((
            r.prefix_raw(store::CI_RUNS, &[], None, usize::MAX)?,
            r.prefix_raw(store::CI_ATTEMPTS, &[], None, usize::MAX)?,
        ))
    })?;
    let runs = runs
        .into_iter()
        .map(|(key, bytes)| {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value["selection"] = serde_json::Value::Null;
            if let Some(jobs) = value["pipeline"]["jobs"].as_array_mut() {
                for job in jobs {
                    job["paths"] = serde_json::json!([]);
                }
            }
            Ok((key, serde_json::from_value::<Run>(value)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let attempts = attempts
        .into_iter()
        .map(|(key, bytes)| {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value["path_skip"] = serde_json::Value::Null;
            value["job"]["paths"] = serde_json::json!([]);
            Ok((key, serde_json::from_value::<Attempt>(value)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(db.finish(|w| {
        for (key, run) in &runs {
            w.put(store::CI_RUNS, key, run)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        for (key, attempt) in &attempts {
            w.put(store::CI_ATTEMPTS, key, attempt)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        Ok(format!(
            "{} CI run(s) upgraded with no path selection; {} attempt(s) preserved",
            runs.len(),
            attempts.len()
        ))
    })?))
}
