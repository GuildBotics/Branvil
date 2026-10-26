//! 21 → 22: tickets carry whether a person works on them interactively; none
//! does yet. Only the preceding schema is supported.

use crate::config::Layout;
use crate::model::Ticket;
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 21;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let tickets = db
        .read(|r| r.prefix_raw(store::TICKETS, &[], None, usize::MAX))?
        .into_iter()
        .map(|(key, bytes)| {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value["interactive"] = false.into();
            Ok((key, serde_json::from_value::<Ticket>(value)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(db.finish(|w| {
        for (key, ticket) in &tickets {
            w.put(store::TICKETS, key, ticket)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        Ok(format!(
            "{} ticket(s) upgraded as not interactive",
            tickets.len()
        ))
    })?))
}
