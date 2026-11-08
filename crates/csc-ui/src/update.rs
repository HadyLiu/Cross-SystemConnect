//! Update check against GitHub Releases. Reads the latest release only; never
//! downloads or installs anything itself.

use eframe::egui;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

const LATEST_API: &str = "https://api.github.com/repos/HadyLiu/Cross-SystemConnect/releases/latest";
/// Links from the reply are only shown when they stay inside this repository.
const RELEASES_PREFIX: &str = "https://github.com/HadyLiu/Cross-SystemConnect/releases/";
const MAX_BODY: u64 = 1 << 20;
const MAX_NOTES: usize = 2000;

#[cfg(windows)]
const ASSET_SUFFIX: &str = "windows-x86_64.zip";
#[cfg(not(windows))]
const ASSET_SUFFIX: &str = "linux-x86_64.tar.gz";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub download: Option<String>,
    pub notes: String,
    pub published: String,
}

#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate(String),
    Available(Release),
    Failed(String),
}

/// Shared with the background check thread.
#[derive(Clone, Default)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
}

impl Updater {
    pub fn state(&self) -> UpdateState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Start a check unless one is already running.
    pub fn check(&self, ctx: &egui::Context) {
        {
            let Ok(mut s) = self.state.lock() else { return };
            if matches!(*s, UpdateState::Checking) {
                return;
            }
            *s = UpdateState::Checking;
        }
        let state = self.state.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let next = match latest() {
                    Ok(release) if is_newer(&release.version, env!("CARGO_PKG_VERSION")) => {
                        UpdateState::Available(release)
                    }
                    Ok(release) => UpdateState::UpToDate(release.version),
                    Err(e) => {
                        tracing::warn!("update check: {e:#}");
                        UpdateState::Failed(format!("{e:#}"))
                    }
                };
                if let Ok(mut s) = state.lock() {
                    *s = next;
                }
                ctx.request_repaint();
            });
        if spawned.is_err() {
            if let Ok(mut s) = self.state.lock() {
                *s = UpdateState::Failed("could not start the check".into());
            }
        }
    }
}

fn agent() -> ureq::Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into()
}

fn latest() -> anyhow::Result<Release> {
    let mut reply = agent()
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .header(
            "User-Agent",
            &format!("Cross-SystemConnect/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()?;
    let body = reply
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()?;
    parse_release(&serde_json::from_str(&body)?)
}

fn parse_release(v: &Value) -> anyhow::Result<Release> {
    let text = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("");
    let tag = text("tag_name");
    let version = tag.trim_start_matches('v').to_string();
    if parse_version(&version).is_none() {
        anyhow::bail!("unexpected release tag {tag:?}");
    }
    let page = text("html_url");
    if !page.starts_with(RELEASES_PREFIX) {
        anyhow::bail!("release page outside the project: {page:?}");
    }
    let download = v
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| {
            a.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.ends_with(ASSET_SUFFIX))
        })
        .filter_map(|a| a.get("browser_download_url").and_then(Value::as_str))
        .find(|url| url.starts_with(RELEASES_PREFIX))
        .map(str::to_string);
    let notes: String = text("body")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_NOTES)
        .collect();
    let published = text("published_at").chars().take(10).collect();
    Ok(Release {
        version,
        page: page.to_string(),
        download,
        notes,
        published,
    })
}

/// `MAJOR.MINOR.PATCH`, ignoring a `-pre` or `+build` suffix.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "needs the network: cargo test -p csc-ui -- --ignored"]
    fn live_latest_release() {
        let release = latest().unwrap();
        assert!(parse_version(&release.version).is_some());
        assert!(release.download.is_some());
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.4", "0.2.4"));
        assert!(!is_newer("0.2.3", "0.2.4"));
        assert!(!is_newer("garbage", "0.2.4"));
        assert_eq!(parse_version("0.3.0-rc1"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3"), None);
    }

    #[test]
    fn release_reply_keeps_only_project_links() {
        let reply = json!({
            "tag_name": "v0.2.5",
            "html_url": "https://github.com/HadyLiu/Cross-SystemConnect/releases/tag/v0.2.5",
            "published_at": "2026-11-10T01:02:03Z",
            "body": "fixes\u{0007}\nmore",
            "assets": [
                {"name": format!("Cross-SystemConnect-{ASSET_SUFFIX}"),
                 "browser_download_url": "https://evil.example/x"},
                {"name": format!("Cross-SystemConnect-{ASSET_SUFFIX}"),
                 "browser_download_url":
                    "https://github.com/HadyLiu/Cross-SystemConnect/releases/download/v0.2.5/a"},
            ],
        });
        let release = parse_release(&reply).unwrap();
        assert_eq!(release.version, "0.2.5");
        assert_eq!(release.published, "2026-11-10");
        assert_eq!(release.notes, "fixes\nmore");
        assert_eq!(
            release.download.as_deref(),
            Some("https://github.com/HadyLiu/Cross-SystemConnect/releases/download/v0.2.5/a")
        );

        let foreign = json!({"tag_name": "v9.9.9", "html_url": "https://evil.example/"});
        assert!(parse_release(&foreign).is_err());
    }
}
