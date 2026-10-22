// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hearth_core::{
    Attachment, BlobRef, Change, ChannelKind, ChatMessage, Config, EventId, FriendState, Hearth, MediaSender, PeerId,
    ServerId, ServerSummary, Update, identity,
};
use hearth_media::{Audio, AudioOptions};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, State,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response},
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

// ---- wire types for the UI (ids are hex strings, events are "author:seq") ----

fn eid(id: &EventId) -> String {
    format!("{}:{}", id.author, id.seq)
}

fn parse_eid(s: &str) -> Res<EventId> {
    let (a, n) = s.split_once(':').ok_or("bad id")?;
    Ok(EventId { author: a.parse().map_err(err)?, seq: n.parse().map_err(err)? })
}

fn parse_sid(s: &str) -> Res<ServerId> {
    s.parse().map_err(err)
}

fn parse_peer(s: &str) -> Res<PeerId> {
    s.trim().parse().map_err(|_| "that doesn't look like a peer ID".to_string())
}

fn to_hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Res<[u8; 32]> {
    if s.len() != 64 {
        return Err("bad hex".into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(err)?;
    }
    Ok(out)
}

#[derive(Serialize, Deserialize, Clone)]
struct BlobDto {
    hash: String,
    key: String,
    size: u64,
}

impl From<BlobRef> for BlobDto {
    fn from(b: BlobRef) -> Self {
        Self { hash: to_hex(&b.hash), key: to_hex(&b.key), size: b.size }
    }
}

impl TryFrom<&BlobDto> for BlobRef {
    type Error = String;
    fn try_from(b: &BlobDto) -> Res<Self> {
        Ok(BlobRef { hash: from_hex(&b.hash)?, key: from_hex(&b.key)?, size: b.size })
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct AttachmentDto {
    name: String,
    mime: String,
    blob: BlobDto,
}

impl From<&Attachment> for AttachmentDto {
    fn from(a: &Attachment) -> Self {
        Self { name: a.name.clone(), mime: a.mime.clone(), blob: a.blob.into() }
    }
}

impl TryFrom<&AttachmentDto> for Attachment {
    type Error = String;
    fn try_from(a: &AttachmentDto) -> Res<Self> {
        Ok(Attachment { name: a.name.chars().take(200).collect(), mime: a.mime.chars().take(100).collect(), blob: (&a.blob).try_into()? })
    }
}

#[derive(Serialize, Clone)]
struct ChannelDto {
    id: String,
    name: String,
    voice: bool,
}

#[derive(Serialize, Clone)]
struct ServerDto {
    id: String,
    name: Option<String>,
    owner: Option<String>,
    members: Vec<String>,
    hosts: Vec<String>,
    channels: Vec<ChannelDto>,
    epoch: u32,
    dm: Option<String>,
}

impl From<ServerSummary> for ServerDto {
    fn from(s: ServerSummary) -> Self {
        Self {
            id: s.id.to_string(),
            name: s.name,
            owner: s.owner.map(|o| o.to_string()),
            members: s.members.iter().map(|m| m.to_string()).collect(),
            hosts: s.hosts.iter().map(|m| m.to_string()).collect(),
            channels: s
                .channels
                .iter()
                .map(|c| ChannelDto { id: eid(&c.id), name: c.name.clone(), voice: c.kind == ChannelKind::Voice })
                .collect(),
            epoch: s.epoch,
            dm: s.dm.map(|p| p.to_string()),
        }
    }
}

#[derive(Serialize, Clone)]
struct MessageDto {
    id: String,
    channel: String,
    author: String,
    body: String,
    sent_at_ms: u64,
    reply_to: Option<String>,
    attachments: Vec<AttachmentDto>,
    edited: bool,
    deleted: bool,
    reactions: BTreeMap<String, Vec<String>>,
}

impl From<&ChatMessage> for MessageDto {
    fn from(m: &ChatMessage) -> Self {
        Self {
            id: eid(&m.id),
            channel: eid(&m.channel),
            author: m.author.to_string(),
            body: m.body.clone(),
            sent_at_ms: m.sent_at_ms,
            reply_to: m.reply_to.as_ref().map(eid),
            attachments: m.attachments.iter().map(Into::into).collect(),
            edited: m.edited,
            deleted: m.deleted,
            reactions: m.reactions.iter().map(|(e, who)| (e.clone(), who.iter().map(|p| p.to_string()).collect())).collect(),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UpdateDto {
    PeerOnline { peer: String, name: String },
    PeerOffline { peer: String },
    Dm { peer: String, body: String, sent_at_ms: u64 },
    Invited { server: String, from: String, name: String },
    Message { server: String, message: MessageDto },
    MessageUpdated { server: String, message: MessageDto },
    ServerChanged { server: String },
    Removed { server: String },
    ServerRemoved { server: String },
    Voice { server: String, channel: String, peers: Vec<String>, live: Vec<String> },
    Speaking { server: String, peer: String, speaking: bool },
    Typing { server: String, channel: String, peer: String },
    Friend { peer: String, request: bool },
    Profile { peer: String },
    KeyframeRequested,
}

fn to_dto(u: Update, me: PeerId) -> UpdateDto {
    match u {
        Update::PeerOnline { peer, name } => UpdateDto::PeerOnline { peer: peer.to_string(), name },
        Update::PeerOffline { peer } => UpdateDto::PeerOffline { peer: peer.to_string() },
        Update::Dm { peer, body, sent_at_ms } => UpdateDto::Dm { peer: peer.to_string(), body, sent_at_ms },
        Update::Invited { server, from, name } => {
            UpdateDto::Invited { server: server.to_string(), from: from.to_string(), name }
        }
        Update::Server { server, change: Change::Message(m) } => {
            UpdateDto::Message { server: server.to_string(), message: (&m).into() }
        }
        Update::Server { server, change: Change::MessageUpdated(m) } => {
            UpdateDto::MessageUpdated { server: server.to_string(), message: (&m).into() }
        }
        Update::Server { server, change: Change::MemberRemoved(p) } if p == me => {
            UpdateDto::Removed { server: server.to_string() }
        }
        Update::Server { server, .. } => UpdateDto::ServerChanged { server: server.to_string() },
        Update::ServerRemoved { server } => UpdateDto::ServerRemoved { server: server.to_string() },
        Update::Voice { server, channel, peers, live } => UpdateDto::Voice {
            server: server.to_string(),
            channel: eid(&channel),
            peers: peers.iter().map(|p| p.to_string()).collect(),
            live: live.iter().map(|p| p.to_string()).collect(),
        },
        Update::Speaking { server, peer, speaking } => {
            UpdateDto::Speaking { server: server.to_string(), peer: peer.to_string(), speaking }
        }
        Update::Typing { server, channel, peer } => {
            UpdateDto::Typing { server: server.to_string(), channel: eid(&channel), peer: peer.to_string() }
        }
        Update::Friend { peer, request } => UpdateDto::Friend { peer: peer.to_string(), request },
        Update::Profile { peer } => UpdateDto::Profile { peer: peer.to_string() },
        Update::KeyframeRequested => UpdateDto::KeyframeRequested,
    }
}

// ---- app state ----

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    name: Option<String>,
}

struct App {
    hearth: Hearth,
    name: Mutex<String>,
    data_dir: PathBuf,
    audio: Mutex<Option<Audio>>,
    video_out: Mutex<Option<MediaSender>>,
    /// Where incoming screen-share frames go: `peer id (32 bytes) | payload`.
    video_in: Arc<Mutex<Option<Channel<InvokeResponseBody>>>>,
    /// What we last told peers about our speaking state.
    speaking: Mutex<bool>,
}

fn default_name() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "anon".into())
}

fn load_settings(dir: &std::path::Path) -> Settings {
    std::fs::read(dir.join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

// ---- commands: identity & servers ----

#[derive(Serialize)]
struct Me {
    id: String,
    name: String,
}

#[tauri::command]
fn me(app: State<'_, App>) -> Me {
    Me { id: app.hearth.id().to_string(), name: app.name.lock().unwrap().clone() }
}

#[tauri::command]
async fn set_name(app: State<'_, App>, name: String) -> Res<()> {
    let name: String = name.trim().chars().take(64).collect();
    if name.is_empty() {
        return Err("name can't be empty".into());
    }
    let settings = Settings { name: Some(name.clone()) };
    std::fs::write(app.data_dir.join("settings.json"), serde_json::to_vec_pretty(&settings).map_err(err)?)
        .map_err(err)?;
    *app.name.lock().unwrap() = name.clone();
    app.hearth.set_name(name).await.map_err(err)
}

#[tauri::command]
async fn servers(app: State<'_, App>) -> Res<Vec<ServerDto>> {
    Ok(app.hearth.servers().await.map_err(err)?.into_iter().map(Into::into).collect())
}

#[tauri::command]
async fn names(app: State<'_, App>) -> Res<HashMap<String, String>> {
    Ok(app.hearth.known_names().await.map_err(err)?.into_iter().map(|(p, n)| (p.to_string(), n)).collect())
}

#[tauri::command]
async fn online(app: State<'_, App>) -> Res<Vec<String>> {
    Ok(app.hearth.online_peers().await.map_err(err)?.into_iter().map(|(p, _)| p.to_string()).collect())
}

#[tauri::command]
async fn history(app: State<'_, App>, server: String, channel: String, limit: usize) -> Res<Vec<MessageDto>> {
    let (sid, cid) = (parse_sid(&server)?, parse_eid(&channel)?);
    Ok(app.hearth.history(sid, cid, limit).await.map_err(err)?.iter().map(Into::into).collect())
}

#[tauri::command]
async fn send(
    app: State<'_, App>,
    server: String,
    channel: String,
    body: String,
    reply_to: Option<String>,
    attachments: Option<Vec<AttachmentDto>>,
) -> Res<String> {
    let reply_to = reply_to.as_deref().map(parse_eid).transpose()?;
    let attachments = attachments.unwrap_or_default().iter().map(Attachment::try_from).collect::<Res<Vec<_>>>()?;
    let id = app.hearth.post(parse_sid(&server)?, parse_eid(&channel)?, body, reply_to, attachments).await.map_err(err)?;
    Ok(eid(&id))
}

#[tauri::command]
async fn edit(app: State<'_, App>, server: String, target: String, body: String) -> Res<()> {
    app.hearth.edit(parse_sid(&server)?, parse_eid(&target)?, body).await.map_err(err)
}

#[tauri::command]
async fn delete(app: State<'_, App>, server: String, target: String) -> Res<()> {
    app.hearth.delete(parse_sid(&server)?, parse_eid(&target)?).await.map_err(err)
}

#[tauri::command]
async fn react(app: State<'_, App>, server: String, target: String, emoji: String, on: bool) -> Res<()> {
    app.hearth.react(parse_sid(&server)?, parse_eid(&target)?, emoji, on).await.map_err(err)
}

#[tauri::command]
async fn typing(app: State<'_, App>, server: String, channel: String) -> Res<()> {
    app.hearth.typing(parse_sid(&server)?, parse_eid(&channel)?).await.map_err(err)
}

#[tauri::command]
async fn create_server(app: State<'_, App>, name: String) -> Res<String> {
    Ok(app.hearth.create_server(name).await.map_err(err)?.to_string())
}

#[tauri::command]
async fn create_channel(app: State<'_, App>, server: String, name: String, voice: bool) -> Res<String> {
    let kind = if voice { ChannelKind::Voice } else { ChannelKind::Text };
    Ok(eid(&app.hearth.create_channel(parse_sid(&server)?, name, kind).await.map_err(err)?))
}

#[tauri::command]
async fn invite(app: State<'_, App>, server: String, peer: String) -> Res<()> {
    app.hearth.invite(parse_sid(&server)?, parse_peer(&peer)?).await.map_err(err)
}

#[tauri::command]
async fn kick(app: State<'_, App>, server: String, peer: String) -> Res<()> {
    app.hearth.kick(parse_sid(&server)?, parse_peer(&peer)?).await.map_err(err)
}

#[tauri::command]
async fn leave_server(app: State<'_, App>, server: String) -> Res<()> {
    app.hearth.leave_server(parse_sid(&server)?).await.map_err(err)
}

#[tauri::command]
async fn add_host(app: State<'_, App>, server: String, peer: String) -> Res<()> {
    app.hearth.add_host(parse_sid(&server)?, parse_peer(&peer)?).await.map_err(err)
}

#[tauri::command]
async fn join(app: State<'_, App>, server: String, via: String) -> Res<()> {
    app.hearth.join(parse_sid(&server)?, parse_peer(&via)?).await.map_err(err)
}

#[tauri::command]
async fn dial(app: State<'_, App>, peer: String) -> Res<()> {
    app.hearth.dial(parse_peer(&peer)?).await.map_err(err)
}

// ---- friends ----

#[derive(Serialize)]
struct FriendDto {
    peer: String,
    state: &'static str,
    since_ms: u64,
    dm: Option<String>,
    online: bool,
}

#[tauri::command]
async fn friends(app: State<'_, App>) -> Res<Vec<FriendDto>> {
    Ok(app
        .hearth
        .friends()
        .await
        .map_err(err)?
        .into_iter()
        .map(|f| FriendDto {
            peer: f.peer.to_string(),
            state: match f.state {
                FriendState::Incoming => "incoming",
                FriendState::Outgoing => "outgoing",
                FriendState::Friends => "friends",
                FriendState::Blocked => "blocked",
            },
            since_ms: f.since_ms,
            dm: f.dm.map(|s| s.to_string()),
            online: f.online,
        })
        .collect())
}

#[tauri::command]
async fn add_friend(app: State<'_, App>, peer: String) -> Res<()> {
    app.hearth.add_friend(parse_peer(&peer)?).await.map_err(err)
}

#[tauri::command]
async fn remove_friend(app: State<'_, App>, peer: String) -> Res<()> {
    app.hearth.remove_friend(parse_peer(&peer)?).await.map_err(err)
}

#[tauri::command]
async fn block(app: State<'_, App>, peer: String, blocked: bool) -> Res<()> {
    app.hearth.block(parse_peer(&peer)?, blocked).await.map_err(err)
}

// ---- files & avatars ----

fn header(request: &Request<'_>, name: &str) -> String {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| urlencoding::decode(v).map(|s| s.into_owned()).unwrap_or_default())
        .unwrap_or_default()
}

/// Encrypts the request body as an attachment. Headers: `x-name`, `x-mime` (URI-encoded).
#[tauri::command]
async fn upload(app: State<'_, App>, request: Request<'_>) -> Res<AttachmentDto> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected file bytes".into());
    };
    let blob = app.hearth.upload(bytes).map_err(err)?;
    let name = header(&request, "x-name");
    let mime = header(&request, "x-mime");
    Ok(AttachmentDto { name: if name.is_empty() { "file".into() } else { name }, mime, blob: blob.into() })
}

/// Returns a file's bytes, downloading from `prefer` (the author) or anyone online.
#[tauri::command]
async fn fetch_blob(app: State<'_, App>, blob: BlobDto, prefer: Vec<String>) -> Res<Response> {
    let r = BlobRef::try_from(&blob)?;
    let prefer = prefer.iter().filter_map(|p| p.parse().ok()).collect();
    let bytes = app.hearth.fetch(r, prefer).await.map_err(err)?;
    Ok(Response::new(bytes))
}

/// Saves an attachment into Downloads and shows it in the file manager.
#[tauri::command]
async fn save_attachment(handle: AppHandle, app: State<'_, App>, attachment: AttachmentDto, prefer: Vec<String>) -> Res<String> {
    let r = BlobRef::try_from(&attachment.blob)?;
    let prefer = prefer.iter().filter_map(|p| p.parse().ok()).collect();
    let bytes = app.hearth.fetch(r, prefer).await.map_err(err)?;
    let dir = directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(|p| p.to_path_buf()))
        .ok_or("no Downloads folder")?;
    let path = unique_path(&dir, &safe_file_name(&attachment.name));
    std::fs::write(&path, bytes).map_err(err)?;
    let _ = handle.opener().reveal_item_in_dir(&path);
    Ok(path.display().to_string())
}

/// A sender-chosen name made safe to write: no path separators or reserved characters.
fn safe_file_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    if safe.trim_matches(['.', ' ']).is_empty() { "file".to_string() } else { safe }
}

/// `dir/name`, or `dir/name (1).ext` etc. if that exists.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

#[tauri::command]
async fn set_avatar(app: State<'_, App>, request: Request<'_>) -> Res<()> {
    let image = match request.body() {
        InvokeBody::Raw(bytes) if !bytes.is_empty() => Some(bytes.clone()),
        _ => None,
    };
    if image.as_ref().is_some_and(|i| i.len() > 2 * 1024 * 1024) {
        return Err("profile pictures can be up to 2 MB".into());
    }
    app.hearth.set_avatar(image).await.map_err(err)
}

#[tauri::command]
async fn avatars(app: State<'_, App>) -> Res<HashMap<String, BlobDto>> {
    Ok(app.hearth.avatars().await.map_err(err)?.into_iter().map(|(p, b)| (p.to_string(), b.into())).collect())
}

// ---- notifications & links ----

#[tauri::command]
fn notify(handle: AppHandle, title: String, body: String) -> Res<()> {
    handle.notification().builder().title(title).body(body).show().map_err(err)
}

/// Opens a web link in the user's browser (never inside the app).
#[tauri::command]
fn open_url(handle: AppHandle, url: String) -> Res<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only web links can be opened".into());
    }
    handle.opener().open_url(url, None::<&str>).map_err(err)
}

// ---- voice ----

/// Joins a voice channel and starts audio. Returns non-fatal warnings (e.g. no mic).
#[tauri::command]
async fn voice_join(app: State<'_, App>, server: String, channel: String) -> Res<Vec<String>> {
    *app.audio.lock().unwrap() = None;
    *app.video_out.lock().unwrap() = None;
    let session = app.hearth.voice_join(parse_sid(&server)?, parse_eid(&channel)?).await.map_err(err)?;
    let sink = app.video_in.clone();
    let opts = AudioOptions {
        on_media: Some(Box::new(move |p| {
            if p.kind != hearth_core::KIND_VIDEO {
                return;
            }
            if let Some(ch) = sink.lock().unwrap().as_ref() {
                let mut buf = Vec::with_capacity(32 + p.payload.len());
                buf.extend_from_slice(p.from.as_bytes());
                buf.extend_from_slice(&p.payload);
                let _ = ch.send(InvokeResponseBody::Raw(buf));
            }
        })),
        ..Default::default()
    };
    match Audio::start(session, opts) {
        Ok(audio) => {
            let warnings = audio.warnings.clone();
            *app.video_out.lock().unwrap() = Some(audio.sender());
            *app.audio.lock().unwrap() = Some(audio);
            Ok(warnings)
        }
        Err(e) => {
            let _ = app.hearth.voice_leave().await;
            Err(format!("couldn't start audio: {e:#}"))
        }
    }
}

#[tauri::command]
async fn voice_leave(app: State<'_, App>) -> Res<()> {
    *app.audio.lock().unwrap() = None;
    *app.video_out.lock().unwrap() = None;
    *app.speaking.lock().unwrap() = false;
    app.hearth.voice_leave().await.map_err(err)
}

#[tauri::command]
fn set_mute(app: State<'_, App>, muted: bool, deafened: bool) {
    if let Some(a) = app.audio.lock().unwrap().as_ref() {
        a.controls.muted.store(muted || deafened, std::sync::atomic::Ordering::Relaxed);
        a.controls.deafened.store(deafened, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Serialize)]
struct Levels {
    input_db: f32,
    transmitting: bool,
}

/// Our mic level, polled by the UI. Also tells peers when we start or stop talking.
#[tauri::command]
async fn voice_levels(app: State<'_, App>) -> Res<Option<Levels>> {
    let levels = {
        let guard = app.audio.lock().unwrap();
        guard.as_ref().map(|a| Levels {
            input_db: a.controls.input_db(),
            transmitting: a.controls.transmitting.load(std::sync::atomic::Ordering::Relaxed),
        })
    };
    if let Some(l) = &levels {
        let changed = {
            let mut s = app.speaking.lock().unwrap();
            let changed = *s != l.transmitting;
            *s = l.transmitting;
            changed
        };
        if changed {
            app.hearth.set_speaking(l.transmitting).await.map_err(err)?;
        }
    }
    Ok(levels)
}

#[derive(Serialize)]
struct VoiceRoomDto {
    server: String,
    channel: String,
    peers: Vec<String>,
    live: Vec<String>,
}

/// Everyone in voice right now, so a freshly opened window starts complete.
#[tauri::command]
async fn voice_snapshot(app: State<'_, App>) -> Res<Vec<VoiceRoomDto>> {
    Ok(app
        .hearth
        .voice_snapshot()
        .await
        .map_err(err)?
        .into_iter()
        .map(|r| VoiceRoomDto {
            server: r.server.to_string(),
            channel: eid(&r.channel),
            peers: r.peers.iter().map(|p| p.to_string()).collect(),
            live: r.live.iter().map(|p| p.to_string()).collect(),
        })
        .collect())
}

#[tauri::command]
async fn set_live(app: State<'_, App>, live: bool) -> Res<()> {
    app.hearth.set_live(live).await.map_err(err)
}

#[tauri::command]
async fn watch(app: State<'_, App>, peer: String, on: bool) -> Res<()> {
    app.hearth.watch(parse_peer(&peer)?, on).await.map_err(err)
}

#[tauri::command]
fn video_channel(app: State<'_, App>, channel: Channel<InvokeResponseBody>) {
    *app.video_in.lock().unwrap() = Some(channel);
}

/// One encoded frame from the page's encoder. Byte 0 bit 0 marks a keyframe.
/// Async so it runs off the UI thread.
#[tauri::command]
async fn push_video(app: State<'_, App>, request: Request<'_>) -> Res<usize> {
    let InvokeBody::Raw(frame) = request.body() else {
        return Err("expected raw bytes".into());
    };
    let keyframe = frame.first().is_some_and(|f| f & 1 == 1);
    let out = app.video_out.lock().unwrap();
    Ok(out.as_ref().map_or(0, |s| s.send_frame(frame, keyframe)))
}

// ---- updates from GitHub releases ----

/// An update found by `check_update`, kept until the user installs it.
#[derive(Default)]
struct PendingUpdate(Mutex<Option<tauri_plugin_updater::Update>>);

#[derive(Serialize)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: Option<String>,
}

/// Asks GitHub for a newer signed release.
#[tauri::command]
async fn check_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Res<Option<UpdateInfo>> {
    let Some(update) = app.updater().map_err(err)?.check().await.map_err(err)? else {
        return Ok(None);
    };
    let info = UpdateInfo { version: update.version.clone(), current: update.current_version.clone(), notes: update.body.clone() };
    *pending.0.lock().unwrap() = Some(update);
    Ok(Some(info))
}

/// Downloads the update, verifies its signature, installs it, and restarts.
#[tauri::command]
async fn install_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Res<()> {
    let update = pending.0.lock().unwrap().take().ok_or("no update to install")?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(err)?;
    app.restart()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(PendingUpdate::default())
        .setup(|app| {
            // HEARTH_DATA_DIR lets you run several identities side by side.
            let data_dir = match std::env::var_os("HEARTH_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => directories::ProjectDirs::from("", "", "hearth")
                    .ok_or("no home directory")?
                    .data_dir()
                    .to_path_buf(),
            };
            let secret = identity::load_or_create(&data_dir.join("identity.key"))?;
            let name = load_settings(&data_dir).name.unwrap_or_else(default_name);
            let cfg = Config { secret, name: name.clone(), data_dir: data_dir.clone(), relay: None, host: None };
            let (hearth, mut updates) = tauri::async_runtime::block_on(Hearth::start(cfg))?;

            let handle = app.handle().clone();
            let me = hearth.id();
            tauri::async_runtime::spawn(async move {
                while let Some(u) = updates.recv().await {
                    let _ = handle.emit("hearth", to_dto(u, me));
                }
            });

            app.manage(App {
                hearth,
                name: Mutex::new(name),
                data_dir,
                audio: Mutex::new(None),
                video_out: Mutex::new(None),
                video_in: Arc::new(Mutex::new(None)),
                speaking: Mutex::new(false),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            me,
            set_name,
            servers,
            names,
            online,
            history,
            send,
            edit,
            delete,
            react,
            typing,
            create_server,
            create_channel,
            invite,
            kick,
            leave_server,
            add_host,
            join,
            dial,
            friends,
            add_friend,
            remove_friend,
            block,
            upload,
            fetch_blob,
            save_attachment,
            set_avatar,
            avatars,
            notify,
            open_url,
            voice_join,
            voice_leave,
            set_mute,
            voice_levels,
            voice_snapshot,
            set_live,
            watch,
            video_channel,
            push_video,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hearth");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_file_names_stay_in_the_folder() {
        assert_eq!(safe_file_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_file_name("C:\\Windows\\x.dll"), "C__Windows_x.dll");
        assert_eq!(safe_file_name(".."), "file");
        assert_eq!(safe_file_name("cat.png"), "cat.png");
    }

    #[test]
    fn unique_paths_number_duplicates() {
        let dir = std::env::temp_dir().join(format!("hearth-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
