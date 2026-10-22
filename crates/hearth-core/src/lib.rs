//! hearth-core: identity, wire protocol, and peer-to-peer sessions.
//!
//! Every user is an ed25519 keypair. The public key *is* the address: iroh
//! dials peers by key, hole-punches where it can, and falls back to relays
//! that only ever see QUIC/TLS ciphertext authenticated to that key.

pub mod identity;
pub mod node;
pub mod proto;

pub use iroh::{EndpointId as PeerId, SecretKey};
pub use node::{Event, Node, Session};
pub use proto::Message;
