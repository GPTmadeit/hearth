//! Wire format: each frame is a u32 big-endian length followed by a postcard-encoded [`Message`].

use anyhow::{Result, bail};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

/// ALPN for the direct-message protocol. Bump the suffix on breaking changes.
pub const CHAT_ALPN: &[u8] = b"hearth/chat/0";

const MAX_FRAME: u32 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// First frame on every stream: the sender's chosen display name.
    Hello { name: String },
    Text { body: String, sent_at_ms: u64 },
}

pub async fn write(send: &mut SendStream, msg: &Message) -> Result<()> {
    let bytes = postcard::to_stdvec(msg)?;
    if bytes.len() > MAX_FRAME as usize {
        bail!("frame of {} bytes exceeds limit", bytes.len());
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

/// Reads the next frame, or `None` if the peer finished the stream cleanly.
pub async fn read(recv: &mut RecvStream) -> Result<Option<Message>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
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
        let m = Message::Text { body: "hi".into(), sent_at_ms: 42 };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(postcard::from_bytes::<Message>(&bytes).unwrap(), m);
    }
}
