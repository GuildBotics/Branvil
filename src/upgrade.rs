//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 13 → 14: a repository may land its pull requests by itself, and a
//! published pull request says whether Branvil published it and when the
//! webhook was told. Every repository lands none by itself, every published
//! pull request was published by the owner and told nobody, and a pending
//! merge no longer names who merged it; no index entry follows from that.

use serde_json::{Value, json};

use crate::config::Layout;
use crate::error::AppError;
use crate::model::{JournalEntry, Pull, Repo};
use crate::store::{self, Rd, Tbl, Upgraded, Upgrading};

/// The schema version this module upgrades from.
pub const FROM: u64 = 13;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Upgrades the database of `layout`. Returns `None` when it already has this
/// build's version.
pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Every record that changes must read as this version's before anything
    // is written.
    let repos = changed::<Repo>(&db, store::REPOS, "a repository", |r| {
        r["auto_land"] = json!(false);
        true
    })?;
    let pulls = changed::<Pull>(&db, store::PULLS, "a pull request", |p| {
        let Some(x) = p["upstream"].as_object_mut() else {
            return false;
        };
        x.insert("auto".into(), json!(false));
        x.insert("notified_at".into(), Value::Null);
        true
    })?;
    let merges = changed::<JournalEntry>(&db, store::JOURNAL, "a pending merge", |e| {
        e["op"].as_object_mut().map(|op| op.remove("merged_by"));
        true
    })?;
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, repo) in &repos {
            w.put(store::REPOS, key, repo).map_err(db_err)?;
        }
        for (key, pull) in &pulls {
            w.put(store::PULLS, key, pull).map_err(db_err)?;
        }
        for (key, entry) in &merges {
            w.put(store::JOURNAL, key, entry).map_err(db_err)?;
        }
        Ok(format!(
            "{} repository(ies) land nothing by themselves, {} published pull request(s) were published by the owner, {} pending merge(s) changed; nothing else changed",
            repos.len(),
            pulls.len(),
            merges.len()
        ))
    })?))
}

/// The records of `table` that `change` changes (it answers whether it
/// does), each read as this version's record.
fn changed<T: serde::de::DeserializeOwned>(
    db: &Upgrading,
    table: Tbl,
    what: &str,
    mut change: impl FnMut(&mut Value) -> bool,
) -> anyhow::Result<Vec<(Vec<u8>, T)>> {
    let mut out = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(table, &[], None, usize::MAX))? {
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("{what} is not JSON: {e}"))?;
        if change(&mut value) {
            let record = serde_json::from_value(value)
                .map_err(|e| anyhow::anyhow!("{what} does not decode: {e}"))?;
            out.push((key, record));
        }
    }
    Ok(out)
}
