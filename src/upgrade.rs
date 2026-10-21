//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 11 → 12: a member names the members asked to review every pull request it
//! opens. Every member gets none; no index entry follows from that.

use serde_json::{Value, json};

use crate::config::Layout;
use crate::error::AppError;
use crate::model::Member;
use crate::store::{self, Rd, Upgraded, Upgrading};

/// The schema version this module upgrades from.
pub const FROM: u64 = 11;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Upgrades the database of `layout`. Returns `None` when it already has this
/// build's version.
pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Every member must read as this version's before anything is written.
    let members = db
        .read(|r| r.prefix_raw(store::MEMBERS, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| Ok((key, member_of(&bytes)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, member) in &members {
            w.put(store::MEMBERS, key, member).map_err(db_err)?;
        }
        Ok(format!(
            "{} member(s) got no reviewers; nothing else changed",
            members.len()
        ))
    })?))
}

/// A version-11 member record as a version-12 member.
fn member_of(bytes: &[u8]) -> anyhow::Result<Member> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("a member is not JSON: {e}"))?;
    value["reviewers"] = json!([]);
    serde_json::from_value(value).map_err(|e| anyhow::anyhow!("a member does not decode: {e}"))
}
