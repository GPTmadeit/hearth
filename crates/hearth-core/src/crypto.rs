//! Symmetric sealing for server content. Each server epoch has one 32-byte key;
//! a new epoch starts whenever the owner removes a member.

use anyhow::{Result, anyhow};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};

pub type EpochKey = [u8; 32];

pub fn random_key() -> EpochKey {
    rand::random()
}

pub fn random_nonce() -> [u8; 24] {
    rand::random()
}

pub fn seal(key: &EpochKey, nonce: &[u8; 24], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    XChaCha20Poly1305::new(&Key::from(*key))
        .encrypt(&XNonce::from(*nonce), Payload { msg: plaintext, aad })
        .expect("XChaCha20-Poly1305 encryption is infallible for in-memory buffers")
}

pub fn open(key: &EpochKey, nonce: &[u8; 24], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(&Key::from(*key))
        .decrypt(&XNonce::from(*nonce), Payload { msg: ciphertext, aad })
        .map_err(|_| anyhow!("decryption failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_tamper() {
        let (k, n) = (random_key(), random_nonce());
        let ct = seal(&k, &n, b"aad", b"secret");
        assert_eq!(open(&k, &n, b"aad", &ct).unwrap(), b"secret");
        assert!(open(&k, &n, b"other", &ct).is_err());
        assert!(open(&random_key(), &n, b"aad", &ct).is_err());
    }
}
