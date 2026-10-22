use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hearth_core::{
    Change, ChannelKind, Config, EventId, Hearth, PeerId, ServerId, ServerSummary, Update, identity,
};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(name = "hearth", about = "Peer-to-peer encrypted chat")]
struct Cli {
    /// Where your identity and data live (defaults to the platform data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
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
    },
}

const HELP: &str = "\
commands:
  /servers                 list servers          /use <n> [channel]   switch server/channel
  /new <name>              create a server       /channels            list channels
  /channel <name> [voice]  create a channel      /members             list members
  /invite <peer>           invite to server      /kick <peer>         remove + rotate key
  /invites                 pending invites       /accept <n>          join an invite
  /dm <peer> <text>        direct message        /peers               who's online
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
        Cmd::Chat { name, peers } => {
            let (hearth, updates) = Hearth::start(Config { secret, name, data_dir }).await?;
            Repl::new(hearth).run(updates, peers).await?;
        }
    }
    Ok(())
}

struct Repl {
    hearth: Hearth,
    names: HashMap<PeerId, String>,
    invites: Vec<(ServerId, PeerId, String)>,
    current: Option<(ServerId, EventId)>,
}

impl Repl {
    fn new(hearth: Hearth) -> Self {
        Self { hearth, names: HashMap::new(), invites: Vec::new(), current: None }
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
        }
    }
}
