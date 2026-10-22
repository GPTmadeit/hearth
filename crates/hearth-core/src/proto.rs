//! Wire format: each frame is a u32 big-endian length followed by a postcard-encoded [`Frame`].

use anyhow::{Result, bail};
use iroh::{
    EndpointId as PeerId,
    endpoint::{ReadExactError, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};

use crate::{
    crypto::EpochKey,
    event::{Event, ServerId},
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
    /// A direct message, delivered only while both peers are online.
    Dm { body: String, sent_at_ms: u64 },
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
pub async fn read(recv: &mut RecvStream) -> Result<Option<Frame>> {
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
    Ok(Some(postcard::from_bytes(&buf)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let f = Frame::Dm { body: "hi".into(), sent_at_ms: 42 };
        let bytes = postcard::to_stdvec(&f).unwrap();
        assert_eq!(postcard::from_bytes::<Frame>(&bytes).unwrap(), f);
    }
}
