//! Update check for Android, where the Tauri updater isn't available: find a
//! newer release on GitHub with an APK. Installing is left to the system,
//! which only accepts it if it's signed with the same key as this app.

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;

const REPO: &str = "GPTmadeit/hearth";

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
    pub apk_url: String,
}

fn version(v: &str) -> Option<(u64, u64, u64)> {
    let mut p = v.trim_start_matches('v').split(['.', '-', '+']).map(|x| x.parse::<u64>().ok());
    Some((p.next()??, p.next()??, p.next()??))
}

pub async fn check() -> Result<Option<Available>> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("hearth-android/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let release: Release = client
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let newer = matches!((version(&release.tag_name), version(env!("CARGO_PKG_VERSION"))), (Some(a), Some(b)) if a > b);
    if !newer {
        return Ok(None);
    }
    let apk = release.assets.into_iter().find(|a| a.name.ends_with(".apk"));
    Ok(apk.map(|a| Available { version: release.tag_name.trim_start_matches('v').to_string(), apk_url: a.browser_download_url }))
}
