//! Durable storage: events (still sealed, exactly as signed), epoch keys, and
//! which servers we've joined. Plaintext never touches disk.

use std::path::Path;

use anyhow::Result;
use iroh::EndpointId as PeerId;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::{
    crypto::EpochKey,
    event::{Event, ServerId},
};

type EventKey = ([u8; 32], [u8; 32], u64);

const EVENTS: TableDefinition<EventKey, &[u8]> = TableDefinition::new("events");
const KEYS: TableDefinition<([u8; 32], u32), [u8; 32]> = TableDefinition::new("keys");
const SERVERS: TableDefinition<[u8; 32], ()> = TableDefinition::new("servers");
const NAMES: TableDefinition<[u8; 32], &str> = TableDefinition::new("names");

pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Database::create(path)?;
        let tx = db.begin_write()?;
        tx.open_table(EVENTS)?;
        tx.open_table(KEYS)?;
        tx.open_table(SERVERS)?;
        tx.open_table(NAMES)?;
        tx.commit()?;
        Ok(Self { db })
    }

    pub fn add_server(&self, id: ServerId) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(SERVERS)?.insert(id.0, ())?;
        tx.commit()?;
        Ok(())
    }

    pub fn servers(&self) -> Result<Vec<ServerId>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(SERVERS)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            out.push(ServerId(row?.0.value()));
        }
        Ok(out)
    }

    /// Remembers the display name a peer last introduced themselves with.
    pub fn put_name(&self, peer: PeerId, name: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(NAMES)?.insert(*peer.as_bytes(), name)?;
        tx.commit()?;
        Ok(())
    }

    pub fn names(&self) -> Result<Vec<(PeerId, String)>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(NAMES)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (k, v) = row?;
            if let Ok(peer) = PeerId::from_bytes(&k.value()) {
                out.push((peer, v.value().to_string()));
            }
        }
        Ok(out)
    }

    pub fn put_events(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(EVENTS)?;
            for e in events {
                let bytes = postcard::to_stdvec(e)?;
                table.insert((e.server.0, *e.author.as_bytes(), e.seq), bytes.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All stored events for `server`, grouped by author in seq order.
    pub fn events(&self, server: ServerId) -> Result<Vec<Event>> {
        self.scan((server.0, [0; 32], 0)..=(server.0, [0xff; 32], u64::MAX))
    }

    /// `author`'s events in `server` with seq greater than `after`.
    pub fn events_after(&self, server: ServerId, author: PeerId, after: u64) -> Result<Vec<Event>> {
        let a = *author.as_bytes();
        self.scan((server.0, a, after.saturating_add(1))..=(server.0, a, u64::MAX))
    }

    fn scan(&self, range: std::ops::RangeInclusive<EventKey>) -> Result<Vec<Event>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(EVENTS)?;
        let mut out = Vec::new();
        for row in table.range(range)? {
            out.push(postcard::from_bytes(row?.1.value())?);
        }
        Ok(out)
    }

    pub fn put_key(&self, server: ServerId, epoch: u32, key: &EpochKey) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(KEYS)?.insert((server.0, epoch), *key)?;
        tx.commit()?;
        Ok(())
    }

    pub fn keys(&self, server: ServerId) -> Result<Vec<(u32, EpochKey)>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(KEYS)?;
        let mut out = Vec::new();
        for row in table.range((server.0, 0)..=(server.0, u32::MAX))? {
            let (k, v) = row?;
            out.push((k.value().1, v.value()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ServerState;

    #[test]
    fn events_and_keys_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hearth-store-{}", std::process::id()));
        let store = Store::open(&dir.join("db.redb")).unwrap();
        let owner = iroh::SecretKey::generate();
        let (state, key, events) = ServerState::create(&owner, "s");

        store.add_server(state.id).unwrap();
        store.put_events(&events).unwrap();
        store.put_key(state.id, 0, &key).unwrap();

        assert_eq!(store.servers().unwrap(), [state.id]);
        assert_eq!(store.events(state.id).unwrap(), events);
        assert_eq!(store.events_after(state.id, owner.public(), 2).unwrap(), events[2..]);
        assert_eq!(store.keys(state.id).unwrap(), [(0, key)]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
