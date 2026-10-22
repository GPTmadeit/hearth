use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use iroh::SecretKey;

/// Loads the identity key at `path`, creating a new one if it doesn't exist.
pub fn load_or_create(path: &Path) -> Result<SecretKey> {
    if path.exists() {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
            bail!("{} is not a 32-byte identity key", path.display());
        };
        return Ok(SecretKey::from_bytes(&bytes));
    }

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let key = SecretKey::generate();
    write_private(path, &key.to_bytes())?;
    Ok(key)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

// On Windows the file inherits the user-profile ACL, which is already per-user.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!("hearth-test-{}", std::process::id()));
        let path = dir.join("identity.key");
        let a = load_or_create(&path).unwrap();
        let b = load_or_create(&path).unwrap();
        assert_eq!(a.public(), b.public());
        fs::remove_dir_all(dir).unwrap();
    }
}
