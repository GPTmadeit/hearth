//! Deterministic server state built from a set of events.
//!
//! Every peer runs the same rules over the same events, so everyone converges
//! on the same membership, channels, and history regardless of arrival order.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use iroh::{EndpointId as PeerId, SecretKey};

use crate::{
    crypto::EpochKey,
    event::{AdminOp, Attachment, Body, ChannelKind, Content, Event, EventId, ServerId},
};

const MAX_PENDING: usize = 50_000;

pub fn key_hash(key: &EpochKey) -> [u8; 32] {
    *blake3::keyed_hash(&[0x42; 32], key).as_bytes()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: EventId,
    pub name: String,
    pub kind: ChannelKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: EventId,
    pub channel: EventId,
    pub author: PeerId,
    pub body: String,
    pub sent_at_ms: u64,
    pub reply_to: Option<EventId>,
    pub attachments: Vec<Attachment>,
    pub edited: bool,
    pub deleted: bool,
    /// emoji -> who reacted with it, in the order they did.
    pub reactions: BTreeMap<String, Vec<PeerId>>,
}

/// A change aimed at a message that may not have arrived yet.
#[derive(Debug, Clone)]
enum Modify {
    Edit(String),
    Delete,
    React { emoji: String, on: bool },
}

/// A user-visible consequence of ingesting events or keys.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Renamed(String),
    Channel(Channel),
    Message(ChatMessage),
    MemberAdded(PeerId),
    MemberRemoved(PeerId),
    /// A member asked to leave. The owner's client acts on it.
    LeaveRequested(PeerId),
    /// A message was edited, deleted, or reacted to.
    MessageUpdated(ChatMessage),
    HostAdded(PeerId),
    HostRemoved(PeerId),
    Epoch(u32),
}

#[derive(Debug, Default)]
pub struct Ingested {
    /// Newly accepted events, in acceptance order (persist and forward these).
    pub accepted: Vec<Event>,
    pub changes: Vec<Change>,
    /// Events dropped for good, with the reason.
    pub rejected: Vec<(EventId, &'static str)>,
}

#[derive(Debug, Clone, Copy)]
enum Membership {
    Joined,
    /// Removed; events by this author beyond `through_seq` are rejected.
    Removed { through_seq: u64 },
}

enum Verdict {
    Accept,
    Wait,
    Reject(&'static str),
}

#[derive(Debug)]
pub struct ServerState {
    pub id: ServerId,
    owner: Option<PeerId>,
    have: BTreeMap<PeerId, u64>,
    /// Per peer, membership changes keyed by the owner seq that made them.
    membership: HashMap<PeerId, Vec<(u64, Membership)>>,
    /// (owner seq, epoch, key hash), epoch 0 first.
    epochs: Vec<(u64, u32, [u8; 32])>,
    keys: BTreeMap<u32, EpochKey>,
    hosts: Vec<PeerId>,
    /// Hosts never get keys, so they shouldn't hold sealed events in memory waiting for one.
    blind: bool,
    unverified_keys: Vec<(u32, EpochKey)>,
    pending: Vec<Event>,
    locked: Vec<Event>,
    pub name: Option<String>,
    pub channels: BTreeMap<EventId, Channel>,
    pub messages: BTreeMap<EventId, Vec<ChatMessage>>,
    /// Which channel each message is in.
    located: HashMap<EventId, EventId>,
    /// Modifications waiting for their target message, with who made them.
    pending_mods: HashMap<EventId, Vec<(PeerId, Modify)>>,
}

impl ServerState {
    pub fn new(id: ServerId) -> Self {
        Self {
            id,
            owner: None,
            have: BTreeMap::new(),
            membership: HashMap::new(),
            epochs: Vec::new(),
            keys: BTreeMap::new(),
            hosts: Vec::new(),
            blind: false,
            unverified_keys: Vec::new(),
            pending: Vec::new(),
            locked: Vec::new(),
            name: None,
            channels: BTreeMap::new(),
            messages: BTreeMap::new(),
            located: HashMap::new(),
            pending_mods: HashMap::new(),
        }
    }

    pub fn message(&self, id: &EventId) -> Option<&ChatMessage> {
        let channel = self.located.get(id)?;
        self.messages.get(channel)?.iter().find(|m| m.id == *id)
    }

    /// Applies `m` by `by` to its target if it has arrived; otherwise holds it.
    fn modify(&mut self, target: EventId, by: PeerId, m: Modify, changes: &mut Vec<Change>) {
        let owner = self.owner;
        let Some(channel) = self.located.get(&target).copied() else {
            self.pending_mods.entry(target).or_default().push((by, m));
            return;
        };
        let Some(msg) = self.messages.get_mut(&channel).and_then(|l| l.iter_mut().find(|x| x.id == target)) else {
            return;
        };
        let applied = match m {
            Modify::Edit(body) if by == msg.author && !msg.deleted => {
                msg.body = body;
                msg.edited = true;
                true
            }
            Modify::Delete if by == msg.author || Some(by) == owner => {
                msg.deleted = true;
                msg.body.clear();
                msg.attachments.clear();
                msg.reactions.clear();
                true
            }
            Modify::React { emoji, on } if !msg.deleted => {
                let who = msg.reactions.entry(emoji.clone()).or_default();
                let had = who.contains(&by);
                if on && !had {
                    who.push(by);
                } else if !on && had {
                    who.retain(|p| *p != by);
                }
                if who.is_empty() {
                    msg.reactions.remove(&emoji);
                }
                on != had
            }
            _ => false,
        };
        if applied {
            changes.push(Change::MessageUpdated(msg.clone()));
        }
    }

    /// Creates a brand-new server owned by `secret`, with default channels.
    /// Returns the state, the epoch-0 key, and the events to persist.
    pub fn create(secret: &SecretKey, name: &str) -> (Self, EpochKey, Vec<Event>) {
        let nonce: [u8; 16] = rand::random();
        let id = ServerId::derive(&secret.public(), &nonce);
        let key = crate::crypto::random_key();
        let mut state = Self::new(id);
        state.offer_key(0, key);
        let genesis = Event::sign(secret, id, 1, Body::Admin(AdminOp::Genesis { nonce, key_hash: key_hash(&key) }));
        let mut events = state.ingest(vec![genesis]).accepted;
        for content in [
            Content::Rename { name: name.to_string() },
            Content::CreateChannel { name: "general".into(), kind: ChannelKind::Text },
            Content::CreateChannel { name: "General".into(), kind: ChannelKind::Voice },
        ] {
            let e = state.author_content(secret, &content).expect("owner holds epoch 0 key");
            events.extend(state.ingest(vec![e]).accepted);
        }
        (state, key, events)
    }

    /// Marks this replica as a storage host: sealed events are validated and kept on disk only.
    pub fn set_blind(&mut self) {
        self.blind = true;
        self.locked.clear();
    }

    pub fn hosts(&self) -> &[PeerId] {
        &self.hosts
    }

    pub fn is_host(&self, peer: &PeerId) -> bool {
        self.hosts.contains(peer)
    }

    /// Everyone we exchange events with: members and hosts.
    pub fn sync_peers(&self) -> Vec<PeerId> {
        let mut out = self.members();
        out.extend(self.hosts.iter().filter(|h| !out.contains(h)).copied().collect::<Vec<_>>());
        out
    }

    pub fn owner(&self) -> Option<PeerId> {
        self.owner
    }

    pub fn epoch(&self) -> u32 {
        self.epochs.last().map(|e| e.1).unwrap_or(0)
    }

    pub fn have(&self) -> &BTreeMap<PeerId, u64> {
        &self.have
    }

    pub fn keys(&self) -> &BTreeMap<u32, EpochKey> {
        &self.keys
    }

    pub fn is_member(&self, peer: &PeerId) -> bool {
        self.member_at(peer, u64::MAX)
    }

    pub fn members(&self) -> Vec<PeerId> {
        let mut out: Vec<PeerId> = self.owner.into_iter().collect();
        out.extend(
            self.membership
                .iter()
                .filter(|(p, h)| Some(**p) != self.owner && matches!(h.last(), Some((_, Membership::Joined))))
                .map(|(p, _)| *p),
        );
        out
    }

    fn member_at(&self, peer: &PeerId, owner_seq: u64) -> bool {
        if Some(*peer) == self.owner {
            return true;
        }
        let Some(history) = self.membership.get(peer) else { return false };
        matches!(history.iter().rev().find(|(s, _)| *s <= owner_seq), Some((_, Membership::Joined)))
    }

    fn epoch_at(&self, owner_seq: u64) -> u32 {
        self.epochs.iter().rev().find(|(s, _, _)| *s <= owner_seq).map(|e| e.1).unwrap_or(0)
    }

    fn next_seq(&self, author: &PeerId) -> u64 {
        self.have.get(author).copied().unwrap_or(0) + 1
    }

    /// Signs an admin op as the owner. Callers ingest the result.
    pub fn author_admin(&self, secret: &SecretKey, op: AdminOp) -> Result<Event> {
        if self.owner != Some(secret.public()) {
            bail!("only the server owner can do that");
        }
        Ok(Event::sign(secret, self.id, self.next_seq(&secret.public()), Body::Admin(op)))
    }

    /// Seals `content` under the current epoch. Callers ingest the result.
    pub fn author_content(&self, secret: &SecretKey, content: &Content) -> Result<Event> {
        let me = secret.public();
        if !self.is_member(&me) {
            bail!("not a member of this server");
        }
        let epoch = self.epoch();
        let Some(key) = self.keys.get(&epoch) else {
            bail!("waiting for the key to epoch {epoch}");
        };
        let admin_seq = self.owner.and_then(|o| self.have.get(&o).copied()).unwrap_or(0);
        Ok(Event::seal(secret, self.id, self.next_seq(&me), epoch, admin_seq, key, content))
    }

    /// Offers an epoch key; it's installed once it matches the owner's commitment.
    pub fn offer_key(&mut self, epoch: u32, key: EpochKey) -> Vec<Change> {
        if self.keys.contains_key(&epoch) {
            return Vec::new();
        }
        self.unverified_keys.push((epoch, key));
        self.install_verified_keys()
    }

    fn install_verified_keys(&mut self) -> Vec<Change> {
        let mut installed = false;
        let epochs = &self.epochs;
        let keys = &mut self.keys;
        self.unverified_keys.retain(|(epoch, key)| {
            match epochs.iter().find(|(_, e, _)| e == epoch) {
                Some((_, _, hash)) if *hash == key_hash(key) => {
                    keys.insert(*epoch, *key);
                    installed = true;
                    false
                }
                Some(_) => false, // wrong key for a known epoch
                None => true,     // epoch not seen yet
            }
        });
        if !installed {
            return Vec::new();
        }
        let mut changes = Vec::new();
        for e in std::mem::take(&mut self.locked) {
            self.reveal(e, &mut changes);
        }
        changes
    }

    /// Validates and applies events in any order. Events that can't be judged
    /// yet (gaps, missing admin history) are held until they can.
    pub fn ingest(&mut self, events: Vec<Event>) -> Ingested {
        let mut out = Ingested::default();
        self.pending.extend(events);
        loop {
            let owner = self.owner;
            self.pending.sort_by_key(|e| (Some(e.author) != owner, e.author, e.seq));
            self.pending.dedup_by(|a, b| a.author == b.author && a.seq == b.seq);
            let mut progress = false;
            for e in std::mem::take(&mut self.pending) {
                match self.check(&e) {
                    Verdict::Accept => {
                        self.apply(e, &mut out);
                        progress = true;
                    }
                    Verdict::Wait => self.pending.push(e),
                    Verdict::Reject(why) => out.rejected.push((e.id(), why)),
                }
            }
            if !progress {
                break;
            }
        }
        if self.pending.len() > MAX_PENDING {
            self.pending.truncate(MAX_PENDING);
        }
        out
    }

    fn check(&self, e: &Event) -> Verdict {
        if e.server != self.id {
            return Verdict::Reject("wrong server");
        }
        let have = self.have.get(&e.author).copied().unwrap_or(0);
        if e.seq <= have {
            return Verdict::Reject("duplicate");
        }
        if e.seq > have + 1 {
            return Verdict::Wait;
        }
        if e.verify().is_err() {
            return Verdict::Reject("bad signature");
        }
        let Some(owner) = self.owner else {
            return match &e.body {
                Body::Admin(AdminOp::Genesis { nonce, .. })
                    if e.seq == 1 && ServerId::derive(&e.author, nonce) == self.id =>
                {
                    Verdict::Accept
                }
                _ => Verdict::Wait,
            };
        };
        match &e.body {
            Body::Admin(_) if e.author != owner => Verdict::Reject("admin op from non-owner"),
            Body::Admin(AdminOp::Genesis { .. }) => Verdict::Reject("second genesis"),
            Body::Admin(AdminOp::Add { peer } | AdminOp::Remove { peer, .. }) if *peer == owner => {
                Verdict::Reject("owner membership is fixed")
            }
            Body::Admin(AdminOp::Rotate { epoch, .. }) if *epoch != self.epoch() + 1 => {
                Verdict::Reject("epoch out of order")
            }
            Body::Admin(_) => Verdict::Accept,
            Body::Sealed { epoch, admin_seq, .. } => {
                if e.author == owner {
                    // The owner's seq is the admin clock, so its own history is authoritative.
                    return if *epoch == self.epoch_at(e.seq) { Verdict::Accept } else { Verdict::Reject("stale epoch") };
                }
                if self.have.get(&owner).copied().unwrap_or(0) < *admin_seq {
                    return Verdict::Wait;
                }
                if !self.member_at(&e.author, *admin_seq) {
                    return Verdict::Reject("author not a member");
                }
                let backdated = self.membership[&e.author].iter().any(|(s, m)| {
                    *s > *admin_seq && matches!(m, Membership::Removed { through_seq } if e.seq > *through_seq)
                });
                if backdated {
                    return Verdict::Reject("authored after removal");
                }
                if *epoch != self.epoch_at(*admin_seq) {
                    return Verdict::Reject("epoch does not match admin_seq");
                }
                Verdict::Accept
            }
        }
    }

    fn apply(&mut self, e: Event, out: &mut Ingested) {
        self.have.insert(e.author, e.seq);
        match &e.body {
            Body::Admin(op) => match op {
                AdminOp::Genesis { key_hash, .. } => {
                    self.owner = Some(e.author);
                    self.epochs.push((e.seq, 0, *key_hash));
                }
                AdminOp::Add { peer } => {
                    self.membership.entry(*peer).or_default().push((e.seq, Membership::Joined));
                    out.changes.push(Change::MemberAdded(*peer));
                }
                AdminOp::Remove { peer, through_seq } => {
                    let m = Membership::Removed { through_seq: *through_seq };
                    self.membership.entry(*peer).or_default().push((e.seq, m));
                    out.changes.push(Change::MemberRemoved(*peer));
                }
                AdminOp::AddHost { peer } => {
                    if !self.hosts.contains(peer) {
                        self.hosts.push(*peer);
                    }
                    out.changes.push(Change::HostAdded(*peer));
                }
                AdminOp::RemoveHost { peer } => {
                    self.hosts.retain(|h| h != peer);
                    out.changes.push(Change::HostRemoved(*peer));
                }
                AdminOp::Rotate { epoch, key_hash } => {
                    self.epochs.push((e.seq, *epoch, *key_hash));
                    out.changes.push(Change::Epoch(*epoch));
                }
            },
            Body::Sealed { .. } => self.reveal(e.clone(), &mut out.changes),
        }
        if matches!(e.body, Body::Admin(AdminOp::Genesis { .. } | AdminOp::Rotate { .. })) {
            out.changes.extend(self.install_verified_keys());
        }
        out.accepted.push(e);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_message(
        &mut self,
        id: EventId,
        channel: EventId,
        author: PeerId,
        body: String,
        sent_at_ms: u64,
        reply_to: Option<EventId>,
        attachments: Vec<Attachment>,
        changes: &mut Vec<Change>,
    ) {
        let msg = ChatMessage {
            id,
            channel,
            author,
            body,
            sent_at_ms,
            reply_to,
            attachments,
            edited: false,
            deleted: false,
            reactions: BTreeMap::new(),
        };
        let list = self.messages.entry(channel).or_default();
        let at = list.partition_point(|m| (m.sent_at_ms, m.id) <= (sent_at_ms, msg.id));
        list.insert(at, msg.clone());
        self.located.insert(id, channel);
        changes.push(Change::Message(msg));
        for (by, m) in self.pending_mods.remove(&id).unwrap_or_default() {
            self.modify(id, by, m, changes);
        }
    }

    /// Decrypts an accepted sealed event, or parks it until its key arrives.
    fn reveal(&mut self, e: Event, changes: &mut Vec<Change>) {
        let Body::Sealed { epoch, .. } = &e.body else { return };
        let Some(key) = self.keys.get(epoch) else {
            if !self.blind {
                self.locked.push(e);
            }
            return;
        };
        let Ok(Some(content)) = e.open(key) else { return };
        let from_owner = Some(e.author) == self.owner;
        match content {
            Content::Rename { name } if from_owner => {
                self.name = Some(name.clone());
                changes.push(Change::Renamed(name));
            }
            Content::CreateChannel { name, kind } if from_owner => {
                let ch = Channel { id: e.id(), name, kind };
                self.channels.insert(ch.id, ch.clone());
                changes.push(Change::Channel(ch));
            }
            Content::Message { channel, body, sent_at_ms } => {
                self.add_message(e.id(), channel, e.author, body, sent_at_ms, None, Vec::new(), changes);
            }
            Content::Post { channel, body, sent_at_ms, reply_to, attachments } => {
                self.add_message(e.id(), channel, e.author, body, sent_at_ms, reply_to, attachments, changes);
            }
            Content::Edit { target, body } => self.modify(target, e.author, Modify::Edit(body), changes),
            Content::Delete { target } => self.modify(target, e.author, Modify::Delete, changes),
            Content::React { target, emoji, on } => self.modify(target, e.author, Modify::React { emoji, on }, changes),
            Content::Leave if !from_owner => changes.push(Change::LeaveRequested(e.author)),
            _ => {} // owner-only content from a non-owner: replicated but ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_channel(s: &ServerState) -> EventId {
        s.channels.values().find(|c| c.kind == ChannelKind::Text).unwrap().id
    }

    fn say(s: &ServerState, who: &SecretKey, body: &str) -> Event {
        // Distinct, increasing timestamps so display order is deterministic.
        static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let sent_at_ms = CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let content = Content::Message { channel: text_channel(s), body: body.into(), sent_at_ms };
        s.author_content(who, &content).unwrap()
    }

    fn bodies(s: &ServerState) -> Vec<String> {
        s.messages.get(&text_channel(s)).map(|m| m.iter().map(|m| m.body.clone()).collect()).unwrap_or_default()
    }

    /// Owner, plus one replica that has received everything.
    fn setup() -> (SecretKey, ServerState, EpochKey, Vec<Event>) {
        let owner = SecretKey::generate();
        let (state, key, events) = ServerState::create(&owner, "test");
        (owner, state, key, events)
    }

    #[test]
    fn create_has_defaults() {
        let (owner, s, _, events) = setup();
        assert_eq!(s.name.as_deref(), Some("test"));
        assert_eq!(s.channels.len(), 2);
        assert_eq!(s.owner(), Some(owner.public()));
        assert_eq!(events.len(), 4);
    }

    #[test]
    fn replica_converges_from_shuffled_events_and_late_key() {
        let (owner, mut s, key, mut log) = setup();
        let bob = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::Add { peer: bob.public() }).unwrap();
        log.extend(s.ingest(vec![add]).accepted);
        let msg = say(&s, &owner, "hello");
        log.extend(s.ingest(vec![msg]).accepted);

        let mut replica = ServerState::new(s.id);
        log.reverse();
        assert!(replica.ingest(log).accepted.len() == 6);
        assert!(replica.channels.is_empty(), "nothing readable without the key");
        replica.offer_key(0, key);
        assert_eq!(bodies(&replica), ["hello"]);
        assert!(replica.is_member(&bob.public()));
    }

    #[test]
    fn non_member_and_forged_events_rejected() {
        let (owner, mut s, key, _) = setup();
        let mallory = SecretKey::generate();
        let channel = text_channel(&s);
        let content = Content::Message { channel, body: "spam".into(), sent_at_ms: 1 };
        let admin_seq = s.have()[&owner.public()];
        let spam = Event::seal(&mallory, s.id, 1, 0, admin_seq, &key, &content);
        assert_eq!(s.ingest(vec![spam]).rejected[0].1, "author not a member");

        let mut forged = say(&s, &owner, "real");
        if let Body::Sealed { ciphertext, .. } = &mut forged.body {
            ciphertext[0] ^= 1;
        }
        assert_eq!(s.ingest(vec![forged]).rejected[0].1, "bad signature");

        let fake_admin = Event::sign(&mallory, s.id, 1, Body::Admin(AdminOp::Add { peer: mallory.public() }));
        assert_eq!(s.ingest(vec![fake_admin]).rejected[0].1, "admin op from non-owner");
        assert!(!s.is_member(&mallory.public()));
    }

    #[test]
    fn removal_rotates_and_blocks_backdating() {
        let (owner, mut s, key0, _) = setup();
        let bob = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::Add { peer: bob.public() }).unwrap();
        s.ingest(vec![add]);
        let added_at = s.have()[&owner.public()];

        // Authoring only needs state that knows Bob is a member; the owner's will do.
        let hi = say(&s, &bob, "pre-removal");
        assert_eq!(s.ingest(vec![hi]).accepted.len(), 1);

        let through_seq = s.have()[&bob.public()];
        let remove = s.author_admin(&owner, AdminOp::Remove { peer: bob.public(), through_seq }).unwrap();
        s.ingest(vec![remove]);
        let key1 = crate::crypto::random_key();
        let rotate = s.author_admin(&owner, AdminOp::Rotate { epoch: 1, key_hash: key_hash(&key1) }).unwrap();
        s.ingest(vec![rotate]);
        s.offer_key(1, key1);
        assert_eq!(s.epoch(), 1);
        assert!(!s.is_member(&bob.public()));

        // Bob tries to post claiming he hadn't seen the removal yet.
        let channel = text_channel(&s);
        let content = Content::Message { channel, body: "backdated".into(), sent_at_ms: 2 };
        let backdated = Event::seal(&bob, s.id, through_seq + 1, 0, added_at, &key0, &content);
        assert_eq!(s.ingest(vec![backdated]).rejected[0].1, "authored after removal");

        // Owner keeps talking under epoch 1, which Bob's old key can't open.
        let post = say(&s, &owner, "after");
        assert!(matches!(post.body, Body::Sealed { epoch: 1, .. }));
        assert!(post.open(&key0).is_err());
        s.ingest(vec![post]);
        assert_eq!(bodies(&s), ["pre-removal", "after"]);
    }

    #[test]
    fn blind_host_replicates_without_reading() {
        let (owner, mut s, _, mut log) = setup();
        let node = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::AddHost { peer: node.public() }).unwrap();
        log.extend(s.ingest(vec![add]).accepted);
        let msg = say(&s, &owner, "for members only");
        log.extend(s.ingest(vec![msg]).accepted);

        let mut host = ServerState::new(s.id);
        host.set_blind();
        assert_eq!(host.ingest(log).accepted.len(), 6, "host validates and keeps everything");
        assert!(host.is_host(&node.public()));
        assert!(!host.is_member(&node.public()));
        assert!(host.channels.is_empty() && host.messages.is_empty());
        assert!(host.author_content(&node, &Content::Rename { name: "x".into() }).is_err());
        assert!(s.sync_peers().contains(&node.public()));
    }

    fn post(s: &ServerState, who: &SecretKey, content: Content) -> Event {
        s.author_content(who, &content).unwrap()
    }

    #[test]
    fn edits_deletes_and_reactions() {
        let (owner, mut s, _, _) = setup();
        let bob = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::Add { peer: bob.public() }).unwrap();
        s.ingest(vec![add]);
        let ch = text_channel(&s);
        let m = say(&s, &bob, "frist");
        let id = m.id();
        s.ingest(vec![m]);

        // Owner can't edit Bob's message; Bob can.
        s.ingest(vec![post(&s, &owner, Content::Edit { target: id, body: "hijacked".into() })]);
        assert_eq!(s.message(&id).unwrap().body, "frist");
        s.ingest(vec![post(&s, &bob, Content::Edit { target: id, body: "first".into() })]);
        assert!(s.message(&id).unwrap().edited && s.message(&id).unwrap().body == "first");

        // Reactions toggle per person; the emoji disappears when nobody's left.
        s.ingest(vec![post(&s, &owner, Content::React { target: id, emoji: "🔥".into(), on: true })]);
        s.ingest(vec![post(&s, &bob, Content::React { target: id, emoji: "🔥".into(), on: true })]);
        assert_eq!(s.message(&id).unwrap().reactions["🔥"].len(), 2);
        s.ingest(vec![post(&s, &bob, Content::React { target: id, emoji: "🔥".into(), on: false })]);
        s.ingest(vec![post(&s, &owner, Content::React { target: id, emoji: "🔥".into(), on: false })]);
        assert!(s.message(&id).unwrap().reactions.is_empty());

        // The owner can delete anyone's message.
        let out = s.ingest(vec![post(&s, &owner, Content::Delete { target: id })]);
        assert!(matches!(&out.changes[..], [Change::MessageUpdated(m)] if m.deleted && m.body.is_empty()));
        let _ = ch;
    }

    #[test]
    fn reaction_before_message_is_applied_on_arrival() {
        let (owner, mut s, key, mut log) = setup();
        let bob = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::Add { peer: bob.public() }).unwrap();
        log.extend(s.ingest(vec![add]).accepted);
        let ch = text_channel(&s);
        let attachment = Attachment {
            name: "cat.png".into(),
            mime: "image/png".into(),
            blob: crate::blob::BlobRef { hash: [1; 32], key: [2; 32], size: 3 },
        };
        // Bob posts; the owner reacts. Replicas process the owner's chain first,
        // so the reaction is seen before the message it targets.
        let m = post(&s, &bob, Content::Post { channel: ch, body: "look".into(), sent_at_ms: 5, reply_to: None, attachments: vec![attachment.clone()] });
        let id = m.id();
        log.extend(s.ingest(vec![m]).accepted);
        log.extend(s.ingest(vec![post(&s, &owner, Content::React { target: id, emoji: "😺".into(), on: true })]).accepted);

        let mut r = ServerState::new(s.id);
        r.offer_key(0, key);
        r.ingest(log);
        assert!(r.pending_mods.is_empty(), "held reaction was applied");
        let msg = r.message(&id).unwrap();
        assert_eq!(msg.reactions["😺"], [owner.public()]);
        assert_eq!(msg.attachments, [attachment]);
    }

    #[test]
    fn leave_is_surfaced_to_owner() {
        let (owner, mut s, _, _) = setup();
        let bob = SecretKey::generate();
        let add = s.author_admin(&owner, AdminOp::Add { peer: bob.public() }).unwrap();
        s.ingest(vec![add]);
        let out = s.ingest(vec![post(&s, &bob, Content::Leave)]);
        assert!(out.changes.contains(&Change::LeaveRequested(bob.public())));
    }

    #[test]
    fn wrong_key_offer_is_ignored() {
        let (_, s, _, log) = setup();
        let mut replica = ServerState::new(s.id);
        replica.ingest(log);
        replica.offer_key(0, crate::crypto::random_key());
        assert!(replica.keys().is_empty());
        assert!(replica.channels.is_empty());
    }
}
