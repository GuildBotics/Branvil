//! 23 → 24: images attached to tickets (`attachments`, new and empty) and the
//! index of the references to them, derived from the ticket bodies and
//! comments kept. Only the immediately preceding format is read here.

use redb::TableHandle;

use crate::config::Layout;
use crate::model;
use crate::store::{self, Upgraded, Upgrading};

pub const FROM: u64 = 23;
const _: () = assert!(FROM + 1 == store::SCHEMA_VERSION);

pub fn upgrade(layout: &Layout) -> anyhow::Result<Option<Upgraded>> {
    let Some(db) = Upgrading::open(&layout.db())? else {
        return Ok(None);
    };
    let refs: Vec<model::IxEntry> = db.read(|r| {
        Ok(model::expected_index(r)?
            .into_iter()
            .filter(|e| e.table.name() == store::IX_ATTACHMENT_REFS.name())
            .collect())
    })?;
    Ok(Some(db.finish(|w| {
        for e in &refs {
            w.put_raw(e.table, &e.key, &e.value)
                .map_err(|e| anyhow::anyhow!(e.message))?;
        }
        Ok(format!(
            "{} reference(s) of ticket attachments indexed",
            refs.len()
        ))
    })?))
}
