//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 9 → 10: a ticket may have a parent and tickets it is blocked by. Every
//! ticket gets neither; no index entry follows from that.

use serde_json::{Value, json};

use crate::config::Layout;
use crate::error::AppError;
use crate::model::Ticket;
use crate::store::{self, Rd, Upgraded, Upgrading};

/// The schema version this module upgrades from.
pub const FROM: u64 = 9;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Upgrades the database of `layout`. Returns `None` when it already has this
/// build's version.
pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let tickets = db
        .read(|r| r.prefix_raw(store::TICKETS, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| Ok((key, ticket_of(&bytes)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, ticket) in &tickets {
            w.put(store::TICKETS, key, ticket).map_err(db_err)?;
        }
        Ok(format!(
            "{} ticket(s) got no parent and no blockers; nothing else changed",
            tickets.len()
        ))
    })?))
}

/// A version-9 ticket record as a version-10 ticket.
fn ticket_of(bytes: &[u8]) -> anyhow::Result<Ticket> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("a ticket is not JSON: {e}"))?;
    value["parent"] = Value::Null;
    value["blocked_by"] = json!([]);
    serde_json::from_value(value).map_err(|e| anyhow::anyhow!("a ticket does not decode: {e}"))
}
