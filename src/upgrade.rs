//! 19 → 20: resolve existing ticket and comment mentions and derive their index.
//! Only the preceding schema is supported.

use crate::config::Layout;
use crate::model::{self, Comment, Ticket};
use crate::store::{self, Rd, Upgraded, Upgrading};

pub const FROM: u64 = 19;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    // Decode and resolve before the copy and the single write transaction.
    let read = |table| -> anyhow::Result<Vec<(Vec<u8>, serde_json::Value)>> {
        let mut records = Vec::new();
        for (key, bytes) in db.read(|r| r.prefix_raw(table, &[], None, usize::MAX))? {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let body = value["body"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing body"))?;
            value["mentions"] = serde_json::json!(db.read(|r| model::mentions(r, body))?);
            records.push((key, value));
        }
        Ok(records)
    };
    let tickets = read(store::TICKETS)?
        .into_iter()
        .map(|(key, value)| Ok((key, serde_json::from_value::<Ticket>(value)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let comments = read(store::COMMENTS)?
        .into_iter()
        .map(|(key, value)| Ok((key, serde_json::from_value::<Comment>(value)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(db.finish(|w| {
        for (key, ticket) in &tickets {
            w.put(store::TICKETS, key, ticket)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        for (key, comment) in &comments {
            w.put(store::COMMENTS, key, comment)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        crate::model::rebuild_indexes(w).map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(format!(
            "{} ticket(s) and {} comment(s) upgraded with mentions",
            tickets.len(),
            comments.len()
        ))
    })?))
}
