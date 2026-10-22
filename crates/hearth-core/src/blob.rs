//! Encrypted, content-addressed blobs: attachments and profile pictures.
//!
//! Each file is sealed under its own random key in 1 MiB chunks. Every chunk's
//! AEAD covers its index and whether it is the last one, so chunks can't be
//! reordered, dropped, or the file truncated. A blob is named by the BLAKE3
//! hash of its *ciphertext*, so anyone (including blind storage nodes) can
//! check they hold the right bytes without being able to read them. The key
//! travels inside the (sealed) message that references the blob.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use iroh::endpoint::Connection;
use serde::{Deserialize, Serialize};

pub type BlobHash = [u8; 32];

/// Largest file we'll accept, send, or store.
pub const MAX_BLOB: u64 = 100 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;
const TAG: usize = 16;
const REQUEST_MAGIC: &[u8; 2] = b"HB";

/// Everything needed to fetch and decrypt one blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub hash: BlobHash,
    pub key: [u8; 32],
    /// Ciphertext size in bytes.
    pub size: u64,
}

fn nonce(index: u64) -> XNonce {
    let mut n = [0u8; 24];
    n[..8].copy_from_slice(&index.to_be_bytes());
    XNonce::from(n)
}

fn aad(index: u64, last: bool) -> [u8; 9] {
    let mut a = [0u8; 9];
    a[..8].copy_from_slice(&index.to_be_bytes());
    a[8] = last as u8;
    a
}

/// Encrypts `plain` under a fresh key. Nonces can be simple counters because
/// every key is used for exactly one file.
pub fn seal(plain: &[u8]) -> (BlobRef, Vec<u8>) {
    let key: [u8; 32] = rand::random();
    let cipher = XChaCha20Poly1305::new(&Key::from(key));
    let chunks: Vec<&[u8]> = if plain.is_empty() { vec![&[][..]] } else { plain.chunks(CHUNK).collect() };
    let mut out = Vec::with_capacity(plain.len() + chunks.len() * TAG);
    for (i, chunk) in chunks.iter().enumerate() {
        let last = i + 1 == chunks.len();
        let ct = cipher
            .encrypt(&nonce(i as u64), Payload { msg: chunk, aad: &aad(i as u64, last) })
            .expect("in-memory AEAD encryption cannot fail");
        out.extend_from_slice(&ct);
    }
    let r = BlobRef { hash: *blake3::hash(&out).as_bytes(), key, size: out.len() as u64 };
    (r, out)
}

/// Verifies and decrypts a blob.
pub fn open(r: &BlobRef, ct: &[u8]) -> Result<Vec<u8>> {
    if blake3::hash(ct).as_bytes() != &r.hash {
        bail!("blob does not match its hash");
    }
    let cipher = XChaCha20Poly1305::new(&Key::from(r.key));
    let pieces: Vec<&[u8]> = ct.chunks(CHUNK + TAG).collect();
    let mut out = Vec::with_capacity(ct.len());
    for (i, piece) in pieces.iter().enumerate() {
        let last = i + 1 == pieces.len();
        let plain = cipher
            .decrypt(&nonce(i as u64), Payload { msg: piece, aad: &aad(i as u64, last) })
            .map_err(|_| anyhow::anyhow!("blob chunk {i} failed to decrypt"))?;
        out.extend_from_slice(&plain);
    }
    Ok(out)
}

pub fn hex(hash: &BlobHash) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ciphertext blobs on disk, one file per hash.
#[derive(Debug)]
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.to_path_buf() })
    }

    fn path(&self, hash: &BlobHash) -> PathBuf {
        self.dir.join(hex(hash))
    }

    pub fn has(&self, hash: &BlobHash) -> bool {
        self.path(hash).exists()
    }

    pub fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(hash)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Stores `ct` if it hashes to `hash`. Writes atomically so a crash never leaves a bad blob.
    pub fn put(&self, hash: &BlobHash, ct: &[u8]) -> Result<()> {
        if blake3::hash(ct).as_bytes() != hash {
            bail!("refusing to store blob that doesn't match its hash");
        }
        if self.has(hash) {
            return Ok(());
        }
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
        tmp.write_all(ct)?;
        tmp.persist(self.path(hash)).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Serves one blob request on a stream the peer opened.
pub async fn serve(store: &BlobStore, mut send: iroh::endpoint::SendStream, mut recv: iroh::endpoint::RecvStream) -> Result<()> {
    let mut req = [0u8; 34];
    recv.read_exact(&mut req).await?;
    if &req[..2] != REQUEST_MAGIC {
        bail!("not a blob request");
    }
    let hash: BlobHash = req[2..].try_into().expect("32 bytes");
    match store.get(&hash)? {
        Some(ct) => {
            send.write_all(&[1]).await?;
            send.write_all(&(ct.len() as u64).to_be_bytes()).await?;
            send.write_all(&ct).await?;
        }
        None => send.write_all(&[0]).await?,
    }
    send.finish()?;
    Ok(())
}

/// Asks `conn`'s peer for a blob. Returns `None` if they don't have it.
pub async fn request(conn: &Connection, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let mut req = Vec::with_capacity(34);
    req.extend_from_slice(REQUEST_MAGIC);
    req.extend_from_slice(hash);
    send.write_all(&req).await?;
    send.finish()?;
    let mut status = [0u8; 1];
    recv.read_exact(&mut status).await?;
    if status[0] != 1 {
        return Ok(None);
    }
    let mut len = [0u8; 8];
    recv.read_exact(&mut len).await?;
    let len = u64::from_be_bytes(len);
    if len > MAX_BLOB + MAX_BLOB / CHUNK as u64 * TAG as u64 + TAG as u64 {
        bail!("peer offered an oversized blob");
    }
    let mut ct = vec![0u8; len as usize];
    recv.read_exact(&mut ct).await.context("blob transfer interrupted")?;
    if blake3::hash(&ct).as_bytes() != hash {
        bail!("peer sent a blob that doesn't match its hash");
    }
    Ok(Some(ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sizes() {
        for size in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let plain: Vec<u8> = (0..size).map(|i| (i * 7) as u8).collect();
            let (r, ct) = seal(&plain);
            assert_eq!(r.size as usize, ct.len());
            assert_eq!(open(&r, &ct).unwrap(), plain, "size {size}");
        }
    }

    #[test]
    fn rejects_tampering_and_truncation() {
        let plain = vec![9u8; 2 * CHUNK + 5];
        let (r, ct) = seal(&plain);
        let mut flipped = ct.clone();
        flipped[10] ^= 1;
        assert!(open(&r, &flipped).is_err());

        // Drop the last chunk and fix up the hash: the "last chunk" flag still catches it.
        let truncated = ct[..2 * (CHUNK + TAG)].to_vec();
        let forged = BlobRef { hash: *blake3::hash(&truncated).as_bytes(), ..r };
        assert!(open(&forged, &truncated).is_err());

        let wrong_key = BlobRef { key: [0; 32], ..r };
        assert!(open(&wrong_key, &ct).is_err());
    }

    #[test]
    fn store_checks_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let (r, ct) = seal(b"hello");
        assert!(store.put(&[0; 32], &ct).is_err());
        store.put(&r.hash, &ct).unwrap();
        assert!(store.has(&r.hash));
        assert_eq!(open(&r, &store.get(&r.hash).unwrap().unwrap()).unwrap(), b"hello");
    }
}
