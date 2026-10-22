//! Wire format: each frame is a u32 big-endian length followed by a postcard-encoded [`Frame`].

use anyhow::{Result, bail};
use iroh::{
    EndpointId as PeerId,
    endpoint::{ReadExactError, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};

use crate::{
    blob::BlobRef,
    crypto::EpochKey,
    event::{Event, EventId, ServerId},
};

/// ALPN for the peer protocol. Bump the suffix on breaking changes.
pub const ALPN: &[u8] = b"hearth/peer/0";

const MAX_FRAME: u32 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// First frame on every connection: the sender's chosen display name.
    Hello { name: String },
    /// What the sender holds for a server: highest seq per author, and which epoch keys.
    Have { server: ServerId, have: Vec<(PeerId, u64)>, epochs: Vec<u32> },
    Events(Vec<Event>),
    /// An epoch key, sent only to members. Receivers check it against the owner's commitment.
    Key { server: ServerId, epoch: u32, key: EpochKey },
    /// The sender has added the receiver to a server.
    Invite { server: ServerId, name: String },
    /// The owner removed the receiver from a server. Trusted only from the owner.
    Removed { server: ServerId },
    /// Which voice channel the sender is in (`None` = left voice), and whether
    /// they're streaming their screen. Live-only state.
    Voice { server: ServerId, channel: Option<EventId>, live: bool },
    /// The sender wants (or no longer wants) the receiver's screen stream.
    Watch { server: ServerId, on: bool },
    /// A direct message, delivered only while both peers are online.
    Dm { body: String, sent_at_ms: u64 },
    // ---- 0.2: append new variants only, so older peers can skip what they don't know ----
    /// The sender started or stopped talking in their voice channel of `server`.
    Speaking { server: ServerId, speaking: bool },
    /// The sender is typing in a channel.
    Typing { server: ServerId, channel: EventId },
    FriendRequest { name: String },
    /// Accepts a friend request. `dm` is the conversation the sender created for
    /// the two of you; whichever of you has the smaller ID creates it.
    FriendAccept { dm: Option<ServerId> },
    FriendRemove,
    /// "I have this blob for `server`": storage hosts fetch it for offline delivery.
    BlobOffer { server: ServerId, hash: [u8; 32], size: u64 },
    /// The sender's display name and profile picture.
    Profile { name: String, avatar: Option<BlobRef> },
    /// Full voice state (0.3+). Sent after the legacy `Voice` frame, which older peers still read.
    VoiceState { server: ServerId, channel: Option<EventId>, live: bool, camera: bool, muted: bool, deafened: bool },
}

pub async fn write(send: &mut SendStream, frame: &Frame) -> Result<()> {
    let bytes = postcard::to_stdvec(frame)?;
    if bytes.len() > MAX_FRAME as usize {
        bail!("frame of {} bytes exceeds limit", bytes.len());
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

/// Reads the next frame, or `None` if the peer finished the stream cleanly.
/// Frames we can't decode (from a newer version) are skipped, not fatal.
pub async fn read(recv: &mut RecvStream) -> Result<Option<Frame>> {
    loop {
        let mut len = [0u8; 4];
        match recv.read_exact(&mut len).await {
            Ok(()) => {}
            Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_be_bytes(len);
        if len > MAX_FRAME {
            bail!("peer sent oversized frame ({len} bytes)");
        }
        let mut buf = vec![0u8; len as usize];
        recv.read_exact(&mut buf).await?;
        if let Ok(frame) = postcard::from_bytes(&buf) {
            return Ok(Some(frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_variants_keep_old_encodings() {
        // Variant tags 0.1 peers use. Reordering would make old and new peers misread each other.
        let tag = |f: &Frame| postcard::to_stdvec(f).unwrap()[0];
        assert_eq!(tag(&Frame::Hello { name: String::new() }), 0);
        assert_eq!(tag(&Frame::Events(vec![])), 2);
        assert_eq!(tag(&Frame::Removed { server: ServerId([0; 32]) }), 5);
        assert_eq!(tag(&Frame::Dm { body: "hi".into(), sent_at_ms: 1 }), 8);
        let old = Frame::Dm { body: "hi".into(), sent_at_ms: 1 };
        assert_eq!(postcard::from_bytes::<Frame>(&postcard::to_stdvec(&old).unwrap()).unwrap(), old);
    }

    #[test]
    fn roundtrip() {
        let f = Frame::Dm { body: "hi".into(), sent_at_ms: 42 };
        let bytes = postcard::to_stdvec(&f).unwrap();
        assert_eq!(postcard::from_bytes::<Frame>(&bytes).unwrap(), f);
    }
}
