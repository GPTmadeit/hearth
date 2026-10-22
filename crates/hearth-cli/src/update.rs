//! Self-update from GitHub releases.
//!
//! Each release carries one binary per platform, named by target triple, plus
//! a minisign signature made in CI with the same key that signs the desktop
//! installers. Nothing is installed unless its signature verifies against the
//! public key compiled in here.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::Deserialize;

const REPO: &str = "GPTmadeit/hearth";

/// Base64 minisign public key; must match `plugins.updater.pubkey` in tauri.conf.json.
pub const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDYzNkRFQzlCQ0E0RDEwMDMKUldRREVFM0ttK3h0WXdSVUZVeExTb0l5Yi81c2l4Zm13Y3lHSXJjR2RlaTBLVkJ5bFJuQ0xXL0kK";

const MAX_BINARY: usize = 256 * 1024 * 1024;

/// The release asset for this build, if we publish one for this platform.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("hearth-x86_64-pc-windows-msvc.exe"),
        ("linux", "x86_64") => Some("hearth-x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub struct Available {
    pub version: String,
    binary_url: String,
    signature_url: String,
}

fn client() -> Result<reqwest::Client> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// Returns the latest release if it's newer than this binary and has a build for this platform.
pub async fn check() -> Result<Option<Available>> {
    let Some(asset) = asset_name() else { return Ok(None) };
    let release: Release = client()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !is_newer(&release.tag_name, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url_of = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let (Some(binary_url), Some(signature_url)) = (url_of(asset), url_of(&format!("{asset}.sig"))) else {
        return Ok(None);
    };
    Ok(Some(Available { version: release.tag_name.trim_start_matches('v').to_string(), binary_url, signature_url }))
}

/// Verifies `data` against a Tauri-style signature (base64 of a minisign signature file).
pub fn verify(data: &[u8], signature_b64: &str, public_key_b64: &str) -> Result<()> {
    let key_text = String::from_utf8(B64.decode(public_key_b64.trim())?)?;
    let key = minisign_verify::PublicKey::decode(&key_text).context("bad public key")?;
    let sig_text = String::from_utf8(B64.decode(signature_b64.trim())?)?;
    let sig = minisign_verify::Signature::decode(&sig_text).context("bad signature file")?;
    key.verify(data, &sig, true).context("signature does not match: refusing to install")?;
    Ok(())
}

/// Downloads, verifies, and swaps in the new binary. The running process keeps
/// the old code until it restarts.
pub async fn install(update: &Available) -> Result<()> {
    let http = client()?;
    let signature = http.get(&update.signature_url).send().await?.error_for_status()?.text().await?;
    let binary = http.get(&update.binary_url).send().await?.error_for_status()?.bytes().await?;
    if binary.len() > MAX_BINARY {
        bail!("download is unexpectedly large ({} bytes)", binary.len());
    }
    verify(&binary, &signature, PUBLIC_KEY)?;

    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("executable has no directory")?;
    let mut tmp = tempfile::Builder::new().prefix(".hearth-update").tempfile_in(dir)?;
    std::io::Write::write_all(&mut tmp, &binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))?;
    }
    self_replace::self_replace(tmp.path())?;
    Ok(())
}

/// Starts the freshly installed binary with the same arguments, replacing this process.
pub fn restart() -> Result<std::convert::Infallible> {
    let exe = std::env::current_exe()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Same PID, so service managers like systemd see a seamless restart.
        Err(std::process::Command::new(exe).args(args).exec().into())
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new(exe).args(args).spawn()?;
        std::process::exit(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn public_key_matches_desktop_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../hearth-desktop/tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(conf["plugins"]["updater"]["pubkey"].as_str(), Some(PUBLIC_KEY));
    }

    // Fixtures were signed with `cargo tauri signer sign`, exactly as CI signs releases.
    const FIXTURE: &[u8] = include_bytes!("../testdata/fixture.bin");

    #[test]
    fn accepts_release_signature() {
        verify(FIXTURE, include_str!("../testdata/fixture.bin.sig"), PUBLIC_KEY).unwrap();
    }

    #[test]
    fn rejects_tampered_binary() {
        let mut tampered = FIXTURE.to_vec();
        tampered[0] ^= 1;
        assert!(verify(&tampered, include_str!("../testdata/fixture.bin.sig"), PUBLIC_KEY).is_err());
    }

    #[test]
    fn rejects_signature_from_another_key() {
        assert!(verify(FIXTURE, include_str!("../testdata/fixture.bin.wrongkey.sig"), PUBLIC_KEY).is_err());
    }

    #[test]
    fn this_platform_has_an_asset() {
        if cfg!(all(target_arch = "x86_64", any(target_os = "windows", target_os = "linux"))) {
            assert!(asset_name().is_some());
        }
    }
}
