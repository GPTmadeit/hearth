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
    blob::{self, BlobRef, BlobStore},
    crypto,
    event::{AdminOp, Attachment, ChannelKind, Content, Event, EventId, ServerId},
    link::{InviteCode, Link as ShareLink},
    media::{MediaHub, RoomConfig, VoiceSession},
    proto::{self, ALPN, Frame},
    state::{Change, Channel, ChatMessage, ServerState, key_hash},
    store::{FriendRecord, FriendState, Store},
};

const BATCH: usize = 128;
const MAX_BODY: usize = 4000;
const MAX_ATTACHMENTS: usize = 10;
const BLOB_TIMEOUT: Duration = Duration::from_secs(120);
const RECONNECT_EVERY: Duration = Duration::from_secs(30);
/// Store key for the invite codes we've made.
const INVITE_CODES: &str = "invite-codes";

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
    /// Who is in a voice channel now, and who among them is streaming,
    /// has their camera on, or is muted/deafened.
    Voice {
        server: ServerId,
        channel: EventId,
        peers: Vec<PeerId>,
        live: Vec<PeerId>,
        camera: Vec<PeerId>,
        muted: Vec<PeerId>,
        deafened: Vec<PeerId>,
    },
    /// Someone new needs a picture from us: the next screen (`camera: false`)
    /// or camera (`camera: true`) frame should be a keyframe.
    KeyframeRequested { camera: bool },
    /// A member in voice started or stopped talking (including us).
    Speaking { server: ServerId, peer: PeerId, speaking: bool },
    Typing { server: ServerId, channel: EventId, peer: PeerId },
    /// A friendship changed; `request` is set when someone just asked to be friends.
    Friend { peer: PeerId, request: bool },
    /// A peer's name or avatar changed.
    Profile { peer: PeerId },
    /// We left (or were removed from) a server and dropped our copy.
    ServerRemoved { server: ServerId },
    /// An invite code we used was accepted; the server is syncing in.
    Joined { server: ServerId },
}

#[derive(Debug, Clone)]
pub struct FriendView {
    pub peer: PeerId,
    pub state: FriendState,
    pub since_ms: u64,
    pub dm: Option<ServerId>,
    pub online: bool,
}

#[derive(Debug, Clone)]
pub struct VoiceRoom {
    pub server: ServerId,
    pub channel: EventId,
    pub peers: Vec<PeerId>,
    pub live: Vec<PeerId>,
    pub camera: Vec<PeerId>,
    pub muted: Vec<PeerId>,
    pub deafened: Vec<PeerId>,
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
    /// Set for a direct-message conversation with this friend.
    pub dm: Option<PeerId>,
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
    blobs: Arc<BlobStore>,
}

impl Hearth {
    pub async fn start(cfg: Config) -> Result<(Self, mpsc::UnboundedReceiver<Update>)> {
        let store = Store::open(&cfg.data_dir.join("hearth.redb"))?;
        let blobs = Arc::new(BlobStore::open(&cfg.data_dir.join("blobs"))?);
        let mut builder = Endpoint::builder(presets::N0).secret_key(cfg.secret.clone());
        if let Some(url) = cfg.relay {
            builder = builder.relay_mode(RelayMode::Custom(url.into()));
        }
        let endpoint = builder.bind().await?;
        let (inbox, inbox_rx) = mpsc::unbounded_channel();
        let (updates, updates_rx) = mpsc::unbounded_channel();

        let media = Arc::new(MediaHub::default());
        let handler = Accept(Shared { name: cfg.name.clone(), inbox: inbox.clone(), media: media.clone(), blobs: blobs.clone() });
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
            speaking: false,
            my_flags: (false, false),
            blobs: blobs.clone(),
            friends: HashMap::new(),
            dms: HashMap::new(),
            avatars: HashMap::new(),
            my_avatar: None,
            invite_codes: HashMap::new(),
            join_requests: HashSet::new(),
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
        Ok((Self { inbox, id, endpoint, blobs }, updates_rx))
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

    /// Starts or stops announcing that our camera is on.
    pub async fn set_camera(&self, on: bool) -> Result<()> {
        self.call(move |e| e.update_my_slot(|s| s.camera = on)).await?
    }

    /// Tells others in the call whether we're muted or deafened (for their indicators).
    pub async fn set_voice_flags(&self, muted: bool, deafened: bool) -> Result<()> {
        self.call(move |e| {
            e.my_flags = (muted, deafened);
            e.update_my_slot(|s| {
                s.muted = muted;
                s.deafened = deafened;
            })
            .or(Ok(()))
        })
        .await?
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

    /// Every voice channel with someone in it, across all servers.
    pub async fn voice_snapshot(&self) -> Result<Vec<VoiceRoom>> {
        self.call(|e| e.voice_rooms()).await
    }

    /// Tells the server whether we're talking, for speaking indicators everywhere.
    pub async fn set_speaking(&self, speaking: bool) -> Result<()> {
        self.call(move |e| e.set_speaking(speaking)).await
    }

    pub async fn typing(&self, server: ServerId, channel: EventId) -> Result<()> {
        self.call(move |e| e.typing(server, channel)).await
    }

    /// Posts a message, optionally replying and with attachments made by [`Hearth::upload`].
    pub async fn post(
        &self,
        server: ServerId,
        channel: EventId,
        body: String,
        reply_to: Option<EventId>,
        attachments: Vec<Attachment>,
    ) -> Result<EventId> {
        self.call(move |e| e.post(server, channel, body, reply_to, attachments)).await?
    }

    pub async fn edit(&self, server: ServerId, target: EventId, body: String) -> Result<()> {
        self.call(move |e| e.edit(server, target, body)).await?
    }

    pub async fn delete(&self, server: ServerId, target: EventId) -> Result<()> {
        self.call(move |e| e.delete(server, target)).await?
    }

    pub async fn react(&self, server: ServerId, target: EventId, emoji: String, on: bool) -> Result<()> {
        self.call(move |e| e.react(server, target, emoji, on)).await?
    }

    pub async fn leave_server(&self, server: ServerId) -> Result<()> {
        self.call(move |e| e.leave_server(server)).await?
    }

    pub async fn friends(&self) -> Result<Vec<FriendView>> {
        self.call(|e| e.friend_views()).await
    }

    /// Sends a friend request, or accepts one they sent us.
    pub async fn add_friend(&self, peer: PeerId) -> Result<()> {
        self.call(move |e| e.add_friend(peer)).await?
    }

    /// Unfriends, cancels a request we sent, or declines one we received.
    pub async fn remove_friend(&self, peer: PeerId) -> Result<()> {
        self.call(move |e| e.remove_friend(peer)).await?
    }

    /// Makes a link (for a QR code) that lets anyone who has it join `server`
    /// for the next `valid` duration. Only the owner can let people in.
    pub async fn invite_link(&self, server: ServerId, valid: Duration) -> Result<ShareLink> {
        self.call(move |e| e.invite_link(server, valid)).await?
    }

    /// Invalidates every invite code made for `server`. Returns how many there were.
    pub async fn revoke_invite_links(&self, server: ServerId) -> Result<usize> {
        self.call(move |e| {
            let before = e.invite_codes.len();
            e.invite_codes.retain(|_, (s, _)| *s != server);
            e.save_invite_codes()?;
            Ok(before - e.invite_codes.len())
        })
        .await?
    }

    /// Acts on a scanned or pasted link: sends a friend request, or asks to join a server.
    pub async fn open_link(&self, link: ShareLink) -> Result<()> {
        self.call(move |e| e.open_link(link)).await?
    }

    pub async fn block(&self, peer: PeerId, blocked: bool) -> Result<()> {
        self.call(move |e| e.block(peer, blocked)).await?
    }

    pub async fn set_name(&self, name: String) -> Result<()> {
        self.call(move |e| e.set_profile(Some(name), None)).await
    }

    /// Sets (or clears) our profile picture from image bytes.
    pub async fn set_avatar(&self, image: Option<Vec<u8>>) -> Result<()> {
        let avatar = match image {
            Some(bytes) => Some(self.upload(&bytes)?),
            None => None,
        };
        self.call(move |e| e.set_profile(None, Some(avatar))).await
    }

    /// Everyone's profile picture we know of, including ours.
    pub async fn avatars(&self) -> Result<Vec<(PeerId, BlobRef)>> {
        self.call(|e| {
            let mut v: Vec<(PeerId, BlobRef)> = e.avatars.iter().map(|(p, a)| (*p, *a)).collect();
            v.extend(e.my_avatar.map(|a| (e.me(), a)));
            v
        })
        .await
    }

    /// Encrypts and stores a file locally, ready to attach.
    pub fn upload(&self, bytes: &[u8]) -> Result<BlobRef> {
        if bytes.len() as u64 > blob::MAX_BLOB {
            bail!("files can be up to {} MB", blob::MAX_BLOB / 1024 / 1024);
        }
        let (r, ct) = blob::seal(bytes);
        self.blobs.put(&r.hash, &ct)?;
        Ok(r)
    }

    /// Returns a blob's contents, downloading it if needed: from `prefer`
    /// first, then storage nodes, then anyone else we're connected to.
    pub async fn fetch(&self, r: BlobRef, prefer: Vec<PeerId>) -> Result<Vec<u8>> {
        if let Some(ct) = self.blobs.get(&r.hash)? {
            return blob::open(&r, &ct);
        }
        if r.size > blob::MAX_BLOB + blob::MAX_BLOB / 1000 {
            bail!("file is too large");
        }
        let candidates = self.call(move |e| e.blob_sources(&prefer)).await?;
        for (_, conn) in candidates {
            if let Ok(Ok(Some(ct))) = tokio::time::timeout(BLOB_TIMEOUT, blob::request(&conn, &r.hash)).await {
                self.blobs.put(&r.hash, &ct)?;
                return blob::open(&r, &ct);
            }
        }
        bail!("nobody who has this file is online right now")
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
    camera: bool,
    muted: bool,
    deafened: bool,
}

impl VoiceSlot {
    fn new(channel: EventId) -> Self {
        Self { channel, live: false, camera: false, muted: false, deafened: false }
    }
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
    /// Whether we're talking right now (for others' indicators).
    speaking: bool,
    /// (muted, deafened), kept across calls.
    my_flags: (bool, bool),
    blobs: Arc<BlobStore>,
    friends: HashMap<PeerId, FriendRecord>,
    /// DM conversation -> the friend it's with.
    dms: HashMap<ServerId, PeerId>,
    avatars: HashMap<PeerId, BlobRef>,
    my_avatar: Option<BlobRef>,
    /// Invite codes we made: code -> (server, expiry in ms since the epoch).
    invite_codes: HashMap<InviteCode, (ServerId, u64)>,
    /// Servers we've asked to join with a code; their invite is accepted automatically.
    join_requests: HashSet<ServerId>,
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
        self.friends = self.store.friends()?.into_iter().collect();
        self.dms = self.store.dms()?.into_iter().collect();
        self.avatars = self.store.avatars()?.into_iter().collect();
        let codes: Vec<(InviteCode, ServerId, u64)> = self.store.meta(INVITE_CODES)?.unwrap_or_default();
        self.invite_codes = codes.into_iter().filter(|c| c.2 > now_ms()).map(|(c, s, t)| (c, (s, t))).collect();
        self.my_avatar = self.store.meta("avatar")?;
        Ok(())
    }

    fn is_blocked(&self, peer: &PeerId) -> bool {
        self.friends.get(peer).is_some_and(|f| f.state == FriendState::Blocked)
    }

    /// Sends now if connected; otherwise keeps it until we are.
    fn send_or_queue(&mut self, peer: PeerId, frame: Frame) -> Result<()> {
        if self.peers.contains_key(&peer) {
            self.send_to(&peer, frame);
        } else {
            self.store.push_outbox(peer, &frame)?;
            self.dial(peer);
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
        let endpoint = self.endpoint.clone();
        let shared = Shared { name: self.name.clone(), inbox: self.inbox.clone(), media: self.media.clone(), blobs: self.blobs.clone() };
        tokio::spawn(async move {
            let result = async {
                let conn = endpoint.connect(peer, ALPN).await?;
                let (send, recv) = conn.open_bi().await?;
                anyhow::Ok((conn, send, recv))
            }
            .await;
            let _ = shared.inbox.send(Inbox::Call(Box::new(move |e| {
                e.dialing.remove(&peer);
            })));
            if let Ok((conn, send, recv)) = result {
                let _ = drive(conn, send, recv, true, shared).await;
            }
        });
    }

    /// Keeps a connection to every member of every server we're in.
    fn reconnect(&mut self) {
        let mut wanted: HashSet<PeerId> = self.servers.values().flat_map(|s| s.sync_peers()).collect();
        wanted.extend(self.friends.iter().filter(|(_, f)| f.state != FriendState::Blocked).map(|(p, _)| *p));
        wanted.retain(|p| !self.is_blocked(p));
        for peer in wanted {
            self.dial(peer);
        }
    }

    fn on_connected(&mut self, peer: PeerId, new: PeerConn) {
        if self.is_blocked(&peer) {
            new.conn.close(0u32.into(), b"blocked");
            return;
        }
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
            let slot = self.voice.get(&server).and_then(|r| r.get(&me)).copied().unwrap_or(VoiceSlot::new(channel));
            for f in voice_frames(server, Some(slot)) {
                self.send_to(&peer, f);
            }
            if self.speaking {
                self.send_to(&peer, Frame::Speaking { server, speaking: true });
            }
        }
        self.send_to(&peer, Frame::Profile { name: self.name.clone(), avatar: self.my_avatar });
        match self.store.take_outbox(peer) {
            Ok(frames) => {
                for f in frames {
                    self.send_to(&peer, f);
                }
            }
            Err(e) => eprintln!("reading outbox: {e:#}"),
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
        if self.is_blocked(&peer) {
            return Ok(());
        }
        match frame {
            Frame::Hello { .. } => {}
            Frame::Speaking { server, speaking } => {
                if self.voice.get(&server).is_some_and(|r| r.contains_key(&peer)) {
                    self.emit(Update::Speaking { server, peer, speaking });
                }
            }
            Frame::Typing { server, channel } => {
                if self.servers.get(&server).is_some_and(|s| s.is_member(&peer)) {
                    self.emit(Update::Typing { server, channel, peer });
                }
            }
            Frame::FriendRequest { name } => self.on_friend_request(peer, name)?,
            Frame::FriendAccept { dm } => self.on_friend_accept(peer, dm)?,
            Frame::FriendRemove => {
                if self.friends.get(&peer).is_some_and(|f| f.state != FriendState::Blocked) {
                    self.friends.remove(&peer);
                    self.store.put_friend(peer, None)?;
                    self.emit(Update::Friend { peer, request: false });
                }
            }
            Frame::BlobOffer { server, hash, size } => self.on_blob_offer(peer, server, hash, size),
            Frame::Profile { name, avatar } => {
                let name: String = name.chars().take(64).collect();
                self.store.put_name(peer, &name)?;
                self.store.put_avatar(peer, avatar)?;
                match avatar {
                    Some(a) => self.avatars.insert(peer, a),
                    None => self.avatars.remove(&peer),
                };
                if let Some(p) = self.peers.get_mut(&peer) {
                    p.name = name;
                }
                self.emit(Update::Profile { peer });
            }
            Frame::Dm { body, sent_at_ms } => self.emit(Update::Dm { peer, body, sent_at_ms }),
            Frame::JoinRequest { server, code } => self.on_join_request(peer, server, code)?,
            Frame::Invite { server, name } => {
                if self.servers.contains_key(&server) {
                    return Ok(());
                }
                if self.join_requests.remove(&server) {
                    self.join(server, peer)?;
                    self.emit(Update::Joined { server });
                    return Ok(());
                }
                if self.dms.get(&server) == Some(&peer) {
                    return self.join(server, peer);
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
                    // Legacy frame: keep flags we already know for the same channel.
                    let known = self.voice.get(&server).and_then(|r| r.get(&peer)).copied();
                    let slot = channel.map(|channel| match known {
                        Some(k) if k.channel == channel => VoiceSlot { live, ..k },
                        _ => VoiceSlot { live, ..VoiceSlot::new(channel) },
                    });
                    self.set_voice(Some(server), peer, slot);
                }
            }
            Frame::VoiceState { server, channel, live, camera, muted, deafened } => {
                if self.servers.get(&server).is_some_and(|s| s.is_member(&peer)) {
                    let slot = channel.map(|channel| VoiceSlot { channel, live, camera, muted, deafened });
                    self.set_voice(Some(server), peer, slot);
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
            if let Change::LeaveRequested(peer) = &change
                && self.servers[&server].owner() == Some(self.me())
                && self.servers[&server].is_member(peer)
            {
                self.kick(server, *peer)?;
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

    fn invite_link(&mut self, server: ServerId, valid: Duration) -> Result<ShareLink> {
        if self.server(server)?.owner() != Some(self.me()) {
            bail!("only the server's owner can make invite codes");
        }
        let code: InviteCode = rand::random();
        let expires = now_ms() + valid.as_millis() as u64;
        self.invite_codes.retain(|_, (_, t)| *t > now_ms());
        self.invite_codes.insert(code, (server, expires));
        self.save_invite_codes()?;
        Ok(ShareLink::Join { server, via: self.me(), code })
    }

    fn save_invite_codes(&self) -> Result<()> {
        let codes: Vec<(InviteCode, ServerId, u64)> = self.invite_codes.iter().map(|(c, (s, t))| (*c, *s, *t)).collect();
        self.store.put_meta(INVITE_CODES, &codes)
    }

    fn open_link(&mut self, link: ShareLink) -> Result<()> {
        match link {
            ShareLink::Friend(peer) if peer == self.me() => bail!("that's your own code"),
            ShareLink::Friend(peer) => self.add_friend(peer),
            ShareLink::Join { server, .. } if self.servers.contains_key(&server) => bail!("you're already in that server"),
            ShareLink::Join { server, via, code } => {
                self.join_requests.insert(server);
                self.send_or_queue(via, Frame::JoinRequest { server, code })
            }
        }
    }

    fn on_join_request(&mut self, peer: PeerId, server: ServerId, code: InviteCode) -> Result<()> {
        match self.invite_codes.get(&code) {
            Some((s, expires)) if *s == server && *expires > now_ms() => {}
            _ => return Ok(()), // unknown, expired, or for another server: ignore
        }
        if self.is_blocked(&peer) || self.server(server)?.owner() != Some(self.me()) {
            return Ok(());
        }
        self.invite(server, peer)
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
        let (muted, deafened) = self.my_flags;
        self.set_voice(Some(server), me, Some(VoiceSlot { muted, deafened, ..VoiceSlot::new(channel) }));
        Ok(VoiceSession { hub: self.media.clone(), incoming: rx })
    }

    fn voice_leave(&mut self) {
        let Some((server, _)) = self.my_voice.take() else { return };
        self.media.set_sink(None);
        self.watchers.clear();
        self.speaking = false;
        let me = self.me();
        self.set_voice(Some(server), me, None);
    }

    fn set_live(&mut self, live: bool) -> Result<()> {
        if !live {
            self.watchers.clear();
        }
        self.update_my_slot(|s| s.live = live)
    }

    /// Changes our own voice flags and tells everyone.
    fn update_my_slot(&mut self, f: impl FnOnce(&mut VoiceSlot)) -> Result<()> {
        let (server, channel) = self.my_voice.context("join a voice channel first")?;
        let me = self.me();
        let mut slot = self.voice.get(&server).and_then(|r| r.get(&me)).copied().unwrap_or(VoiceSlot::new(channel));
        f(&mut slot);
        self.set_voice(Some(server), me, Some(slot));
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
                self.emit(Update::KeyframeRequested { camera: false });
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
            let mut channels: Vec<EventId> = [before, slot].into_iter().flatten().map(|s| s.channel).collect();
            channels.dedup();
            for c in channels {
                if let Some(room) = room_view(sid, c, &self.voice[&sid]) {
                    let VoiceRoom { peers, live, camera, muted, deafened, .. } = room;
                    let _ = self.updates.send(Update::Voice { server: sid, channel: c, peers, live, camera, muted, deafened });
                } else {
                    let none = Vec::new;
                    let _ = self.updates.send(Update::Voice {
                        server: sid,
                        channel: c,
                        peers: none(),
                        live: none(),
                        camera: none(),
                        muted: none(),
                        deafened: none(),
                    });
                }
            }
            let me = self.me();
            // Someone new arrived in our call while our camera is on: send them a picture now.
            let arrived = slot.map(|s| s.channel) != before.map(|s| s.channel);
            if peer != me && arrived && self.my_voice == slot.map(|s| (sid, s.channel)) {
                let camera_on = self.voice[&sid].get(&me).is_some_and(|s| s.camera);
                if camera_on {
                    self.emit(Update::KeyframeRequested { camera: true });
                }
            }
            if peer == me
                && let Some(s) = self.servers.get(&sid)
            {
                for m in s.members() {
                    for f in voice_frames(sid, slot) {
                        self.send_to(&m, f);
                    }
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

    // ---- presence ----

    fn voice_rooms(&self) -> Vec<VoiceRoom> {
        let mut out = Vec::new();
        for (server, rooms) in &self.voice {
            let channels: HashSet<EventId> = rooms.values().map(|s| s.channel).collect();
            out.extend(channels.into_iter().filter_map(|c| room_view(*server, c, rooms)));
        }
        out
    }

    fn set_speaking(&mut self, speaking: bool) {
        if self.speaking == speaking {
            return;
        }
        self.speaking = speaking;
        let Some((server, _)) = self.my_voice else { return };
        let me = self.me();
        self.emit(Update::Speaking { server, peer: me, speaking });
        for m in self.servers[&server].members() {
            self.send_to(&m, Frame::Speaking { server, speaking });
        }
    }

    fn typing(&self, server: ServerId, channel: EventId) {
        let Some(s) = self.servers.get(&server) else { return };
        for m in s.members() {
            self.send_to(&m, Frame::Typing { server, channel });
        }
    }

    fn set_profile(&mut self, name: Option<String>, avatar: Option<Option<BlobRef>>) {
        if let Some(n) = name {
            self.name = n;
        }
        if let Some(a) = avatar {
            self.my_avatar = a;
            if let Err(e) = self.store.put_meta("avatar", &a) {
                eprintln!("saving avatar: {e:#}");
            }
        }
        let frame = Frame::Profile { name: self.name.clone(), avatar: self.my_avatar };
        for p in self.peers.keys() {
            self.send_to(p, frame.clone());
        }
        let me = self.me();
        self.emit(Update::Profile { peer: me });
    }

    // ---- messages ----

    fn post(
        &mut self,
        server: ServerId,
        channel: EventId,
        body: String,
        reply_to: Option<EventId>,
        attachments: Vec<Attachment>,
    ) -> Result<EventId> {
        if body.chars().count() > MAX_BODY {
            bail!("message longer than {MAX_BODY} characters");
        }
        if body.trim().is_empty() && attachments.is_empty() {
            bail!("message is empty");
        }
        if attachments.len() > MAX_ATTACHMENTS {
            bail!("up to {MAX_ATTACHMENTS} attachments per message");
        }
        match self.server(server)?.channels.get(&channel) {
            Some(c) if c.kind == ChannelKind::Text => {}
            _ => bail!("unknown text channel"),
        }
        let offers: Vec<(blob::BlobHash, u64)> = attachments.iter().map(|a| (a.blob.hash, a.blob.size)).collect();
        let sent_at_ms = now_ms();
        let id = self.author_content(server, Content::Post { channel, body, sent_at_ms, reply_to, attachments })?;
        // Storage nodes fetch the files so they're available while we're offline.
        for host in self.servers[&server].hosts().to_vec() {
            for (hash, size) in &offers {
                self.send_to(&host, Frame::BlobOffer { server, hash: *hash, size: *size });
            }
        }
        Ok(id)
    }

    fn own_message(&self, server: ServerId, target: EventId) -> Result<&ChatMessage> {
        let m = self.server(server)?.message(&target).context("message not found")?;
        if m.author != self.me() {
            bail!("you can only change your own messages");
        }
        Ok(m)
    }

    fn edit(&mut self, server: ServerId, target: EventId, body: String) -> Result<()> {
        if body.trim().is_empty() || body.chars().count() > MAX_BODY {
            bail!("messages must be 1 to {MAX_BODY} characters");
        }
        self.own_message(server, target)?;
        self.author_content(server, Content::Edit { target, body })?;
        Ok(())
    }

    fn delete(&mut self, server: ServerId, target: EventId) -> Result<()> {
        let s = self.server(server)?;
        if s.owner() != Some(self.me()) {
            self.own_message(server, target)?;
        }
        self.author_content(server, Content::Delete { target })?;
        Ok(())
    }

    fn react(&mut self, server: ServerId, target: EventId, emoji: String, on: bool) -> Result<()> {
        if emoji.is_empty() || emoji.chars().count() > 16 {
            bail!("not an emoji");
        }
        self.server(server)?.message(&target).context("message not found")?;
        self.author_content(server, Content::React { target, emoji, on })?;
        Ok(())
    }

    fn leave_server(&mut self, server: ServerId) -> Result<()> {
        let s = self.server(server)?;
        let owner = s.owner();
        if owner == Some(self.me()) {
            bail!("you own this server, so you can't leave it");
        }
        if s.is_member(&self.me()) && s.keys().contains_key(&s.epoch()) {
            let e = s.author_content(&self.secret, &Content::Leave)?;
            self.ingest(server, vec![e.clone()], None)?;
            // Make sure the owner hears about it even if they're offline right now.
            if let Some(o) = owner
                && !self.peers.contains_key(&o)
            {
                self.store.push_outbox(o, &Frame::Events(vec![e]))?;
            }
        }
        self.forget_server(server)
    }

    fn forget_server(&mut self, server: ServerId) -> Result<()> {
        if self.my_voice.is_some_and(|(s, _)| s == server) {
            self.voice_leave();
        }
        self.servers.remove(&server);
        self.dms.remove(&server);
        self.voice.remove(&server);
        self.store.remove_server(server)?;
        self.emit(Update::ServerRemoved { server });
        Ok(())
    }

    // ---- files ----

    /// Connections to try for a blob, best first.
    fn blob_sources(&self, prefer: &[PeerId]) -> Vec<(PeerId, Connection)> {
        let hosts: HashSet<PeerId> = self.servers.values().flat_map(|s| s.hosts().to_vec()).collect();
        let mut order: Vec<PeerId> = prefer.to_vec();
        order.extend(hosts.iter().copied());
        order.extend(self.peers.keys().copied());
        let mut seen = HashSet::new();
        order
            .into_iter()
            .filter(|p| seen.insert(*p))
            .filter_map(|p| self.peers.get(&p).map(|c| (p, c.conn.clone())))
            .collect()
    }

    /// Storage hosts pull files that members offer, so they can serve them later.
    fn on_blob_offer(&self, peer: PeerId, server: ServerId, hash: blob::BlobHash, size: u64) {
        let me = self.me();
        let hosted = self.servers.get(&server).is_some_and(|s| s.is_host(&me) && s.is_member(&peer));
        if self.host.is_none() || !hosted || size > blob::MAX_BLOB * 2 || self.blobs.has(&hash) {
            return;
        }
        let Some(conn) = self.peers.get(&peer).map(|c| c.conn.clone()) else { return };
        let blobs = self.blobs.clone();
        tokio::spawn(async move {
            if let Ok(Ok(Some(ct))) = tokio::time::timeout(BLOB_TIMEOUT, blob::request(&conn, &hash)).await {
                let _ = blobs.put(&hash, &ct);
            }
        });
    }

    // ---- friends ----

    fn friend_views(&self) -> Vec<FriendView> {
        self.friends
            .iter()
            .map(|(peer, f)| FriendView {
                peer: *peer,
                state: f.state,
                since_ms: f.since_ms,
                dm: self.dms.iter().find(|(_, p)| *p == peer).map(|(s, _)| *s),
                online: self.peers.contains_key(peer),
            })
            .collect()
    }

    fn set_friend(&mut self, peer: PeerId, state: Option<FriendState>, request: bool) -> Result<()> {
        let record = state.map(|state| FriendRecord { state, since_ms: now_ms() });
        match record {
            Some(r) => self.friends.insert(peer, r),
            None => self.friends.remove(&peer),
        };
        self.store.put_friend(peer, record)?;
        self.emit(Update::Friend { peer, request });
        Ok(())
    }

    fn dm_with(&self, peer: &PeerId) -> Option<ServerId> {
        self.dms.iter().find(|(_, p)| *p == peer).map(|(s, _)| *s)
    }

    /// Both sides agreed. Exactly one DM gets created: by whoever has the smaller ID.
    fn become_friends(&mut self, peer: PeerId) -> Result<()> {
        // Create the DM before announcing, so the UI sees both at once.
        let mut dm = self.dm_with(&peer);
        if dm.is_none() && self.me() < peer {
            dm = Some(self.create_dm(peer)?);
        }
        self.set_friend(peer, Some(FriendState::Friends), false)?;
        self.send_or_queue(peer, Frame::FriendAccept { dm })
    }

    fn create_dm(&mut self, peer: PeerId) -> Result<ServerId> {
        let id = self.create_server("")?;
        self.author_admin(id, AdminOp::Add { peer })?;
        self.dms.insert(id, peer);
        self.store.put_dm(id, peer)?;
        Ok(id)
    }

    fn add_friend(&mut self, peer: PeerId) -> Result<()> {
        if peer == self.me() {
            bail!("that's your own ID");
        }
        match self.friends.get(&peer).map(|f| f.state) {
            Some(FriendState::Blocked) => bail!("unblock them first"),
            Some(FriendState::Friends) => Ok(()),
            Some(FriendState::Incoming) => self.become_friends(peer),
            Some(FriendState::Outgoing) | None => {
                self.set_friend(peer, Some(FriendState::Outgoing), false)?;
                let name = self.name.clone();
                self.send_or_queue(peer, Frame::FriendRequest { name })
            }
        }
    }

    fn remove_friend(&mut self, peer: PeerId) -> Result<()> {
        match self.friends.get(&peer).map(|f| f.state) {
            Some(FriendState::Blocked) | None => Ok(()),
            Some(_) => {
                self.set_friend(peer, None, false)?;
                self.send_or_queue(peer, Frame::FriendRemove)
            }
        }
    }

    fn block(&mut self, peer: PeerId, blocked: bool) -> Result<()> {
        if !blocked {
            if self.is_blocked(&peer) {
                self.set_friend(peer, None, false)?;
            }
            return Ok(());
        }
        self.set_friend(peer, Some(FriendState::Blocked), false)?;
        if let Some(p) = self.peers.remove(&peer) {
            p.conn.close(0u32.into(), b"blocked");
            self.emit(Update::PeerOffline { peer });
            self.set_voice(None, peer, None);
        }
        Ok(())
    }

    fn on_friend_request(&mut self, peer: PeerId, name: String) -> Result<()> {
        self.store.put_name(peer, &name.chars().take(64).collect::<String>())?;
        match self.friends.get(&peer).map(|f| f.state) {
            // We asked them too: that's a yes.
            Some(FriendState::Outgoing) => self.become_friends(peer),
            // They lost track of us; answer again.
            Some(FriendState::Friends) => {
                let dm = self.dm_with(&peer);
                self.send_or_queue(peer, Frame::FriendAccept { dm })
            }
            Some(FriendState::Incoming) | Some(FriendState::Blocked) => Ok(()),
            None => self.set_friend(peer, Some(FriendState::Incoming), true),
        }
    }

    fn on_friend_accept(&mut self, peer: PeerId, dm: Option<ServerId>) -> Result<()> {
        if !matches!(self.friends.get(&peer).map(|f| f.state), Some(FriendState::Outgoing | FriendState::Friends)) {
            return Ok(());
        }
        let was_friends = self.friends.get(&peer).is_some_and(|f| f.state == FriendState::Friends);
        if !was_friends {
            self.set_friend(peer, Some(FriendState::Friends), false)?;
        }
        match (dm, self.dm_with(&peer)) {
            (Some(theirs), None) => {
                self.dms.insert(theirs, peer);
                self.store.put_dm(theirs, peer)?;
                self.join(theirs, peer)?;
                self.emit(Update::Friend { peer, request: false });
            }
            (None, None) if self.me() < peer => {
                let ours = self.create_dm(peer)?;
                self.send_or_queue(peer, Frame::FriendAccept { dm: Some(ours) })?;
                self.emit(Update::Friend { peer, request: false });
            }
            _ => {}
        }
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
                dm: self.dms.get(&s.id).copied(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        out
    }
}

/// What every connection task needs, whichever side dialed.
#[derive(Debug, Clone)]
struct Shared {
    name: String,
    inbox: mpsc::UnboundedSender<Inbox>,
    media: Arc<MediaHub>,
    blobs: Arc<BlobStore>,
}

#[derive(Debug, Clone)]
struct Accept(Shared);

impl ProtocolHandler for Accept {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (send, recv) = conn.accept_bi().await?;
        let _ = drive(conn, send, recv, false, self.0.clone()).await;
        Ok(())
    }
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Runs one connection: hello exchange, then a writer task and a read loop.
async fn drive(conn: Connection, mut send: SendStream, mut recv: RecvStream, dialed: bool, shared: Shared) -> Result<()> {
    let Shared { name: our_name, inbox, media, blobs } = shared;
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

    // Further bidirectional streams are blob requests.
    let requests = conn.clone();
    tokio::spawn(async move {
        while let Ok((send, recv)) = requests.accept_bi().await {
            let blobs = blobs.clone();
            tokio::spawn(async move {
                let _ = blob::serve(&blobs, send, recv).await;
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

/// Who's in `channel` and with which flags, or `None` if nobody is.
fn room_view(server: ServerId, channel: EventId, rooms: &HashMap<PeerId, VoiceSlot>) -> Option<VoiceRoom> {
    let here: Vec<(&PeerId, &VoiceSlot)> = rooms.iter().filter(|(_, s)| s.channel == channel).collect();
    if here.is_empty() {
        return None;
    }
    let pick = |f: fn(&VoiceSlot) -> bool| here.iter().filter(|(_, s)| f(s)).map(|(p, _)| **p).collect();
    Some(VoiceRoom {
        server,
        channel,
        peers: here.iter().map(|(p, _)| **p).collect(),
        live: pick(|s| s.live),
        camera: pick(|s| s.camera),
        muted: pick(|s| s.muted),
        deafened: pick(|s| s.deafened),
    })
}

/// Our voice state as frames: the legacy one older peers understand, then the full one.
fn voice_frames(server: ServerId, slot: Option<VoiceSlot>) -> [Frame; 2] {
    let channel = slot.map(|s| s.channel);
    let flag = |f: fn(&VoiceSlot) -> bool| slot.as_ref().is_some_and(f);
    let live = flag(|s| s.live);
    [
        Frame::Voice { server, channel, live },
        Frame::VoiceState { server, channel, live, camera: flag(|s| s.camera), muted: flag(|s| s.muted), deafened: flag(|s| s.deafened) },
    ]
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
