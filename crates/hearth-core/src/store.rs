//! Durable storage: events (still sealed, exactly as signed), epoch keys, and
//! which servers we've joined. Plaintext never touches disk.

use std::path::Path;

use anyhow::Result;
use iroh::EndpointId as PeerId;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use serde::{Deserialize, Serialize};

use crate::{
    blob::BlobRef,
    crypto::EpochKey,
    event::{Event, ServerId},
    proto::Frame,
};

type EventKey = ([u8; 32], [u8; 32], u64);

const EVENTS: TableDefinition<EventKey, &[u8]> = TableDefinition::new("events");
const KEYS: TableDefinition<([u8; 32], u32), [u8; 32]> = TableDefinition::new("keys");
const SERVERS: TableDefinition<[u8; 32], ()> = TableDefinition::new("servers");
const NAMES: TableDefinition<[u8; 32], &str> = TableDefinition::new("names");
const FRIENDS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("friends");
const OUTBOX: TableDefinition<([u8; 32], u64), &[u8]> = TableDefinition::new("outbox");
const DMS: TableDefinition<[u8; 32], [u8; 32]> = TableDefinition::new("dms");
const AVATARS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("avatars");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FriendState {
    /// They asked us; we haven't answered.
    Incoming,
    /// We asked them; they haven't answered.
    Outgoing,
    Friends,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRecord {
    pub state: FriendState,
    pub since_ms: u64,
}

pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // A process restarting itself (e.g. after an update) can briefly race the
        // old one for the lock, so retry for a few seconds before giving up.
        let mut attempts = 0;
        let db = loop {
            match Database::create(path) {
                Err(redb::DatabaseError::DatabaseAlreadyOpen) if attempts < 50 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                other => break other?,
            }
        };
        let tx = db.begin_write()?;
        tx.open_table(EVENTS)?;
        tx.open_table(KEYS)?;
        tx.open_table(SERVERS)?;
        tx.open_table(NAMES)?;
        tx.open_table(FRIENDS)?;
        tx.open_table(OUTBOX)?;
        tx.open_table(DMS)?;
        tx.open_table(AVATARS)?;
        tx.open_table(META)?;
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

    /// Forgets a server entirely: membership, events, and keys.
    pub fn remove_server(&self, id: ServerId) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            tx.open_table(SERVERS)?.remove(id.0)?;
            tx.open_table(DMS)?.remove(id.0)?;
            let mut events = tx.open_table(EVENTS)?;
            events.retain_in((id.0, [0; 32], 0)..=(id.0, [0xff; 32], u64::MAX), |_, _| false)?;
            let mut keys = tx.open_table(KEYS)?;
            keys.retain_in((id.0, 0)..=(id.0, u32::MAX), |_, _| false)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn put_friend(&self, peer: PeerId, record: Option<FriendRecord>) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(FRIENDS)?;
            match record {
                Some(r) => {
                    t.insert(*peer.as_bytes(), postcard::to_stdvec(&r)?.as_slice())?;
                }
                None => {
                    t.remove(*peer.as_bytes())?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn friends(&self) -> Result<Vec<(PeerId, FriendRecord)>> {
        let tx = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in tx.open_table(FRIENDS)?.iter()? {
            let (k, v) = row?;
            if let (Ok(peer), Ok(r)) = (PeerId::from_bytes(&k.value()), postcard::from_bytes(v.value())) {
                out.push((peer, r));
            }
        }
        Ok(out)
    }

    /// Queues a frame for `peer` until we next connect to them.
    pub fn push_outbox(&self, peer: PeerId, frame: &Frame) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(OUTBOX)?;
            let p = *peer.as_bytes();
            let next = t.range((p, 0)..=(p, u64::MAX))?.next_back().transpose()?.map(|(k, _)| k.value().1 + 1).unwrap_or(0);
            t.insert((p, next), postcard::to_stdvec(frame)?.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes and returns everything queued for `peer`, oldest first.
    pub fn take_outbox(&self, peer: PeerId) -> Result<Vec<Frame>> {
        let tx = self.db.begin_write()?;
        let mut out = Vec::new();
        {
            let mut t = tx.open_table(OUTBOX)?;
            let p = *peer.as_bytes();
            for row in t.extract_from_if((p, 0)..=(p, u64::MAX), |_, _| true)? {
                let (_, v) = row?;
                if let Ok(f) = postcard::from_bytes(v.value()) {
                    out.push(f);
                }
            }
        }
        tx.commit()?;
        Ok(out)
    }

    pub fn put_dm(&self, server: ServerId, peer: PeerId) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(DMS)?.insert(server.0, *peer.as_bytes())?;
        tx.commit()?;
        Ok(())
    }

    pub fn dms(&self) -> Result<Vec<(ServerId, PeerId)>> {
        let tx = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in tx.open_table(DMS)?.iter()? {
            let (k, v) = row?;
            if let Ok(peer) = PeerId::from_bytes(&v.value()) {
                out.push((ServerId(k.value()), peer));
            }
        }
        Ok(out)
    }

    pub fn put_avatar(&self, peer: PeerId, avatar: Option<BlobRef>) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(AVATARS)?;
            match avatar {
                Some(a) => {
                    t.insert(*peer.as_bytes(), postcard::to_stdvec(&a)?.as_slice())?;
                }
                None => {
                    t.remove(*peer.as_bytes())?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn avatars(&self) -> Result<Vec<(PeerId, BlobRef)>> {
        let tx = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in tx.open_table(AVATARS)?.iter()? {
            let (k, v) = row?;
            if let (Ok(peer), Ok(a)) = (PeerId::from_bytes(&k.value()), postcard::from_bytes(v.value())) {
                out.push((peer, a));
            }
        }
        Ok(out)
    }

    pub fn put_meta<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(META)?.insert(key, postcard::to_stdvec(value)?.as_slice())?;
        tx.commit()?;
        Ok(())
    }

    pub fn meta<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(META)?;
        Ok(t.get(key)?.and_then(|v| postcard::from_bytes(v.value()).ok()))
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

    #[test]
    fn friends_outbox_dms_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.redb")).unwrap();
        let (a, b) = (iroh::SecretKey::generate().public(), iroh::SecretKey::generate().public());

        let rec = FriendRecord { state: FriendState::Outgoing, since_ms: 1 };
        store.put_friend(a, Some(rec)).unwrap();
        assert_eq!(store.friends().unwrap(), [(a, rec)]);
        store.put_friend(a, None).unwrap();
        assert!(store.friends().unwrap().is_empty());

        store.push_outbox(a, &Frame::FriendRequest { name: "x".into() }).unwrap();
        store.push_outbox(a, &Frame::FriendRemove).unwrap();
        store.push_outbox(b, &Frame::FriendRemove).unwrap();
        assert_eq!(store.take_outbox(a).unwrap(), [Frame::FriendRequest { name: "x".into() }, Frame::FriendRemove]);
        assert!(store.take_outbox(a).unwrap().is_empty());
        assert_eq!(store.take_outbox(b).unwrap().len(), 1);

        let owner = iroh::SecretKey::generate();
        let (state, key, events) = ServerState::create(&owner, "dm");
        store.add_server(state.id).unwrap();
        store.put_events(&events).unwrap();
        store.put_key(state.id, 0, &key).unwrap();
        store.put_dm(state.id, a).unwrap();
        assert_eq!(store.dms().unwrap(), [(state.id, a)]);
        store.remove_server(state.id).unwrap();
        assert!(store.servers().unwrap().is_empty() && store.events(state.id).unwrap().is_empty());
        assert!(store.keys(state.id).unwrap().is_empty() && store.dms().unwrap().is_empty());

        store.put_meta("x", &5u32).unwrap();
        assert_eq!(store.meta::<u32>("x").unwrap(), Some(5));
    }
}
