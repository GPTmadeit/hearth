use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Connection, ConnectionError, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use tokio::sync::mpsc;

use crate::proto::{self, CHAT_ALPN, Message};

/// Something that happened on the network, delivered to the app layer.
#[derive(Debug)]
pub enum Event {
    Connected { peer: EndpointId, name: String, session: Session },
    Message { peer: EndpointId, msg: Message },
    Disconnected { peer: EndpointId },
}

/// The sending half of a live conversation with one peer.
#[derive(Debug)]
pub struct Session {
    peer: EndpointId,
    send: SendStream,
    conn: Connection,
}

impl Session {
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    pub async fn send_text(&mut self, body: impl Into<String>) -> Result<()> {
        let msg = Message::Text { body: body.into(), sent_at_ms: now_ms() };
        proto::write(&mut self.send, &msg).await
    }

    pub fn close(self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

pub struct Node {
    router: Router,
    name: String,
    events: mpsc::Sender<Event>,
}

impl Node {
    /// Binds an endpoint for `secret` and starts accepting chat connections.
    pub async fn spawn(secret: SecretKey, name: String) -> Result<(Self, mpsc::Receiver<Event>)> {
        let (tx, rx) = mpsc::channel(256);
        let endpoint = Endpoint::builder(presets::N0).secret_key(secret).bind().await?;
        let handler = ChatHandler { name: name.clone(), events: tx.clone() };
        let router = Router::builder(endpoint).accept(CHAT_ALPN, handler).spawn();
        Ok((Self { router, name, events: tx }, rx))
    }

    pub fn id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    /// Waits until we're reachable through a relay, so peers can dial us by key alone.
    pub async fn online(&self) {
        self.router.endpoint().online().await;
    }

    /// Dials `peer`; the resulting session arrives as [`Event::Connected`].
    pub async fn dial(&self, peer: EndpointId) -> Result<()> {
        let conn = self.router.endpoint().connect(peer, CHAT_ALPN).await?;
        let (send, recv) = conn.open_bi().await?;
        let (name, events) = (self.name.clone(), self.events.clone());
        tokio::spawn(async move {
            if let Err(e) = run_session(conn, send, recv, name, events).await {
                eprintln!("session with {} failed: {e:#}", peer.fmt_short());
            }
        });
        Ok(())
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ChatHandler {
    name: String,
    events: mpsc::Sender<Event>,
}

impl ProtocolHandler for ChatHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (send, recv) = conn.accept_bi().await?;
        let peer = conn.remote_id();
        if let Err(e) = run_session(conn, send, recv, self.name.clone(), self.events.clone()).await {
            eprintln!("session with {} failed: {e:#}", peer.fmt_short());
        }
        Ok(())
    }
}

/// Exchanges hellos, hands the send half to the app, then pumps incoming
/// messages until the peer goes away.
async fn run_session(
    conn: Connection,
    mut send: SendStream,
    mut recv: iroh::endpoint::RecvStream,
    our_name: String,
    events: mpsc::Sender<Event>,
) -> Result<()> {
    let peer = conn.remote_id();
    proto::write(&mut send, &Message::Hello { name: our_name }).await?;
    let name = match proto::read(&mut recv).await? {
        Some(Message::Hello { name }) => name,
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };

    let session = Session { peer, send, conn: conn.clone() };
    events.send(Event::Connected { peer, name, session }).await?;

    let reader = async {
        while let Some(msg) = proto::read(&mut recv).await? {
            events.send(Event::Message { peer, msg }).await?;
        }
        anyhow::Ok(())
    };
    let mut result = reader.await;
    // A peer saying goodbye (or us hanging up) isn't a failure.
    if matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(_) | ConnectionError::LocallyClosed)
    ) {
        result = Ok(());
    }
    let _ = events.send(Event::Disconnected { peer }).await;
    result
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
