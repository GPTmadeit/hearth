//! hearth-core: identity, servers, sync, and the peer-to-peer engine.
//!
//! Every user is an ed25519 keypair, and the public key *is* the address:
//! iroh dials peers by key, hole-punches where it can, and falls back to relays
//! that only see QUIC/TLS ciphertext. Servers are replicated logs of signed
//! events whose content is sealed under a per-epoch group key.

pub mod blob;
pub mod crypto;
pub mod engine;
pub mod event;
pub mod identity;
pub mod media;
pub mod proto;
pub mod state;
pub mod store;

pub use engine::{Config, FriendView, Hearth, HostPolicy, ServerSummary, Update, VoiceRoom};
pub use blob::BlobRef;
pub use event::Attachment;
pub use store::FriendState;
pub use iroh::RelayUrl;
pub use media::{KIND_AUDIO, KIND_VIDEO, MAX_VIDEO_FRAME, MediaPacket, MediaSender, VoiceSession};
pub use event::{ChannelKind, EventId, ServerId};
pub use iroh::{EndpointId as PeerId, SecretKey};
pub use state::{Change, Channel, ChatMessage};
