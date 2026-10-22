// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hearth_core::{
    Change, ChannelKind, Config, EventId, Hearth, MediaSender, PeerId, ServerId, ServerSummary, Update, identity,
};
use hearth_media::{Audio, AudioOptions};
use serde::{Deserialize, Serialize};
use tauri::{
    Emitter, Manager, State,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request},
};

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

// ---- wire types for the UI (ids are hex strings, channels are "author:seq") ----

fn eid(id: &EventId) -> String {
    format!("{}:{}", id.author, id.seq)
}

fn parse_eid(s: &str) -> Res<EventId> {
    let (a, n) = s.split_once(':').ok_or("bad channel id")?;
    Ok(EventId { author: a.parse().map_err(err)?, seq: n.parse().map_err(err)? })
}

fn parse_sid(s: &str) -> Res<ServerId> {
    s.parse().map_err(err)
}

fn parse_peer(s: &str) -> Res<PeerId> {
    s.trim().parse().map_err(|_| "that doesn't look like a peer ID".to_string())
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
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UpdateDto {
    PeerOnline { peer: String, name: String },
    PeerOffline { peer: String },
    Dm { peer: String, body: String, sent_at_ms: u64 },
    Invited { server: String, from: String, name: String },
    Message { server: String, message: MessageDto },
    ServerChanged { server: String },
    Removed { server: String },
    Voice { server: String, channel: String, peers: Vec<String>, live: Vec<String> },
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
        Update::Server { server, change: Change::Message(m) } => UpdateDto::Message {
            server: server.to_string(),
            message: MessageDto {
                id: eid(&m.id),
                channel: eid(&m.channel),
                author: m.author.to_string(),
                body: m.body,
                sent_at_ms: m.sent_at_ms,
            },
        },
        Update::Server { server, change: Change::MemberRemoved(p) } if p == me => {
            UpdateDto::Removed { server: server.to_string() }
        }
        Update::Server { server, .. } => UpdateDto::ServerChanged { server: server.to_string() },
        Update::Voice { server, channel, peers, live } => UpdateDto::Voice {
            server: server.to_string(),
            channel: eid(&channel),
            peers: peers.iter().map(|p| p.to_string()).collect(),
            live: live.iter().map(|p| p.to_string()).collect(),
        },
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
}

fn default_name() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "anon".into())
}

fn load_settings(dir: &std::path::Path) -> Settings {
    std::fs::read(dir.join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

// ---- commands ----

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
fn set_name(app: State<'_, App>, name: String) -> Res<()> {
    let name: String = name.trim().chars().take(64).collect();
    if name.is_empty() {
        return Err("name can't be empty".into());
    }
    let settings = Settings { name: Some(name.clone()) };
    std::fs::write(app.data_dir.join("settings.json"), serde_json::to_vec_pretty(&settings).map_err(err)?)
        .map_err(err)?;
    *app.name.lock().unwrap() = name;
    Ok(())
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
    let msgs = app.hearth.history(sid, cid, limit).await.map_err(err)?;
    Ok(msgs
        .into_iter()
        .map(|m| MessageDto {
            id: eid(&m.id),
            channel: channel.clone(),
            author: m.author.to_string(),
            body: m.body,
            sent_at_ms: m.sent_at_ms,
        })
        .collect())
}

#[tauri::command]
async fn send(app: State<'_, App>, server: String, channel: String, body: String) -> Res<()> {
    app.hearth.send(parse_sid(&server)?, parse_eid(&channel)?, body).await.map_err(err)
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

#[tauri::command]
async fn dm(app: State<'_, App>, peer: String, body: String) -> Res<()> {
    app.hearth.dm(parse_peer(&peer)?, body).await.map_err(err)
}

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
    speaking: Vec<String>,
}

#[tauri::command]
fn voice_levels(app: State<'_, App>) -> Option<Levels> {
    let guard = app.audio.lock().unwrap();
    let a = guard.as_ref()?;
    Some(Levels {
        input_db: a.controls.input_db(),
        transmitting: a.controls.transmitting.load(std::sync::atomic::Ordering::Relaxed),
        speaking: a.speaking().into_iter().filter(|(_, db)| *db > -45.0).map(|(p, _)| p.to_string()).collect(),
    })
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
#[tauri::command]
fn push_video(app: State<'_, App>, request: Request<'_>) -> Res<usize> {
    let InvokeBody::Raw(frame) = request.body() else {
        return Err("expected raw bytes".into());
    };
    let keyframe = frame.first().is_some_and(|f| f & 1 == 1);
    let out = app.video_out.lock().unwrap();
    Ok(out.as_ref().map_or(0, |s| s.send_frame(frame, keyframe)))
}

fn main() {
    tauri::Builder::default()
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
            create_server,
            create_channel,
            invite,
            kick,
            add_host,
            join,
            dial,
            dm,
            voice_join,
            voice_leave,
            set_mute,
            voice_levels,
            set_live,
            watch,
            video_channel,
            push_video,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hearth");
}
