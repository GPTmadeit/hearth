//! The engine: one task owns all state and storage; each connection gets a
//! reader task feeding the engine and a writer task draining a queue.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointId as PeerId, RelayMode, RelayUrl, SecretKey,
    endpoint::{Connection, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    crypto,
    event::{AdminOp, ChannelKind, Content, Event, EventId, ServerId},
    media::{MediaHub, RoomConfig, VoiceSession},
    proto::{self, ALPN, Frame},
    state::{Change, Channel, ChatMessage, ServerState, key_hash},
    store::Store,
};

const BATCH: usize = 128;
const MAX_BODY: usize = 4000;
const RECONNECT_EVERY: Duration = Duration::from_secs(30);

pub struct Config {
    pub secret: SecretKey,
    pub name: String,
    pub data_dir: PathBuf,
    /// Use this relay instead of the public n0 relays.
    pub relay: Option<RelayUrl>,
    /// Run as a storage host: accept hosting requests and keep servers blind.
    pub host: Option<HostPolicy>,
}

#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    /// Owners allowed to make us host their servers. Empty means anyone.
    pub allow: Vec<PeerId>,
}

/// Something the UI should know about.
#[derive(Debug, Clone)]
pub enum Update {
    PeerOnline { peer: PeerId, name: String },
    PeerOffline { peer: PeerId },
    Dm { peer: PeerId, body: String, sent_at_ms: u64 },
    Invited { server: ServerId, from: PeerId, name: String },
    Server { server: ServerId, change: Change },
    /// Who is in a voice channel now, and who among them is streaming.
    Voice { server: ServerId, channel: EventId, peers: Vec<PeerId>, live: Vec<PeerId> },
    /// Someone started watching our stream: the next video frame should be a keyframe.
    KeyframeRequested,
}

#[derive(Debug, Clone)]
pub struct ServerSummary {
    pub id: ServerId,
    pub name: Option<String>,
    pub owner: Option<PeerId>,
    pub members: Vec<PeerId>,
    pub hosts: Vec<PeerId>,
    pub channels: Vec<Channel>,
    pub epoch: u32,
}

type Call = Box<dyn FnOnce(&mut Engine) + Send>;

enum Inbox {
    Call(Call),
    Connected { peer: PeerId, name: String, conn: Connection, out: mpsc::UnboundedSender<Frame>, dialed: bool, id: u64 },
    Frame { peer: PeerId, frame: Frame },
    Closed { peer: PeerId, id: u64 },
    Tick,
}

/// Handle to a running engine. Cheap to clone.
#[derive(Clone)]
pub struct Hearth {
    inbox: mpsc::UnboundedSender<Inbox>,
    id: PeerId,
    endpoint: Endpoint,
}

impl Hearth {
    pub async fn start(cfg: Config) -> Result<(Self, mpsc::UnboundedReceiver<Update>)> {
        let store = Store::open(&cfg.data_dir.join("hearth.redb"))?;
        let mut builder = Endpoint::builder(presets::N0).secret_key(cfg.secret.clone());
        if let Some(url) = cfg.relay {
            builder = builder.relay_mode(RelayMode::Custom(url.into()));
        }
        let endpoint = builder.bind().await?;
        let (inbox, inbox_rx) = mpsc::unbounded_channel();
        let (updates, updates_rx) = mpsc::unbounded_channel();

        let media = Arc::new(MediaHub::default());
        let handler = Accept { name: cfg.name.clone(), inbox: inbox.clone(), media: media.clone() };
        let router = Router::builder(endpoint.clone()).accept(ALPN, handler).spawn();

        let mut engine = Engine {
            secret: cfg.secret,
            name: cfg.name,
            host: cfg.host,
            store,
            servers: HashMap::new(),
            persisted_keys: HashSet::new(),
            peers: HashMap::new(),
            dialing: HashSet::new(),
            media,
            voice: HashMap::new(),
            my_voice: None,
            watchers: HashSet::new(),
            updates,
            inbox: inbox.clone(),
            endpoint: endpoint.clone(),
            _router: router,
        };
        engine.load()?;
        tokio::spawn(engine.run(inbox_rx));

        let tick = inbox.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(RECONNECT_EVERY);
            loop {
                interval.tick().await;
                if tick.send(Inbox::Tick).is_err() {
                    break;
                }
            }
        });

        let id = endpoint.id();
        Ok((Self { inbox, id, endpoint }, updates_rx))
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    /// Waits until we're reachable through a relay, so peers can dial us by key alone.
    pub async fn online(&self) {
        self.endpoint.online().await;
    }

    async fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut Engine) -> R + Send + 'static) -> Result<R> {
        let (tx, rx) = oneshot::channel();
        let call: Call = Box::new(move |e| {
            let _ = tx.send(f(e));
        });
        self.inbox.send(Inbox::Call(call)).ok().context("engine stopped")?;
        rx.await.context("engine stopped")
    }

    pub async fn dial(&self, peer: PeerId) -> Result<()> {
        self.call(move |e| e.dial(peer)).await
    }

    pub async fn create_server(&self, name: String) -> Result<ServerId> {
        self.call(move |e| e.create_server(&name)).await?
    }

    /// Starts syncing `server`, fetching it from `via` (who must have invited us).
    pub async fn join(&self, server: ServerId, via: PeerId) -> Result<()> {
        self.call(move |e| e.join(server, via)).await?
    }

    pub async fn invite(&self, server: ServerId, peer: PeerId) -> Result<()> {
        self.call(move |e| e.invite(server, peer)).await?
    }

    /// Asks a storage node to host `server`'s ciphertext.
    pub async fn add_host(&self, server: ServerId, peer: PeerId) -> Result<()> {
        self.call(move |e| e.add_host(server, peer)).await?
    }

    pub async fn remove_host(&self, server: ServerId, peer: PeerId) -> Result<()> {
        self.call(move |e| e.remove_host(server, peer)).await?
    }

    pub async fn kick(&self, server: ServerId, peer: PeerId) -> Result<()> {
        self.call(move |e| e.kick(server, peer)).await?
    }

    pub async fn create_channel(&self, server: ServerId, name: String, kind: ChannelKind) -> Result<EventId> {
        self.call(move |e| e.create_channel(server, name, kind)).await?
    }

    pub async fn send(&self, server: ServerId, channel: EventId, body: String) -> Result<()> {
        self.call(move |e| e.send_message(server, channel, body)).await?
    }

    pub async fn dm(&self, peer: PeerId, body: String) -> Result<()> {
        self.call(move |e| e.dm(peer, body)).await?
    }

    pub async fn servers(&self) -> Result<Vec<ServerSummary>> {
        self.call(|e| e.summaries()).await
    }

    pub async fn history(&self, server: ServerId, channel: EventId, limit: usize) -> Result<Vec<ChatMessage>> {
        self.call(move |e| {
            let list = e.servers.get(&server).and_then(|s| s.messages.get(&channel));
            list.map(|l| l[l.len().saturating_sub(limit)..].to_vec()).unwrap_or_default()
        })
        .await
    }

    /// Joins a voice channel (leaving any other). Media flows through the returned session.
    pub async fn voice_join(&self, server: ServerId, channel: EventId) -> Result<VoiceSession> {
        self.call(move |e| e.voice_join(server, channel)).await?
    }

    pub async fn voice_leave(&self) -> Result<()> {
        self.call(|e| e.voice_leave()).await
    }

    /// Starts or stops announcing that we're streaming our screen.
    pub async fn set_live(&self, live: bool) -> Result<()> {
        self.call(move |e| e.set_live(live)).await?
    }

    /// Asks `streamer` (in our voice channel) to start or stop sending us their stream.
    pub async fn watch(&self, streamer: PeerId, on: bool) -> Result<()> {
        self.call(move |e| e.watch(streamer, on)).await?
    }

    /// Who's in each voice channel of `server` right now.
    pub async fn voice_state(&self, server: ServerId) -> Result<HashMap<EventId, Vec<PeerId>>> {
        self.call(move |e| {
            let mut out: HashMap<EventId, Vec<PeerId>> = HashMap::new();
            for (p, slot) in e.voice.get(&server).into_iter().flatten() {
                out.entry(slot.channel).or_default().push(*p);
            }
            out
        })
        .await
    }

    /// Every display name we've seen, including peers who are offline now.
    pub async fn known_names(&self) -> Result<Vec<(PeerId, String)>> {
        self.call(|e| e.store.names().unwrap_or_default()).await
    }

    /// Names of peers currently connected.
    pub async fn online_peers(&self) -> Result<Vec<(PeerId, String)>> {
        self.call(|e| e.peers.iter().map(|(p, c)| (*p, c.name.clone())).collect()).await
    }

    pub async fn shutdown(&self) {
        let _ = self
            .call(|e| {
                for p in e.peers.values() {
                    p.conn.close(0u32.into(), b"bye");
                }
            })
            .await;
        self.endpoint.close().await;
    }
}

struct PeerConn {
    name: String,
    conn: Connection,
    out: mpsc::UnboundedSender<Frame>,
    dialed: bool,
    id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VoiceSlot {
    channel: EventId,
    live: bool,
}

struct Engine {
    secret: SecretKey,
    name: String,
    host: Option<HostPolicy>,
    store: Store,
    servers: HashMap<ServerId, ServerState>,
    persisted_keys: HashSet<(ServerId, u32)>,
    peers: HashMap<PeerId, PeerConn>,
    dialing: HashSet<PeerId>,
    media: Arc<MediaHub>,
    /// Per server: which voice channel each connected member is in.
    voice: HashMap<ServerId, HashMap<PeerId, VoiceSlot>>,
    my_voice: Option<(ServerId, EventId)>,
    /// Peers watching our screen stream.
    watchers: HashSet<PeerId>,
    updates: mpsc::UnboundedSender<Update>,
    inbox: mpsc::UnboundedSender<Inbox>,
    endpoint: Endpoint,
    _router: Router,
}

impl Engine {
    fn me(&self) -> PeerId {
        self.secret.public()
    }

    fn new_state(&self, id: ServerId) -> ServerState {
        let mut s = ServerState::new(id);
        if self.host.is_some() {
            s.set_blind();
        }
        s
    }

    fn load(&mut self) -> Result<()> {
        for id in self.store.servers()? {
            let mut state = self.new_state(id);
            for (epoch, key) in self.store.keys(id)? {
                state.offer_key(epoch, key);
                self.persisted_keys.insert((id, epoch));
            }
            state.ingest(self.store.events(id)?);
            self.servers.insert(id, state);
        }
        Ok(())
    }

    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Inbox>) {
        self.reconnect();
        while let Some(msg) = inbox.recv().await {
            match msg {
                Inbox::Call(f) => f(&mut self),
                Inbox::Connected { peer, name, conn, out, dialed, id } => {
                    self.on_connected(peer, PeerConn { name, conn, out, dialed, id })
                }
                Inbox::Frame { peer, frame } => {
                    if let Err(e) = self.on_frame(peer, frame) {
                        eprintln!("frame from {}: {e:#}", peer.fmt_short());
                    }
                }
                Inbox::Closed { peer, id } => {
                    if self.peers.get(&peer).is_some_and(|p| p.id == id) {
                        self.peers.remove(&peer);
                        self.emit(Update::PeerOffline { peer });
                        self.set_voice(None, peer, None);
                    }
                }
                Inbox::Tick => self.reconnect(),
            }
        }
    }

    fn emit(&self, u: Update) {
        let _ = self.updates.send(u);
    }

    fn send_to(&self, peer: &PeerId, frame: Frame) {
        if let Some(p) = self.peers.get(peer) {
            let _ = p.out.send(frame);
        }
    }

    // ---- connections ----

    fn dial(&mut self, peer: PeerId) {
        if peer == self.me() || self.peers.contains_key(&peer) || !self.dialing.insert(peer) {
            return;
        }
        let (endpoint, inbox, name) = (self.endpoint.clone(), self.inbox.clone(), self.name.clone());
        let media = self.media.clone();
        tokio::spawn(async move {
            let result = async {
                let conn = endpoint.connect(peer, ALPN).await?;
                let (send, recv) = conn.open_bi().await?;
                anyhow::Ok((conn, send, recv))
            }
            .await;
            let inbox2 = inbox.clone();
            let _ = inbox.send(Inbox::Call(Box::new(move |e| {
                e.dialing.remove(&peer);
            })));
            if let Ok((conn, send, recv)) = result {
                let _ = drive(conn, send, recv, name, true, inbox2, media).await;
            }
        });
    }

    /// Keeps a connection to every member of every server we're in.
    fn reconnect(&mut self) {
        let wanted: HashSet<PeerId> = self.servers.values().flat_map(|s| s.sync_peers()).collect();
        for peer in wanted {
            self.dial(peer);
        }
    }

    fn on_connected(&mut self, peer: PeerId, new: PeerConn) {
        // If both sides dialed at once, both keep the one dialed by the smaller id.
        let preferred_dialed = self.me() < peer;
        if let Some(old) = self.peers.get(&peer) {
            if new.dialed != preferred_dialed && old.dialed == preferred_dialed {
                new.conn.close(0u32.into(), b"duplicate");
                return;
            }
            old.conn.close(0u32.into(), b"duplicate");
        }
        let name = new.name.clone();
        if let Err(e) = self.store.put_name(peer, &name) {
            eprintln!("saving name: {e:#}");
        }
        let was_online = self.peers.insert(peer, new).is_some();
        if !was_online {
            self.emit(Update::PeerOnline { peer, name });
        }
        let ids: Vec<ServerId> = self.servers.keys().copied().collect();
        for id in ids {
            let s = &self.servers[&id];
            // Share a server's existence only with its members, or with whoever we're joining through.
            if s.is_member(&peer) || s.is_host(&peer) || s.owner().is_none() {
                self.send_have(&peer, id);
            }
        }
        if let Some((server, channel)) = self.my_voice
            && self.servers[&server].is_member(&peer)
        {
            let me = self.me();
            let live = self.voice.get(&server).and_then(|r| r.get(&me)).is_some_and(|s| s.live);
            self.send_to(&peer, Frame::Voice { server, channel: Some(channel), live });
        }
        self.refresh_media();
    }

    fn send_have(&self, peer: &PeerId, id: ServerId) {
        let s = &self.servers[&id];
        let have = s.have().iter().map(|(a, n)| (*a, *n)).collect();
        let epochs = s.keys().keys().copied().collect();
        self.send_to(peer, Frame::Have { server: id, have, epochs });
    }

    // ---- inbound ----

    fn on_frame(&mut self, peer: PeerId, frame: Frame) -> Result<()> {
        match frame {
            Frame::Hello { .. } => {}
            Frame::Dm { body, sent_at_ms } => self.emit(Update::Dm { peer, body, sent_at_ms }),
            Frame::Invite { server, name } => {
                if self.servers.contains_key(&server) {
                    return Ok(());
                }
                match &self.host {
                    Some(policy) if policy.allow.is_empty() || policy.allow.contains(&peer) => {
                        self.join(server, peer)?;
                        self.emit(Update::Invited { server, from: peer, name });
                    }
                    Some(_) => {}
                    None => self.emit(Update::Invited { server, from: peer, name }),
                }
            }
            Frame::Voice { server, channel, live } => {
                if self.servers.get(&server).is_some_and(|s| s.is_member(&peer)) {
                    self.set_voice(Some(server), peer, channel.map(|channel| VoiceSlot { channel, live }));
                }
            }
            Frame::Watch { server, on } => self.on_watch(peer, server, on),
            Frame::Removed { server } => {
                let me = self.me();
                if self.servers.get(&server).is_some_and(|s| s.owner() == Some(peer)) {
                    self.emit(Update::Server { server, change: Change::MemberRemoved(me) });
                }
            }
            Frame::Have { server, have, epochs } => self.on_have(peer, server, have, epochs)?,
            Frame::Events(events) => {
                let mut by_server: HashMap<ServerId, Vec<Event>> = HashMap::new();
                for e in events {
                    by_server.entry(e.server).or_default().push(e);
                }
                for (server, events) in by_server {
                    if self.servers.contains_key(&server) {
                        self.ingest(server, events, Some(peer))?;
                    }
                }
            }
            Frame::Key { server, epoch, key } => {
                if let Some(s) = self.servers.get_mut(&server) {
                    let changes = s.offer_key(epoch, key);
                    self.after_changes(server, changes)?;
                }
            }
        }
        Ok(())
    }

    fn on_have(&mut self, peer: PeerId, server: ServerId, theirs: Vec<(PeerId, u64)>, epochs: Vec<u32>) -> Result<()> {
        let Some(s) = self.servers.get(&server) else { return Ok(()) };
        if !s.is_member(&peer) && !s.is_host(&peer) {
            return Ok(());
        }
        let theirs: HashMap<PeerId, u64> = theirs.into_iter().collect();

        // Owner's events first: everything else is judged against them.
        let mut authors: Vec<(PeerId, u64)> = s.have().iter().map(|(a, n)| (*a, *n)).collect();
        authors.sort_by_key(|(a, _)| Some(*a) != s.owner());
        let mut missing = Vec::new();
        for (author, ours) in &authors {
            let have = theirs.get(author).copied().unwrap_or(0);
            if *ours > have {
                missing.extend(self.store.events_after(server, *author, have)?);
            }
        }
        for chunk in missing.chunks(BATCH) {
            self.send_to(&peer, Frame::Events(chunk.to_vec()));
        }
        // Keys go to members only. Hosts sync ciphertext and must never be able to read it.
        for (epoch, key) in s.keys().iter().filter(|_| s.is_member(&peer)) {
            if !epochs.contains(epoch) {
                self.send_to(&peer, Frame::Key { server, epoch: *epoch, key: *key });
            }
        }

        let they_have_more = theirs.iter().any(|(a, n)| *n > s.have().get(a).copied().unwrap_or(0));
        if they_have_more {
            self.send_have(&peer, server);
        }
        Ok(())
    }

    /// Validates, persists, announces, and forwards events. `from` is excluded from forwarding.
    fn ingest(&mut self, server: ServerId, events: Vec<Event>, from: Option<PeerId>) -> Result<()> {
        let s = self.servers.get_mut(&server).expect("caller checked");
        let me = self.secret.public();
        let was_in = s.is_member(&me) || s.is_host(&me);
        let out = s.ingest(events);
        self.store.put_events(&out.accepted)?;

        if !out.accepted.is_empty() {
            let s = &self.servers[&server];
            for member in s.sync_peers() {
                if Some(member) != from && member != self.me() {
                    for chunk in out.accepted.chunks(BATCH) {
                        self.send_to(&member, Frame::Events(chunk.to_vec()));
                    }
                }
            }
        }
        let s = &self.servers[&server];
        if (s.is_member(&me) || s.is_host(&me)) && !was_in {
            // We just learned we're in: catch up with everyone else in it.
            self.reconnect();
        }
        self.after_changes(server, out.changes)
    }

    fn after_changes(&mut self, server: ServerId, changes: Vec<Change>) -> Result<()> {
        let s = &self.servers[&server];
        for (epoch, key) in s.keys() {
            if self.persisted_keys.insert((server, *epoch)) {
                self.store.put_key(server, *epoch, key)?;
            }
        }
        for change in changes {
            if let Change::MemberAdded(peer) | Change::HostAdded(peer) = &change {
                self.dial(*peer);
            }
            if let Change::MemberRemoved(peer) = &change {
                if *peer == self.me() {
                    self.voice_leave();
                }
                self.set_voice(Some(server), *peer, None);
            }
            if let Change::Epoch(_) = &change {
                self.refresh_media();
            }
            self.emit(Update::Server { server, change });
        }
        Ok(())
    }

    // ---- local actions ----

    fn create_server(&mut self, name: &str) -> Result<ServerId> {
        let (state, key, events) = ServerState::create(&self.secret, name);
        let id = state.id;
        self.store.add_server(id)?;
        self.store.put_events(&events)?;
        self.store.put_key(id, 0, &key)?;
        self.persisted_keys.insert((id, 0));
        self.servers.insert(id, state);
        Ok(id)
    }

    fn join(&mut self, server: ServerId, via: PeerId) -> Result<()> {
        if !self.servers.contains_key(&server) {
            self.store.add_server(server)?;
            let state = self.new_state(server);
            self.servers.insert(server, state);
        }
        if self.peers.contains_key(&via) {
            self.send_have(&via, server);
        } else {
            self.dial(via);
        }
        Ok(())
    }

    fn server(&self, id: ServerId) -> Result<&ServerState> {
        self.servers.get(&id).context("unknown server")
    }

    fn author_admin(&mut self, server: ServerId, op: AdminOp) -> Result<()> {
        let e = self.server(server)?.author_admin(&self.secret, op)?;
        self.ingest(server, vec![e], None)
    }

    fn author_content(&mut self, server: ServerId, content: Content) -> Result<EventId> {
        let e = self.server(server)?.author_content(&self.secret, &content)?;
        let id = e.id();
        self.ingest(server, vec![e], None)?;
        Ok(id)
    }

    fn invite(&mut self, server: ServerId, peer: PeerId) -> Result<()> {
        if !self.server(server)?.is_member(&peer) {
            self.author_admin(server, AdminOp::Add { peer })?;
        }
        self.deliver_invite(server, peer)
    }

    /// Hosting is invite-shaped too: the node joins through whoever asked it.
    fn add_host(&mut self, server: ServerId, peer: PeerId) -> Result<()> {
        if !self.server(server)?.is_host(&peer) {
            self.author_admin(server, AdminOp::AddHost { peer })?;
        }
        self.deliver_invite(server, peer)
    }

    fn remove_host(&mut self, server: ServerId, peer: PeerId) -> Result<()> {
        if !self.server(server)?.is_host(&peer) {
            bail!("not a host of this server");
        }
        self.author_admin(server, AdminOp::RemoveHost { peer })
    }

    fn deliver_invite(&mut self, server: ServerId, peer: PeerId) -> Result<()> {
        let name = self.server(server)?.name.clone().unwrap_or_default();
        if self.peers.contains_key(&peer) {
            self.send_to(&peer, Frame::Invite { server, name });
        } else {
            // Deliver the invite once they're reachable.
            self.dial(peer);
            let inbox = self.inbox.clone();
            tokio::spawn(async move {
                for _ in 0..20 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let name = name.clone();
                    let (tx, rx) = oneshot::channel();
                    let call: Call = Box::new(move |e| {
                        let sent = e.peers.contains_key(&peer);
                        if sent {
                            e.send_to(&peer, Frame::Invite { server, name });
                        }
                        let _ = tx.send(sent);
                    });
                    if inbox.send(Inbox::Call(call)).is_err() || rx.await.unwrap_or(true) {
                        break;
                    }
                }
            });
        }
        Ok(())
    }

    fn kick(&mut self, server: ServerId, peer: PeerId) -> Result<()> {
        let s = self.server(server)?;
        if !s.is_member(&peer) {
            bail!("not a member");
        }
        let through_seq = s.have().get(&peer).copied().unwrap_or(0);
        let epoch = s.epoch() + 1;
        self.author_admin(server, AdminOp::Remove { peer, through_seq })?;
        self.send_to(&peer, Frame::Removed { server });

        let key = crypto::random_key();
        self.author_admin(server, AdminOp::Rotate { epoch, key_hash: key_hash(&key) })?;
        let changes = self.servers.get_mut(&server).expect("checked").offer_key(epoch, key);
        self.after_changes(server, changes)?;
        for member in self.servers[&server].members() {
            self.send_to(&member, Frame::Key { server, epoch, key });
        }
        Ok(())
    }

    fn create_channel(&mut self, server: ServerId, name: String, kind: ChannelKind) -> Result<EventId> {
        if self.server(server)?.owner() != Some(self.me()) {
            bail!("only the server owner can create channels");
        }
        self.author_content(server, Content::CreateChannel { name, kind })
    }

    fn send_message(&mut self, server: ServerId, channel: EventId, body: String) -> Result<()> {
        if body.chars().count() > MAX_BODY {
            bail!("message longer than {MAX_BODY} characters");
        }
        if !self.server(server)?.channels.contains_key(&channel) {
            bail!("unknown channel");
        }
        self.author_content(server, Content::Message { channel, body, sent_at_ms: now_ms() })?;
        Ok(())
    }

    fn voice_join(&mut self, server: ServerId, channel: EventId) -> Result<VoiceSession> {
        let s = self.server(server)?;
        if !s.is_member(&self.me()) {
            bail!("not a member of this server");
        }
        match s.channels.get(&channel) {
            Some(c) if c.kind == ChannelKind::Voice => {}
            Some(_) => bail!("that's a text channel"),
            None => bail!("unknown channel"),
        }
        self.voice_leave();
        let (tx, rx) = std::sync::mpsc::channel();
        self.media.set_sink(Some(tx));
        self.my_voice = Some((server, channel));
        let me = self.me();
        self.set_voice(Some(server), me, Some(VoiceSlot { channel, live: false }));
        Ok(VoiceSession { hub: self.media.clone(), incoming: rx })
    }

    fn voice_leave(&mut self) {
        let Some((server, _)) = self.my_voice.take() else { return };
        self.media.set_sink(None);
        self.watchers.clear();
        let me = self.me();
        self.set_voice(Some(server), me, None);
    }

    fn set_live(&mut self, live: bool) -> Result<()> {
        let (server, channel) = self.my_voice.context("join a voice channel first")?;
        if !live {
            self.watchers.clear();
        }
        let me = self.me();
        self.set_voice(Some(server), me, Some(VoiceSlot { channel, live }));
        Ok(())
    }

    fn watch(&mut self, streamer: PeerId, on: bool) -> Result<()> {
        let (server, channel) = self.my_voice.context("join the voice channel first")?;
        let slot = self.voice.get(&server).and_then(|r| r.get(&streamer)).copied();
        if on && !slot.is_some_and(|s| s.channel == channel && s.live) {
            bail!("{} isn't streaming in this channel", streamer.fmt_short());
        }
        self.send_to(&streamer, Frame::Watch { server, on });
        Ok(())
    }

    fn on_watch(&mut self, viewer: PeerId, server: ServerId, on: bool) {
        let Some((my_server, channel)) = self.my_voice else { return };
        let me = self.me();
        let live = self.voice.get(&my_server).and_then(|r| r.get(&me)).is_some_and(|s| s.live);
        let in_room = self.voice.get(&my_server).and_then(|r| r.get(&viewer)).is_some_and(|s| s.channel == channel);
        if my_server != server || !live || !in_room {
            return;
        }
        let changed = if on { self.watchers.insert(viewer) } else { self.watchers.remove(&viewer) };
        if changed {
            self.refresh_media();
            if on {
                self.emit(Update::KeyframeRequested);
            }
        }
    }

    /// Records `peer`'s voice slot in `server` (or in every server when
    /// `None`), tells the UI, and for our own moves tells other members.
    fn set_voice(&mut self, server: Option<ServerId>, peer: PeerId, slot: Option<VoiceSlot>) {
        let servers: Vec<ServerId> = match server {
            Some(s) => vec![s],
            None => self.voice.keys().copied().collect(),
        };
        for sid in servers {
            let rooms = self.voice.entry(sid).or_default();
            let before = rooms.get(&peer).copied();
            if before == slot {
                continue;
            }
            match slot {
                Some(s) => rooms.insert(peer, s),
                None => rooms.remove(&peer),
            };
            if slot.is_none_or(|s| !s.live) {
                self.watchers.remove(&peer);
            }
            let rooms = &self.voice[&sid];
            let mut channels: Vec<EventId> = [before, slot].into_iter().flatten().map(|s| s.channel).collect();
            channels.dedup();
            for c in channels {
                let here = rooms.iter().filter(|(_, s)| s.channel == c);
                let peers = here.clone().map(|(p, _)| *p).collect();
                let live = here.filter(|(_, s)| s.live).map(|(p, _)| *p).collect();
                let _ = self.updates.send(Update::Voice { server: sid, channel: c, peers, live });
            }
            if peer == self.me()
                && let Some(s) = self.servers.get(&sid)
            {
                let frame = Frame::Voice { server: sid, channel: slot.map(|s| s.channel), live: slot.is_some_and(|s| s.live) };
                for m in s.members() {
                    self.send_to(&m, frame.clone());
                }
            }
        }
        self.refresh_media();
    }

    /// Points the media hub at the members currently sharing our voice channel.
    fn refresh_media(&self) {
        let Some((sid, channel)) = self.my_voice else {
            self.media.configure(None);
            return;
        };
        let Some(s) = self.servers.get(&sid) else { return };
        let me = self.me();
        let peers = self
            .voice
            .get(&sid)
            .into_iter()
            .flatten()
            .filter(|(p, slot)| slot.channel == channel && **p != me && s.is_member(p))
            .filter_map(|(p, _)| self.peers.get(p).map(|pc| (*p, pc.conn.clone())))
            .collect();
        self.media.configure(Some(RoomConfig {
            server: sid,
            channel,
            me,
            epoch: s.epoch(),
            keys: s.keys().clone(),
            peers,
            watchers: self.watchers.iter().copied().collect(),
        }));
    }

    fn dm(&mut self, peer: PeerId, body: String) -> Result<()> {
        if !self.peers.contains_key(&peer) {
            self.dial(peer);
            bail!("{} is not connected (dialing now, try again shortly)", peer.fmt_short());
        }
        self.send_to(&peer, Frame::Dm { body, sent_at_ms: now_ms() });
        Ok(())
    }

    fn summaries(&self) -> Vec<ServerSummary> {
        let mut out: Vec<ServerSummary> = self
            .servers
            .values()
            .map(|s| ServerSummary {
                id: s.id,
                name: s.name.clone(),
                owner: s.owner(),
                members: s.members(),
                hosts: s.hosts().to_vec(),
                channels: s.channels.values().cloned().collect(),
                epoch: s.epoch(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        out
    }
}

#[derive(Debug, Clone)]
struct Accept {
    name: String,
    inbox: mpsc::UnboundedSender<Inbox>,
    media: Arc<MediaHub>,
}

impl ProtocolHandler for Accept {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (send, recv) = conn.accept_bi().await?;
        let _ = drive(conn, send, recv, self.name.clone(), false, self.inbox.clone(), self.media.clone()).await;
        Ok(())
    }
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Runs one connection: hello exchange, then a writer task and a read loop.
async fn drive(
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    our_name: String,
    dialed: bool,
    inbox: mpsc::UnboundedSender<Inbox>,
    media: Arc<MediaHub>,
) -> Result<()> {
    let peer = conn.remote_id();
    proto::write(&mut send, &Frame::Hello { name: our_name }).await?;
    let name = match proto::read(&mut recv).await? {
        Some(Frame::Hello { name }) => name.chars().take(64).collect(),
        other => bail!("expected Hello, got {other:?}"),
    };

    let datagrams = conn.clone();
    let dmedia = media.clone();
    tokio::spawn(async move {
        while let Ok(data) = datagrams.read_datagram().await {
            dmedia.receive(peer, &data);
        }
    });
    // Each video frame arrives on its own unidirectional stream.
    let streams = conn.clone();
    tokio::spawn(async move {
        while let Ok(mut recv) = streams.accept_uni().await {
            let media = media.clone();
            tokio::spawn(async move {
                if let Ok(data) = recv.read_to_end(crate::media::MAX_VIDEO_FRAME + 1024).await {
                    media.receive(peer, &data);
                }
            });
        }
    });

    let id = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let (out, mut out_rx) = mpsc::unbounded_channel::<Frame>();
    tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if proto::write(&mut send, &frame).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });
    inbox.send(Inbox::Connected { peer, name, conn: conn.clone(), out, dialed, id }).ok().context("engine stopped")?;

    let result = async {
        while let Some(frame) = proto::read(&mut recv).await? {
            inbox.send(Inbox::Frame { peer, frame }).ok().context("engine stopped")?;
        }
        anyhow::Ok(())
    }
    .await;
    let _ = inbox.send(Inbox::Closed { peer, id });
    result
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
