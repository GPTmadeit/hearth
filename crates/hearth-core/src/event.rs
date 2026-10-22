//! Signed events: the unit of replication for a server.
//!
//! Each author numbers their events per server starting at 1, so a peer's
//! knowledge of a server is just `author -> highest contiguous seq`. Admin ops
//! stay in plaintext so a blind storage node can enforce membership without
//! reading content; everything else is sealed under the current epoch key.

use std::fmt;

use anyhow::{Result, bail};
use iroh::{EndpointId as PeerId, SecretKey, Signature};
use serde::{Deserialize, Serialize};

use crate::crypto::{self, EpochKey};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServerId(pub [u8; 32]);

impl ServerId {
    pub fn derive(owner: &PeerId, nonce: &[u8; 16]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"hearth/server/v0");
        h.update(owner.as_bytes());
        h.update(nonce);
        Self(*h.finalize().as_bytes())
    }

    pub fn short(&self) -> String {
        self.to_string()[..10].to_string()
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerId({})", self.short())
    }
}

impl std::str::FromStr for ServerId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        if s.len() != 64 {
            bail!("server id must be 64 hex characters");
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
        }
        Ok(Self(out))
    }
}

/// Identifies an event (and anything it creates, such as a channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventId {
    pub author: PeerId,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AdminOp {
    /// The owner's first event; `ServerId::derive(owner, nonce)` must match.
    /// `key_hash` commits to the epoch-0 key so granted keys can be checked.
    Genesis { nonce: [u8; 16], key_hash: [u8; 32] },
    Add { peer: PeerId },
    /// `through_seq` is the last of the peer's events the owner accepts.
    Remove { peer: PeerId, through_seq: u64 },
    /// A new epoch key exists; members receive it out of band via key grants.
    Rotate { epoch: u32, key_hash: [u8; 32] },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Body {
    Admin(AdminOp),
    Sealed {
        epoch: u32,
        /// The owner's seq the author had seen, which pins the membership the
        /// event is judged against regardless of arrival order.
        admin_seq: u64,
        nonce: [u8; 24],
        ciphertext: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChannelKind {
    Text,
    Voice,
}

/// What a sealed event decrypts to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Rename { name: String },
    CreateChannel { name: String, kind: ChannelKind },
    Message { channel: EventId, body: String, sent_at_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub server: ServerId,
    pub author: PeerId,
    pub seq: u64,
    pub body: Body,
    pub sig: Signature,
}

#[derive(Serialize)]
struct SigningView<'a> {
    domain: &'static str,
    server: &'a ServerId,
    author: &'a PeerId,
    seq: u64,
    body: &'a Body,
}

fn signing_bytes(server: &ServerId, author: &PeerId, seq: u64, body: &Body) -> Vec<u8> {
    let view = SigningView { domain: "hearth/event/v0", server, author, seq, body };
    postcard::to_stdvec(&view).expect("serializing to a Vec cannot fail")
}

fn sealed_aad(server: &ServerId, author: &PeerId, seq: u64, epoch: u32, admin_seq: u64) -> Vec<u8> {
    postcard::to_stdvec(&(server, author, seq, epoch, admin_seq)).expect("serializing to a Vec cannot fail")
}

impl Event {
    pub fn id(&self) -> EventId {
        EventId { author: self.author, seq: self.seq }
    }

    pub fn sign(secret: &SecretKey, server: ServerId, seq: u64, body: Body) -> Self {
        let author = secret.public();
        let sig = secret.sign(&signing_bytes(&server, &author, seq, &body));
        Self { server, author, seq, body, sig }
    }

    pub fn seal(
        secret: &SecretKey,
        server: ServerId,
        seq: u64,
        epoch: u32,
        admin_seq: u64,
        key: &EpochKey,
        content: &Content,
    ) -> Self {
        let nonce = crypto::random_nonce();
        let aad = sealed_aad(&server, &secret.public(), seq, epoch, admin_seq);
        let plaintext = postcard::to_stdvec(content).expect("serializing to a Vec cannot fail");
        let ciphertext = crypto::seal(key, &nonce, &aad, &plaintext);
        Self::sign(secret, server, seq, Body::Sealed { epoch, admin_seq, nonce, ciphertext })
    }

    pub fn verify(&self) -> Result<()> {
        if self.seq == 0 {
            bail!("seq starts at 1");
        }
        let bytes = signing_bytes(&self.server, &self.author, self.seq, &self.body);
        self.author.verify(&bytes, &self.sig)?;
        Ok(())
    }

    /// Decrypts a sealed event. Returns `None` for admin events.
    pub fn open(&self, key: &EpochKey) -> Result<Option<Content>> {
        let Body::Sealed { epoch, admin_seq, nonce, ciphertext } = &self.body else {
            return Ok(None);
        };
        let aad = sealed_aad(&self.server, &self.author, self.seq, *epoch, *admin_seq);
        let plaintext = crypto::open(key, nonce, &aad, ciphertext)?;
        Ok(Some(postcard::from_bytes(&plaintext)?))
    }
}
