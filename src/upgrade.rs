//! 18 → 19: record automatic retries and their recovery notifications.
//! Only the preceding schema is supported.

use crate::ci::queue::Attempt;
use crate::config::Layout;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 18;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let mut attempts = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::CI_ATTEMPTS, &[], None, usize::MAX))? {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        value["wait_for_runner"] = serde_json::json!(false);
        value["auto_retry"] = serde_json::Value::Null;
        value["recovery_notified_at"] = serde_json::Value::Null;
        attempts.push((key, serde_json::from_value::<Attempt>(value)?));
    }
    Ok(Some(db.finish(|w| {
        for (key, attempt) in &attempts {
            w.put(store::CI_ATTEMPTS, key, attempt)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        crate::model::rebuild_indexes(w).map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(format!(
            "{} CI attempt(s) upgraded with no automatic retry",
            attempts.len()
        ))
    })?))
}
