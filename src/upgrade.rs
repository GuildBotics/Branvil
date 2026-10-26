//! 20 → 21: remove the publication flag and split landing eligibility reasons.
//! Only the preceding schema is supported.

use crate::config::Layout;
use crate::model::Landing;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 20;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let entries = db
        .read(|r| r.prefix_raw(store::LANDING, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("invalid landing entry"))?
                .remove("published");
            if value["reason"]["code"] == "ineligible" {
                value["reason"]["code"] = match value["reason"]["message"].as_str() {
                    Some("The head adds nothing to the base.") => "empty_head",
                    Some(
                        "Publishing requires the default base and an author account on the host.",
                    ) => "publish_requirements",
                    Some("Required head checks have not succeeded.") => "head_checks",
                    Some("The head needs more approvals.") => "approvals",
                    _ => anyhow::bail!("unknown saved landing eligibility reason"),
                }
                .into();
            }
            Ok((key, serde_json::from_value::<Landing>(value)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(db.finish(|w| {
        for (key, entry) in &entries {
            w.put(store::LANDING, key, entry)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        Ok(format!("{} landing entry(s) upgraded", entries.len()))
    })?))
}
