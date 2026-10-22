//! Real-time media transport for voice channels.
//!
//! Audio packets travel as QUIC datagrams on the existing peer connections,
//! so there's no extra handshake and no retransmission delay. Video frames
//! are too big for a datagram, so each one gets its own unidirectional QUIC
//! stream: a lost packet delays only that frame, never the next. Each packet is
//! additionally sealed with a key derived from the server's epoch key and the
//! channel, so only members can decode media even if it is ever forwarded.
//!
//! Datagram layout: `version(1) | tag(8) | epoch(4) | nonce(24) | ciphertext`,
//! where the plaintext is `kind(1) | seq(4) | payload`.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

use bytes::Bytes;
use iroh::{EndpointId as PeerId, endpoint::Connection};

use crate::{
    crypto::{self, EpochKey},
    event::{EventId, ServerId},
};

const VERSION: u8 = 0;
const HEADER: usize = 1 + 8 + 4 + 24;

/// Opus voice frames.
pub const KIND_AUDIO: u8 = 1;
/// Encoded screen-share frames.
pub const KIND_VIDEO: u8 = 2;

/// Video frames allowed in flight per viewer before non-key frames are skipped.
const MAX_INFLIGHT: usize = 4;
pub const MAX_VIDEO_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MediaPacket {
    pub from: PeerId,
    pub kind: u8,
    pub seq: u32,
    pub payload: Vec<u8>,
}

/// Identifies a voice channel on the wire without revealing which one.
fn room_tag(server: &ServerId, channel: &EventId) -> [u8; 8] {
    let mut h = blake3::Hasher::new_derive_key("hearth/media-tag/v0");
    h.update(&server.0);
    h.update(channel.author.as_bytes());
    h.update(&channel.seq.to_be_bytes());
    h.finalize().as_bytes()[..8].try_into().expect("8 bytes")
}

fn media_key(epoch_key: &EpochKey, server: &ServerId, channel: &EventId) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("hearth/media-key/v0");
    h.update(epoch_key);
    h.update(&server.0);
    h.update(channel.author.as_bytes());
    h.update(&channel.seq.to_be_bytes());
    *h.finalize().as_bytes()
}

fn aad(tag: &[u8; 8], epoch: u32, sender: &PeerId) -> [u8; 44] {
    let mut out = [0u8; 44];
    out[..8].copy_from_slice(tag);
    out[8..12].copy_from_slice(&epoch.to_be_bytes());
    out[12..].copy_from_slice(sender.as_bytes());
    out
}

/// Sliding window of recently seen sequence numbers, per sender.
#[derive(Debug, Default)]
struct Replay {
    highest: Option<u32>,
    window: u64,
}

impl Replay {
    fn accept(&mut self, seq: u32) -> bool {
        let Some(high) = self.highest else {
            self.highest = Some(seq);
            self.window = 1;
            return true;
        };
        if seq > high {
            let shift = seq - high;
            self.window = if shift >= 64 { 1 } else { (self.window << shift) | 1 };
            self.highest = Some(seq);
            return true;
        }
        let back = high - seq;
        if back >= 64 || self.window & (1 << back) != 0 {
            return false;
        }
        self.window |= 1 << back;
        true
    }
}

/// The voice channel we're currently in, as configured by the engine.
pub struct RoomConfig {
    pub server: ServerId,
    pub channel: EventId,
    pub me: PeerId,
    pub epoch: u32,
    /// Every epoch key we hold for the server.
    pub keys: BTreeMap<u32, EpochKey>,
    /// Members currently in the channel with us, and their connections.
    pub peers: Vec<(PeerId, Connection)>,
    /// Peers watching our stream.
    pub watchers: Vec<PeerId>,
}

#[derive(Debug)]
struct Room {
    tag: [u8; 8],
    me: PeerId,
    epoch: u32,
    keys: BTreeMap<u32, [u8; 32]>,
    peers: HashMap<PeerId, Connection>,
    watchers: HashSet<PeerId>,
    inflight: HashMap<PeerId, Arc<AtomicUsize>>,
    seq: u32,
    replay: HashMap<PeerId, Replay>,
}

impl Room {
    fn seal(&mut self, kind: u8, payload: &[u8]) -> Option<Bytes> {
        let key = self.keys.get(&self.epoch)?;
        self.seq = self.seq.wrapping_add(1);
        let mut plaintext = Vec::with_capacity(5 + payload.len());
        plaintext.push(kind);
        plaintext.extend_from_slice(&self.seq.to_be_bytes());
        plaintext.extend_from_slice(payload);
        let nonce = crypto::random_nonce();
        let ct = crypto::seal(key, &nonce, &aad(&self.tag, self.epoch, &self.me), &plaintext);

        let mut packet = Vec::with_capacity(HEADER + ct.len());
        packet.push(VERSION);
        packet.extend_from_slice(&self.tag);
        packet.extend_from_slice(&self.epoch.to_be_bytes());
        packet.extend_from_slice(&nonce);
        packet.extend_from_slice(&ct);
        Some(Bytes::from(packet))
    }
}

/// Shared between the engine (which configures it) and media threads (which
/// send and receive through it). All methods are synchronous and cheap.
#[derive(Debug, Default)]
pub struct MediaHub {
    room: Mutex<Option<Room>>,
    sink: Mutex<Option<mpsc::Sender<MediaPacket>>>,
    /// The engine's runtime, so frames can be sent from any thread (e.g. a UI thread).
    runtime: Mutex<Option<tokio::runtime::Handle>>,
}

impl MediaHub {
    pub(crate) fn configure(&self, cfg: Option<RoomConfig>) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            self.runtime.lock().expect("media lock").get_or_insert(handle);
        }
        let mut room = self.room.lock().expect("media lock");
        let Some(cfg) = cfg else {
            *room = None;
            return;
        };
        let tag = room_tag(&cfg.server, &cfg.channel);
        let keys = cfg.keys.iter().map(|(e, k)| (*e, media_key(k, &cfg.server, &cfg.channel))).collect();
        let peers: HashMap<PeerId, Connection> = cfg.peers.into_iter().collect();
        let watchers: HashSet<PeerId> = cfg.watchers.into_iter().filter(|w| peers.contains_key(w)).collect();
        match room.as_mut() {
            // Same room: keep our sequence counter and replay state.
            Some(r) if r.tag == tag => {
                r.epoch = cfg.epoch;
                r.keys = keys;
                r.replay.retain(|p, _| peers.contains_key(p));
                r.inflight.retain(|p, _| watchers.contains(p));
                r.peers = peers;
                r.watchers = watchers;
            }
            _ => {
                *room = Some(Room {
                    tag,
                    me: cfg.me,
                    epoch: cfg.epoch,
                    keys,
                    peers,
                    watchers,
                    inflight: HashMap::new(),
                    seq: 0,
                    replay: HashMap::new(),
                })
            }
        }
    }

    pub(crate) fn set_sink(&self, sink: Option<mpsc::Sender<MediaPacket>>) {
        *self.sink.lock().expect("media lock") = sink;
    }

    /// Seals `payload` and sends it to everyone in the room. Returns how many peers it went to.
    pub fn send(&self, kind: u8, payload: &[u8]) -> usize {
        let mut guard = self.room.lock().expect("media lock");
        let Some(room) = guard.as_mut() else { return 0 };
        let Some(packet) = room.seal(kind, payload) else { return 0 };
        let mut sent = 0;
        for conn in room.peers.values() {
            if conn.send_datagram(packet.clone()).is_ok() {
                sent += 1;
            }
        }
        sent
    }

    /// Sends one encoded video frame to each watcher on its own QUIC stream.
    /// Viewers who are falling behind skip non-key frames until they catch up.
    /// Safe to call from any thread. Returns how many viewers it went to.
    pub fn send_frame(&self, payload: &[u8], keyframe: bool) -> usize {
        let Some(rt) = self.runtime.lock().expect("media lock").clone() else { return 0 };
        let mut guard = self.room.lock().expect("media lock");
        let Some(room) = guard.as_mut() else { return 0 };
        if room.watchers.is_empty() || payload.len() > MAX_VIDEO_FRAME {
            return 0;
        }
        let Some(packet) = room.seal(KIND_VIDEO, payload) else { return 0 };
        let mut sent = 0;
        for w in room.watchers.clone() {
            let Some(conn) = room.peers.get(&w).cloned() else { continue };
            let inflight = room.inflight.entry(w).or_default().clone();
            if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT && !keyframe {
                continue;
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let packet = packet.clone();
            rt.spawn(async move {
                if let Ok(mut s) = conn.open_uni().await
                    && s.write_all(&packet).await.is_ok()
                {
                    let _ = s.finish();
                }
                inflight.fetch_sub(1, Ordering::Relaxed);
            });
            sent += 1;
        }
        sent
    }

    /// Handles a datagram from `from`; anything not for our room, from a peer
    /// outside it, replayed, or failing authentication is dropped.
    pub(crate) fn receive(&self, from: PeerId, data: &[u8]) {
        let Some(packet) = self.open(from, data) else { return };
        if let Some(sink) = self.sink.lock().expect("media lock").as_ref() {
            let _ = sink.send(packet);
        }
    }

    fn open(&self, from: PeerId, data: &[u8]) -> Option<MediaPacket> {
        if data.len() < HEADER + 5 || data[0] != VERSION {
            return None;
        }
        let mut guard = self.room.lock().expect("media lock");
        let room = guard.as_mut()?;
        if data[1..9] != room.tag || !room.peers.contains_key(&from) {
            return None;
        }
        let epoch = u32::from_be_bytes(data[9..13].try_into().ok()?);
        // Allow the previous epoch briefly overlapping a key rotation.
        if epoch != room.epoch && epoch + 1 != room.epoch {
            return None;
        }
        let key = room.keys.get(&epoch)?;
        let nonce: [u8; 24] = data[13..37].try_into().ok()?;
        let plaintext = crypto::open(key, &nonce, &aad(&room.tag, epoch, &from), &data[HEADER..]).ok()?;
        if plaintext.len() < 5 {
            return None;
        }
        let seq = u32::from_be_bytes(plaintext[1..5].try_into().ok()?);
        if !room.replay.entry(from).or_default().accept(seq) {
            return None;
        }
        Some(MediaPacket { from, kind: plaintext[0], seq, payload: plaintext[5..].to_vec() })
    }
}

/// Your end of a voice channel: send encoded media, receive everyone else's.
pub struct VoiceSession {
    pub(crate) hub: std::sync::Arc<MediaHub>,
    pub incoming: mpsc::Receiver<MediaPacket>,
}

impl VoiceSession {
    pub fn send(&self, kind: u8, payload: &[u8]) -> usize {
        self.hub.send(kind, payload)
    }

    /// Separates sending (shareable across threads) from receiving.
    pub fn split(self) -> (MediaSender, mpsc::Receiver<MediaPacket>) {
        (MediaSender { hub: self.hub }, self.incoming)
    }
}

/// The sending half of a [`VoiceSession`].
#[derive(Clone)]
pub struct MediaSender {
    hub: std::sync::Arc<MediaHub>,
}

impl MediaSender {
    pub fn send(&self, kind: u8, payload: &[u8]) -> usize {
        self.hub.send(kind, payload)
    }

    /// See [`MediaHub::send_frame`].
    pub fn send_frame(&self, payload: &[u8], keyframe: bool) -> usize {
        self.hub.send_frame(payload, keyframe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_window() {
        let mut r = Replay::default();
        assert!(r.accept(10));
        assert!(!r.accept(10));
        assert!(r.accept(12));
        assert!(r.accept(11), "late but unseen is fine");
        assert!(!r.accept(11));
        assert!(r.accept(200));
        assert!(!r.accept(100), "too old");
    }

    #[test]
    fn tags_and_keys_differ_per_channel() {
        let s = ServerId([1; 32]);
        let me = iroh::SecretKey::generate().public();
        let (a, b) = (EventId { author: me, seq: 3 }, EventId { author: me, seq: 4 });
        assert_ne!(room_tag(&s, &a), room_tag(&s, &b));
        assert_ne!(media_key(&[7; 32], &s, &a), media_key(&[7; 32], &s, &b));
    }
}
