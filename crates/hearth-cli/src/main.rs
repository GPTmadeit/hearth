use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hearth_core::{
    Change, ChannelKind, Config, EventId, Hearth, HostPolicy, PeerId, RelayUrl, ServerId, ServerSummary, Update,
    identity,
};
use hearth_media::{Audio, AudioOptions, Input, Output};
use tokio::io::{AsyncBufReadExt, BufReader};

mod update;

#[derive(Parser)]
#[command(name = "hearth", about = "Peer-to-peer encrypted chat")]
struct Cli {
    /// Where your identity and data live (defaults to the platform data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Use your own relay (e.g. https://relay.example.com) instead of the public ones.
    #[arg(long, global = true)]
    relay: Option<RelayUrl>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print your peer ID (share this with friends).
    Id,
    /// Go online. Type /help once running.
    Chat {
        /// Display name shown to peers.
        #[arg(long, default_value = "anon")]
        name: String,
        /// Peer IDs to dial on startup.
        peers: Vec<PeerId>,
        /// Send a test tone instead of your microphone in voice channels.
        #[arg(long, value_name = "HZ")]
        tone: Option<f32>,
        /// Write voice audio to this WAV file instead of the speakers.
        #[arg(long, value_name = "FILE")]
        record: Option<PathBuf>,
        /// /live streams synthetic frames, and received frames are verified and counted.
        #[arg(long)]
        video_test: bool,
    },
    /// Run an always-on storage node. It stores and forwards servers'
    /// encrypted events for offline delivery, but never holds their keys.
    Node {
        /// Only host servers for these owners (default: anyone who asks).
        #[arg(long)]
        allow: Vec<PeerId>,
        /// Don't install new releases from GitHub automatically.
        #[arg(long)]
        no_auto_update: bool,
    },
    /// Update hearth to the latest signed release from GitHub.
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
    },
}

const HELP: &str = "\
commands:
  /servers                 list servers          /use <n> [channel]   switch server/channel
  /new <name>              create a server       /channels            list channels
  /channel <name> [voice]  create a channel      /members             list members
  /invite <peer>           invite to server      /kick <peer>         remove + rotate key
  /invites                 pending invites       /accept <n>          join an invite
  /host <node>             add a storage node    /unhost <node>       remove it
  /voice [channel]         join voice           /leave               leave voice
  /mute  /deafen           toggle mic/sound      /dm <peer> <text>    direct message
  /live  /unlive           stream (--video-test) /watch <peer>        watch a stream
  /peers                   who's online
  /history [n]             recent messages       /id                  your peer id
  /quit
anything else is sent to the current channel. <peer> can be an id prefix or a name.";

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = match cli.data_dir {
        Some(p) => p,
        None => directories::ProjectDirs::from("", "", "hearth").context("no home directory")?.data_dir().to_path_buf(),
    };
    let secret = identity::load_or_create(&data_dir.join("identity.key"))?;

    match cli.cmd {
        Cmd::Id => println!("{}", secret.public()),
        Cmd::Update { check } => {
            println!("hearth {}", env!("CARGO_PKG_VERSION"));
            match update::check().await? {
                None => println!("up to date"),
                Some(u) if check => println!("{} is available: run `hearth update`", u.version),
                Some(u) => {
                    println!("downloading {}…", u.version);
                    update::install(&u).await?;
                    println!("updated to {}", u.version);
                }
            }
        }
        Cmd::Chat { name, peers, tone, record, video_test } => {
            notify_if_outdated();
            let cfg = Config { secret, name, data_dir, relay: cli.relay, host: None };
            let (hearth, updates) = Hearth::start(cfg).await?;
            let mut repl = Repl::new(hearth);
            repl.tone = tone;
            repl.record = record;
            repl.video_test = video_test;
            repl.run(updates, peers).await?;
        }
        Cmd::Node { allow, no_auto_update } => {
            let host = Some(HostPolicy { allow });
            let cfg = Config { secret, name: "hearth-node".into(), data_dir, relay: cli.relay, host };
            run_node(cfg, !no_auto_update).await?;
        }
    }
    Ok(())
}

/// Prints a one-line notice if GitHub has a newer release. Never blocks startup.
fn notify_if_outdated() {
    if std::env::var_os("HEARTH_NO_UPDATE_CHECK").is_some() {
        return;
    }
    tokio::spawn(async {
        if let Ok(Ok(Some(u))) = tokio::time::timeout(std::time::Duration::from_secs(10), update::check()).await {
            println!("* hearth {} is available: run `hearth update`", u.version);
        }
    });
}

const NODE_UPDATE_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

async fn run_node(cfg: Config, auto_update: bool) -> Result<()> {
    let (hearth, mut updates) = Hearth::start(cfg).await?;
    println!("node id: {}", hearth.id());
    hearth.online().await;
    println!("online. owners can now run `/host {}`", hearth.id());
    if auto_update {
        println!("auto-update on: checking GitHub every 6 hours (--no-auto-update to disable)");
    }
    let mut update_tick = tokio::time::interval_at(tokio::time::Instant::now() + std::time::Duration::from_secs(60), NODE_UPDATE_EVERY);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = update_tick.tick(), if auto_update => match update::check().await {
                Ok(Some(u)) => match update::install(&u).await {
                    Ok(()) => {
                        println!("installed {}; restarting", u.version);
                        hearth.shutdown().await;
                        update::restart()?;
                    }
                    Err(e) => eprintln!("update to {} failed: {e:#}", u.version),
                },
                Ok(None) => {}
                Err(e) => eprintln!("update check failed: {e:#}"),
            },
            Some(u) = updates.recv() => match u {
                Update::Invited { server, from, .. } => println!("hosting {} for {}", server.short(), from.fmt_short()),
                Update::PeerOnline { peer, name } => println!("+ {name} {}", peer.fmt_short()),
                Update::PeerOffline { peer } => println!("- {}", peer.fmt_short()),
                Update::Server { server, change: Change::HostRemoved(p) } if p == hearth.id() => {
                    println!("no longer hosting {}", server.short())
                }
                _ => {}
            },
        }
    }
    hearth.shutdown().await;
    Ok(())
}

const TEST_FRAME: usize = 50_000;

/// A frame whose bytes are derived from its number, so receivers can verify it.
fn test_frame(n: u32, key: bool) -> Vec<u8> {
    let mut f = Vec::with_capacity(TEST_FRAME);
    f.push(key as u8);
    f.extend_from_slice(&n.to_be_bytes());
    f.extend((0..TEST_FRAME - 5).map(|i| (i as u32).wrapping_mul(31).wrapping_add(n) as u8));
    f
}

fn video_test_receiver() -> Box<dyn Fn(hearth_core::MediaPacket) + Send> {
    // (frames received, frames that failed verification)
    let state = std::sync::Mutex::new((0u32, 0u32));
    Box::new(move |p| {
        if p.kind != hearth_core::KIND_VIDEO || p.payload.len() < 5 {
            return;
        }
        let n = u32::from_be_bytes(p.payload[1..5].try_into().expect("4 bytes"));
        let ok = p.payload == test_frame(n, p.payload[0] == 1);
        let mut s = state.lock().expect("video stats");
        s.0 += 1;
        if !ok {
            s.1 += 1;
        }
        if s.0.is_multiple_of(30) {
            println!("[video] {} frames from {} ({} KB each), {} corrupt", s.0, p.from.fmt_short(), p.payload.len() / 1000, s.1);
        }
    })
}

struct Repl {
    hearth: Hearth,
    names: HashMap<PeerId, String>,
    invites: Vec<(ServerId, PeerId, String)>,
    current: Option<(ServerId, EventId)>,
    audio: Option<Audio>,
    tone: Option<f32>,
    record: Option<PathBuf>,
    video_test: bool,
    streaming: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Repl {
    fn new(hearth: Hearth) -> Self {
        Self { hearth, names: HashMap::new(), invites: Vec::new(), current: None, audio: None, tone: None, record: None, video_test: false, streaming: None }
    }

    async fn run(mut self, mut updates: tokio::sync::mpsc::UnboundedReceiver<Update>, peers: Vec<PeerId>) -> Result<()> {
        println!("your id: {}", self.hearth.id());
        self.names.extend(self.hearth.known_names().await?);
        self.hearth.online().await;
        println!("online. /help for commands.");
        for p in peers {
            self.hearth.dial(p).await?;
        }
        if let Some(s) = self.hearth.servers().await?.first() {
            self.select(s, None);
        }

        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        loop {
            tokio::select! {
                line = stdin.next_line() => {
                    let Some(line) = line? else { break };
                    match self.command(line.trim()).await {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) => println!("! {e:#}"),
                    }
                }
                Some(u) = updates.recv() => self.update(u).await,
            }
        }
        self.audio = None;
        self.hearth.shutdown().await;
        Ok(())
    }

    fn who(&self, peer: &PeerId) -> String {
        if *peer == self.hearth.id() {
            return "you".into();
        }
        self.names.get(peer).cloned().unwrap_or_else(|| peer.fmt_short().to_string())
    }

    fn select(&mut self, s: &ServerSummary, channel: Option<&str>) {
        let text = s.channels.iter().filter(|c| c.kind == ChannelKind::Text);
        let ch = match channel {
            Some(name) => text.clone().find(|c| c.name == name.trim_start_matches('#')),
            None => text.clone().next(),
        };
        match ch {
            Some(ch) => {
                self.current = Some((s.id, ch.id));
                println!("-> {} #{}", s.name.as_deref().unwrap_or("?"), ch.name);
            }
            None => println!("! no such text channel"),
        }
    }

    async fn current_server(&self) -> Result<ServerSummary> {
        let (id, _) = self.current.context("no server selected (/servers, /use)")?;
        self.hearth.servers().await?.into_iter().find(|s| s.id == id).context("server vanished")
    }

    async fn resolve_peer(&self, needle: &str) -> Result<PeerId> {
        if let Ok(id) = needle.parse() {
            return Ok(id);
        }
        let mut known: Vec<PeerId> = self.names.keys().copied().collect();
        for s in self.hearth.servers().await? {
            known.extend(s.members);
        }
        known.sort();
        known.dedup();
        let hits: Vec<PeerId> = known
            .into_iter()
            .filter(|p| p.to_string().starts_with(needle) || self.names.get(p).is_some_and(|n| n == needle))
            .collect();
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => bail!("no peer matches {needle:?}"),
            _ => bail!("{needle:?} is ambiguous"),
        }
    }

    /// Returns true to quit.
    async fn command(&mut self, line: &str) -> Result<bool> {
        if line.is_empty() {
            return Ok(false);
        }
        if !line.starts_with('/') {
            let (server, channel) = self.current.context("no channel selected (/servers, /use)")?;
            self.hearth.send(server, channel, line.to_string()).await?;
            return Ok(false);
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match cmd {
            "/quit" | "/q" => return Ok(true),
            "/help" | "/h" => println!("{HELP}"),
            "/id" => println!("{}", self.hearth.id()),
            "/servers" => {
                for (i, s) in self.hearth.servers().await?.iter().enumerate() {
                    let mark = if self.current.is_some_and(|c| c.0 == s.id) { "*" } else { " " };
                    let name = s.name.as_deref().unwrap_or("(syncing...)");
                    println!("{mark}{i}: {name}  [{} members, {}]", s.members.len(), s.id.short());
                }
            }
            "/new" => {
                if rest.is_empty() {
                    bail!("usage: /new <name>");
                }
                let id = self.hearth.create_server(rest.to_string()).await?;
                let s = self.hearth.servers().await?.into_iter().find(|s| s.id == id).context("created server missing")?;
                println!("created {rest} ({})", id);
                self.select(&s, None);
            }
            "/use" => {
                let (n, ch) = rest.split_once(' ').unwrap_or((rest, ""));
                let servers = self.hearth.servers().await?;
                let s = servers.get(n.parse::<usize>().context("usage: /use <n> [channel]")?).context("no such server")?;
                self.select(s, (!ch.is_empty()).then_some(ch));
            }
            "/channels" => {
                let s = self.current_server().await?;
                for c in &s.channels {
                    let kind = if c.kind == ChannelKind::Voice { "🔊" } else { "#" };
                    println!("  {kind}{}", c.name);
                }
            }
            "/channel" => {
                let s = self.current_server().await?;
                let (name, kind) = match rest.split_once(' ') {
                    Some((n, "voice")) => (n, ChannelKind::Voice),
                    _ => (rest, ChannelKind::Text),
                };
                if name.is_empty() {
                    bail!("usage: /channel <name> [voice]");
                }
                self.hearth.create_channel(s.id, name.to_string(), kind).await?;
            }
            "/members" => {
                let s = self.current_server().await?;
                let online = self.hearth.online_peers().await?;
                for m in &s.members {
                    let crown = if Some(*m) == s.owner { " (owner)" } else { "" };
                    let dot = if online.iter().any(|(p, _)| p == m) || *m == self.hearth.id() { "●" } else { "○" };
                    println!("  {dot} {}{crown}  {}", self.who(m), m.fmt_short());
                }
                for h in &s.hosts {
                    let dot = if online.iter().any(|(p, _)| p == h) { "●" } else { "○" };
                    println!("  {dot} [node] {}", h.fmt_short());
                }
            }
            "/invite" => {
                let s = self.current_server().await?;
                let peer = self.resolve_peer(rest).await?;
                self.hearth.invite(s.id, peer).await?;
                println!("invited {}", self.who(&peer));
            }
            "/kick" => {
                let s = self.current_server().await?;
                let peer = self.resolve_peer(rest).await?;
                self.hearth.kick(s.id, peer).await?;
                println!("removed {}; rotated server key", self.who(&peer));
            }
            "/host" => {
                let s = self.current_server().await?;
                let peer: PeerId = rest.parse().context("usage: /host <node-id>")?;
                self.hearth.add_host(s.id, peer).await?;
                println!("asked {} to host {}", peer.fmt_short(), s.name.as_deref().unwrap_or("?"));
            }
            "/unhost" => {
                let s = self.current_server().await?;
                let peer = self.resolve_peer(rest).await?;
                self.hearth.remove_host(s.id, peer).await?;
            }
            "/invites" => {
                if self.invites.is_empty() {
                    println!("no pending invites");
                }
                for (i, (_, from, name)) in self.invites.iter().enumerate() {
                    println!("  {i}: {name} from {}", self.who(from));
                }
            }
            "/accept" => {
                let n: usize = if rest.is_empty() { 0 } else { rest.parse()? };
                if n >= self.invites.len() {
                    bail!("no such invite");
                }
                let (server, from, name) = self.invites.remove(n);
                self.hearth.join(server, from).await?;
                println!("joining {name}...");
            }
            "/voice" | "/v" => {
                let s = self.current_server().await?;
                let voice = s.channels.iter().filter(|c| c.kind == ChannelKind::Voice);
                let ch = match rest {
                    "" => voice.clone().next(),
                    name => voice.clone().find(|c| c.name == name),
                }
                .context("no such voice channel")?;
                self.audio = None;
                let session = self.hearth.voice_join(s.id, ch.id).await?;
                let opts = AudioOptions {
                    input: self.tone.map_or(Input::Microphone, |hz| Input::Tone { hz }),
                    output: self.record.clone().map_or(Output::Speakers, Output::Wav),
                    on_media: self.video_test.then(video_test_receiver),
                    ..Default::default()
                };
                match Audio::start(session, opts) {
                    Ok(a) => {
                        for w in &a.warnings {
                            println!("! {w}");
                        }
                        self.audio = Some(a);
                    }
                    Err(e) => {
                        self.hearth.voice_leave().await?;
                        return Err(e.context("starting audio"));
                    }
                }
                println!("🔊 joined {} (/mute, /deafen, /leave)", ch.name);
            }
            "/live" => {
                let a = self.audio.as_ref().context("join voice first")?;
                if !self.video_test {
                    bail!("the CLI has no screen capture; run with --video-test to stream synthetic frames");
                }
                self.hearth.set_live(true).await?;
                let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (sender, flag) = (a.sender(), stop.clone());
                // A plain OS thread, like a UI thread pushing encoded frames: send_frame
                // must work outside the async runtime.
                std::thread::spawn(move || {
                    let mut n: u32 = 0;
                    while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(33));
                        let key = n.is_multiple_of(60);
                        sender.send_frame(&test_frame(n, key), key);
                        n += 1;
                    }
                });
                self.streaming = Some(stop);
                println!("streaming synthetic video");
            }
            "/unlive" => {
                if let Some(s) = self.streaming.take() {
                    s.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.hearth.set_live(false).await?;
            }
            "/watch" => {
                let peer = self.resolve_peer(rest).await?;
                self.hearth.watch(peer, true).await?;
                println!("watching {}", self.who(&peer));
            }
            "/leave" => {
                if let Some(s) = self.streaming.take() {
                    s.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.audio = None;
                self.hearth.voice_leave().await?;
                println!("left voice");
            }
            "/mute" | "/deafen" => {
                let a = self.audio.as_ref().context("not in voice")?;
                let flag = if cmd == "/mute" { &a.controls.muted } else { &a.controls.deafened };
                let on = !flag.load(std::sync::atomic::Ordering::Relaxed);
                flag.store(on, std::sync::atomic::Ordering::Relaxed);
                println!("{} {}", &cmd[1..], if on { "on" } else { "off" });
            }
            "/dm" => {
                let (who, text) = rest.split_once(' ').context("usage: /dm <peer> <text>")?;
                let peer = self.resolve_peer(who).await?;
                self.hearth.dm(peer, text.to_string()).await?;
            }
            "/peers" => {
                for (p, name) in self.hearth.online_peers().await? {
                    println!("  ● {name}  {p}");
                }
            }
            "/history" => {
                let (server, channel) = self.current.context("no channel selected")?;
                let n = if rest.is_empty() { 20 } else { rest.parse()? };
                for m in self.hearth.history(server, channel, n).await? {
                    println!("<{}> {}", self.who(&m.author), m.body);
                }
            }
            _ => bail!("unknown command {cmd} (/help)"),
        }
        Ok(false)
    }

    async fn update(&mut self, u: Update) {
        match u {
            Update::PeerOnline { peer, name } => {
                println!("* {name} ({}) is online", peer.fmt_short());
                self.names.insert(peer, name);
            }
            Update::PeerOffline { peer } => println!("* {} went offline", self.who(&peer)),
            Update::Dm { peer, body, .. } => println!("[dm] <{}> {body}", self.who(&peer)),
            Update::Invited { server, from, name } => {
                if !self.invites.iter().any(|(s, ..)| *s == server) {
                    println!("* {} invited you to {name}. /accept {} to join", self.who(&from), self.invites.len());
                    self.invites.push((server, from, name));
                }
            }
            Update::Server { server, change } => self.server_change(server, change).await,
            Update::KeyframeRequested => {}
            Update::Voice { server, channel, peers, .. } => {
                let Some(s) = self.hearth.servers().await.ok().and_then(|v| v.into_iter().find(|s| s.id == server)) else {
                    return;
                };
                let ch = s.channels.iter().find(|c| c.id == channel).map(|c| c.name.clone()).unwrap_or_default();
                let names: Vec<String> = peers.iter().map(|p| self.who(p)).collect();
                println!("* 🔊{ch}: {}", if names.is_empty() { "(empty)".into() } else { names.join(", ") });
            }
        }
    }

    async fn server_change(&mut self, server: ServerId, change: Change) {
        let summary = self.hearth.servers().await.ok().and_then(|v| v.into_iter().find(|s| s.id == server));
        let sname = summary.as_ref().and_then(|s| s.name.clone()).unwrap_or_else(|| server.short());
        match change {
            Change::Message(m) => {
                if m.author == self.hearth.id() {
                    return;
                }
                if self.current == Some((server, m.channel)) {
                    println!("<{}> {}", self.who(&m.author), m.body);
                } else {
                    let ch = summary
                        .and_then(|s| s.channels.into_iter().find(|c| c.id == m.channel))
                        .map(|c| c.name)
                        .unwrap_or_else(|| "?".into());
                    println!("[{sname} #{ch}] <{}> {}", self.who(&m.author), m.body);
                }
            }
            Change::Renamed(name) => {
                println!("* server {name} synced");
                if self.current.is_none()
                    && let Some(s) = summary
                {
                    self.select(&s, None);
                }
            }
            Change::Channel(c) => {
                if self.current.is_none()
                    && let Some(s) = summary.filter(|_| c.kind == ChannelKind::Text)
                {
                    self.select(&s, Some(&c.name));
                }
            }
            Change::MemberAdded(p) => println!("* {} joined {sname}", self.who(&p)),
            Change::MemberRemoved(p) => {
                if p == self.hearth.id() {
                    println!("* you were removed from {sname}");
                    if self.current.is_some_and(|c| c.0 == server) {
                        self.current = None;
                    }
                } else {
                    println!("* {} was removed from {sname}", self.who(&p));
                }
            }
            Change::Epoch(e) => println!("* {sname} rotated to key epoch {e}"),
            Change::HostAdded(p) => println!("* {sname} is now stored on node {}", p.fmt_short()),
            Change::HostRemoved(p) => println!("* node {} no longer stores {sname}", p.fmt_short()),
        }
    }
}
