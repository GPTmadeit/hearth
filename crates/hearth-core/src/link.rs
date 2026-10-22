//! Shareable links, as text or QR codes.
//!
//! - `hearth://friend/<peer>`: add this person as a friend.
//! - `hearth://join/<server>/<via>/<code>`: join a server. `via` is the owner
//!   who made the code; they let in whoever presents it until it expires.
//!
//! A bare peer ID is accepted as a friend link, so IDs pasted from older
//! versions still work.

use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};

use crate::{PeerId, ServerId};

pub type InviteCode = [u8; 16];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Friend(PeerId),
    Join { server: ServerId, via: PeerId, code: InviteCode },
}

const SCHEME: &str = "hearth://";

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Link::Friend(peer) => write!(f, "{SCHEME}friend/{peer}"),
            Link::Join { server, via, code } => {
                write!(f, "{SCHEME}join/{server}/{via}/")?;
                code.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }
    }
}

impl FromStr for Link {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        let Some(rest) = s.strip_prefix(SCHEME) else {
            return s.parse().map(Link::Friend).context("not a Hearth link or peer ID");
        };
        let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
        match parts.as_slice() {
            ["friend", peer] => Ok(Link::Friend(peer.parse().context("bad peer ID in link")?)),
            ["join", server, via, code] => {
                let bytes = (0..code.len())
                    .step_by(2)
                    .map(|i| code.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
                    .collect::<Option<Vec<u8>>>()
                    .context("bad invite code")?;
                let code: InviteCode = bytes.try_into().ok().context("bad invite code")?;
                Ok(Link::Join { server: server.parse()?, via: via.parse().context("bad peer ID in link")?, code })
            }
            _ => bail!("unrecognised Hearth link"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_roundtrip() {
        let peer = crate::SecretKey::generate().public();
        let join = Link::Join { server: ServerId([7; 32]), via: peer, code: [9; 16] };
        assert_eq!(join.to_string().parse::<Link>().unwrap(), join);
        let friend = Link::Friend(peer);
        assert_eq!(friend.to_string().parse::<Link>().unwrap(), friend);
        assert_eq!(peer.to_string().parse::<Link>().unwrap(), friend, "bare IDs are friend links");
        assert!("hearth://join/zz".parse::<Link>().is_err());
        assert!(format!("hearth://join/{}/{peer}/abc", ServerId([7; 32])).parse::<Link>().is_err());
    }
}
