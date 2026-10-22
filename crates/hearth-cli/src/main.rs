use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hearth_core::{Event, Message, Node, PeerId, Session, identity};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(name = "hearth", about = "Peer-to-peer encrypted chat")]
struct Cli {
    /// Identity key file (defaults to the platform data dir).
    #[arg(long, global = true)]
    identity: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print your peer ID (share this with friends).
    Id,
    /// Go online and chat. Lines you type go to every connected peer.
    Chat {
        /// Display name shown to peers.
        #[arg(long, default_value = "anon")]
        name: String,
        /// Peer IDs to dial on startup.
        peers: Vec<PeerId>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match cli.identity {
        Some(p) => p,
        None => directories::ProjectDirs::from("", "", "hearth")
            .context("no home directory")?
            .data_dir()
            .join("identity.key"),
    };
    let secret = identity::load_or_create(&path)?;

    match cli.cmd {
        Cmd::Id => println!("{}", secret.public()),
        Cmd::Chat { name, peers } => chat(secret, name, peers).await?,
    }
    Ok(())
}

async fn chat(secret: hearth_core::SecretKey, name: String, peers: Vec<PeerId>) -> Result<()> {
    let (node, mut events) = Node::spawn(secret, name).await?;
    println!("your id: {}", node.id());
    node.online().await;
    println!("online. type to send, ctrl-d to quit.");

    for peer in peers {
        if let Err(e) = node.dial(peer).await {
            eprintln!("could not reach {}: {e:#}", peer.fmt_short());
        }
    }

    let mut sessions: HashMap<PeerId, Session> = HashMap::new();
    let mut names: HashMap<PeerId, String> = HashMap::new();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();

    loop {
        tokio::select! {
            line = stdin.next_line() => {
                let Some(line) = line? else { break };
                if line.trim().is_empty() { continue }
                if sessions.is_empty() {
                    println!("(nobody connected)");
                }
                let mut dead = Vec::new();
                for (peer, s) in sessions.iter_mut() {
                    if s.send_text(line.clone()).await.is_err() {
                        dead.push(*peer);
                    }
                }
                for peer in dead {
                    sessions.remove(&peer);
                }
            }
            Some(ev) = events.recv() => match ev {
                Event::Connected { peer, name, session } => {
                    println!("* {name} ({}) connected", peer.fmt_short());
                    names.insert(peer, name);
                    sessions.insert(peer, session);
                }
                Event::Message { peer, msg: Message::Text { body, .. } } => {
                    let who = names.get(&peer).map(String::as_str).unwrap_or("?");
                    println!("<{who}> {body}");
                }
                Event::Message { .. } => {}
                Event::Disconnected { peer } => {
                    let who = names.remove(&peer).unwrap_or_default();
                    sessions.remove(&peer);
                    println!("* {who} ({}) left", peer.fmt_short());
                }
            },
        }
    }

    for (_, s) in sessions {
        s.close();
    }
    node.shutdown().await
}
