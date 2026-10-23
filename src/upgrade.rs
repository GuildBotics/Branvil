//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 12 → 13: a repository names the approvals a merge or a publish needs.
//! Every repository needs one; no index entry follows from that.

use serde_json::{Value, json};

use crate::config::Layout;
use crate::error::AppError;
use crate::model::Repo;
use crate::store::{self, Rd, Upgraded, Upgrading};

/// The schema version this module upgrades from.
pub const FROM: u64 = 12;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Upgrades the database of `layout`. Returns `None` when it already has this
/// build's version.
pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Every repository must read as this version's before anything is written.
    let repos = db
        .read(|r| r.prefix_raw(store::REPOS, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| Ok((key, repo_of(&bytes)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, repo) in &repos {
            w.put(store::REPOS, key, repo).map_err(db_err)?;
        }
        Ok(format!(
            "{} repository(ies) need 1 approval; nothing else changed",
            repos.len()
        ))
    })?))
}

/// A version-12 repository record as a version-13 repository.
fn repo_of(bytes: &[u8]) -> anyhow::Result<Repo> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("a repository is not JSON: {e}"))?;
    value["required_approvals"] = json!(1);
    serde_json::from_value(value).map_err(|e| anyhow::anyhow!("a repository does not decode: {e}"))
}
