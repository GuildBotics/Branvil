//! What this build changes in a database of the previous schema version
//! (`branvil admin upgrade`, and `branvil restore` of such a backup). It
//! upgrades from `FROM` only; when `store::SCHEMA_VERSION` is raised again,
//! this whole module is replaced by the upgrade from the version before the
//! new one.
//!
//! 10 → 11: every ticket is a card of its workspace's board. A ticket that
//! was off the board goes to the end of `new` when open and of `done` when
//! closed, in the order of the ticket table (by repository id, then number),
//! and gets its card index entry.

use serde_json::{Value, json};

use crate::config::Layout;
use crate::domain::tickets;
use crate::error::AppError;
use crate::model::{self, Ticket};
use crate::store::{self, Rd, Upgraded, Upgrading};

/// The schema version this module upgrades from.
pub const FROM: u64 = 10;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

/// Upgrades the database of `layout`. Returns `None` when it already has this
/// build's version.
pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Every ticket must read as this version's before anything is written;
    // the ones off the board are kept to be placed.
    let mut off_board = Vec::new();
    for (key, bytes) in db.read(|r| r.prefix_raw(store::TICKETS, &[], None, usize::MAX))? {
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("a ticket is not JSON: {e}"))?;
        let placed = !value["lane"].is_null();
        if !placed {
            // The lane it goes to; its rank is given in it below.
            value["lane"] = json!(if value["state"] == "closed" {
                "done"
            } else {
                "new"
            });
            value["rank"] = json!("");
        }
        let ticket: Ticket = serde_json::from_value(value)
            .map_err(|e| anyhow::anyhow!("a ticket does not decode: {e}"))?;
        if !placed {
            off_board.push((key, ticket));
        }
    }
    let db_err = |e: AppError| anyhow::anyhow!("{}", e.message);
    Ok(Some(db.finish(|w| {
        for (key, ticket) in &off_board {
            let t = tickets::place(w, ticket, &ticket.lane, None, None).map_err(db_err)?;
            // Its other index entries are there already; putting them again
            // changes nothing.
            model::put_indexed(w, store::TICKETS, key, None, &t, model::ticket_index(&t))
                .map_err(db_err)?;
        }
        let closed = off_board.iter().filter(|(_, t)| t.lane == "done").count();
        Ok(format!(
            "{} open ticket(s) off the board went to the end of new and {closed} closed one(s) to the end of done; nothing else changed",
            off_board.len() - closed
        ))
    })?))
}
