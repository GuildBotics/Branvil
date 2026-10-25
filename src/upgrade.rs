//! 15 → 16: keep the reviewed head distinct from a landing gate and add
//! persistent landing queues. Only the preceding schema is supported.

use crate::config::Layout;
use crate::model::{Member, Pull};
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 15;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    for member in db.read(|r| r.prefix_all::<Member>(store::MEMBERS, &[]))? {
        anyhow::ensure!(
            crate::util::fold(&member.name) != "branvil",
            "member name branvil is reserved; rename that member before upgrading"
        );
    }
    let mut pulls = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::PULLS, &[], None, usize::MAX))? {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        value["merged_head"] = if value["merged"] == true {
            if value["upstream"]["state"] == "merged" {
                value["upstream"]["pushed"].clone()
            } else {
                value["merge_sha"].clone()
            }
        } else {
            serde_json::Value::Null
        };
        if let Some(upstream) = value["upstream"].as_object_mut() {
            upstream.remove("notified_at");
        }
        pulls.push((key, serde_json::from_value::<Pull>(value)?));
    }
    Ok(Some(db.finish(|w| {
        for (key, pull) in &pulls {
            w.put(store::PULLS, key, pull)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        Ok(format!(
            "{} pull request(s) upgraded; landing queues initialized",
            pulls.len()
        ))
    })?))
}
