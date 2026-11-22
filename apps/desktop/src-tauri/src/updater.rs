//! GitHub Releases update checker, changelog fetcher, and SHA-256 verified macOS `.app` updater.

use crate::app::open_url;
use crate::proxy::auth::random_token;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_REPO: &str = "07rjain/t3agent37";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available,
    UpToDate,
    Downloading,
    Installing,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
    pub changelog: Option<String>,
    pub published_at: Option<String>,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub checksums_url: Option<String>,
    pub can_self_update: bool,
    pub status: UpdateStatus,
    pub error: Option<String>,
    pub checked_at: Option<i64>,
}

impl Default for UpdateInfo {
    fn default() -> Self {
        Self {
            current_version: CURRENT_VERSION.to_owned(),
            latest_version: None,
            update_available: false,
            release_url: Some(releases_page_url()),
            changelog: None,
            published_at: None,
            asset_name: None,
            asset_url: None,
            checksums_url: None,
            can_self_update: false,
            status: UpdateStatus::Idle,
            error: None,
            checked_at: None,
        }
    }
}

#[derive(Debug)]
pub enum InstallOutcome {
    /// Detached swap helper is waiting for this PID to exit so it can replace the `.app` and relaunch.
    RestartRequired,
    /// Installed `.app` into Applications (when launched from a dev binary) and opened it.
    OpenedExternal(PathBuf),
    /// No direct archive matched this platform; opened the GitHub Release page instead.
    OpenedReleasePage(String),
}

pub fn repo() -> String {
    std::env::var("T3AGENT37_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_owned())
}

pub fn releases_page_url() -> String {
    format!("https://github.com/{}/releases", repo())
}

fn latest_release_api_url() -> String {
    if let Ok(url) = std::env::var("T3AGENT37_UPDATE_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    format!("https://api.github.com/repos/{}/releases/latest", repo())
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()?)
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub async fn check_latest() -> Result<UpdateInfo> {
    let client = http_client()?;
    let url = latest_release_api_url();
    check_latest_from(&client, &url, CURRENT_VERSION, std::env::consts::OS, std::env::consts::ARCH).await
}

async fn check_latest_from(
    client: &reqwest::Client,
    url: &str,
    current_version: &str,
    os: &str,
    arch: &str,
) -> Result<UpdateInfo> {
    let now = chrono::Utc::now().timestamp();
    let res = client
        .get(url)
        .header(USER_AGENT, format!("t3agent37/{current_version}"))
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("checking GitHub Releases")?;

    if res.status() == StatusCode::NOT_FOUND {
        return Ok(UpdateInfo {
            current_version: current_version.to_owned(),
            latest_version: Some(current_version.to_owned()),
            update_available: false,
            release_url: Some(releases_page_url()),
            status: UpdateStatus::UpToDate,
            checked_at: Some(now),
            ..UpdateInfo::default()
        });
    }
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        bail!("GitHub update check failed ({status}): {}", text.trim());
    }

    let release: GithubRelease = res.json().await.context("parsing GitHub release JSON")?;
    let latest_version = normalize_version(&release.tag_name).to_owned();
    let update_available = is_newer(&latest_version, current_version);
    let expected_asset = expected_asset_name(os, arch);
    let matched_asset = expected_asset.and_then(|name| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .or_else(|| release.assets.iter().find(|a| a.name.ends_with(".app.tar.gz") && a.name.contains(arch)))
    });
    let checksums_url = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .map(|a| a.browser_download_url.clone());

    let changelog = release
        .body
        .map(|b| b.trim().to_owned())
        .filter(|b| !b.is_empty());

    let can_self_update = os == "macos" && matched_asset.is_some();

    Ok(UpdateInfo {
        current_version: current_version.to_owned(),
        latest_version: Some(latest_version),
        update_available,
        release_url: Some(release.html_url),
        changelog,
        published_at: release.published_at,
        asset_name: matched_asset.map(|a| a.name.clone()),
        asset_url: matched_asset.map(|a| a.browser_download_url.clone()),
        checksums_url,
        can_self_update,
        status: if update_available { UpdateStatus::Available } else { UpdateStatus::UpToDate },
        error: None,
        checked_at: Some(now),
    })
}

fn expected_asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("t3agent37-macos-aarch64.app.tar.gz"),
        ("macos", "x86_64") => Some("t3agent37-macos-x86_64.app.tar.gz"),
        _ => None,
    }
}

pub fn normalize_version(v: &str) -> &str {
    v.trim().trim_start_matches(['v', 'V'])
}

#[derive(Debug, PartialEq, Eq)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

fn parse_semver(raw: &str) -> Option<SemVer> {
    let stripped = normalize_version(raw).split('+').next()?.trim();
    let (core, pre) = match stripped.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p.to_owned())),
        Some((c, _)) => (c, None),
        None => (stripped, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SemVer { major, minor, patch, pre })
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(l), Some(c)) = (parse_semver(latest), parse_semver(current)) else {
        return false;
    };
    match (l.major, l.minor, l.patch).cmp(&(c.major, c.minor, c.patch)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (&l.pre, &c.pre) {
            (None, Some(_)) => true,
            (Some(_), None) | (None, None) => false,
            (Some(lp), Some(cp)) => lp > cp,
        },
    }
}

fn verify_sha256(bytes: &[u8], checksums_text: &str, expected_filename: &str) -> Result<()> {
    let expected_hex = checksums_text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let digest = parts.next()?;
            let file = parts.next()?.trim_start_matches('*');
            let base = Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file);
            if base == expected_filename {
                Some(digest.to_ascii_lowercase())
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow!("SHA256SUMS.txt does not list {expected_filename}"))?;

    let actual_hex = format!("{:x}", Sha256::digest(bytes));
    if actual_hex != expected_hex {
        bail!("SHA-256 mismatch for {expected_filename} (expected {expected_hex}, got {actual_hex})");
    }
    Ok(())
}

/// Downloads, verifies, and installs the release described by `info`.
pub async fn download_and_install(info: &UpdateInfo, on_stage: impl Fn(UpdateStatus)) -> Result<InstallOutcome> {
    let release_url = info.release_url.clone().unwrap_or_else(releases_page_url);
    if !cfg!(target_os = "macos") {
        open_url(&release_url)?;
        return Ok(InstallOutcome::OpenedReleasePage(release_url));
    }
    let (Some(asset_url), Some(asset_name)) = (info.asset_url.as_deref(), info.asset_name.as_deref()) else {
        open_url(&release_url)?;
        return Ok(InstallOutcome::OpenedReleasePage(release_url));
    };

    on_stage(UpdateStatus::Downloading);
    let client = http_client()?;
    let bytes = download_bytes(&client, asset_url)
        .await
        .with_context(|| format!("downloading {asset_name}"))?;

    if let Some(checksums_url) = info.checksums_url.as_deref() {
        let sum_bytes = download_bytes(&client, checksums_url)
            .await
            .context("downloading SHA256SUMS.txt")?;
        let sum_text = String::from_utf8(sum_bytes).context("SHA256SUMS.txt is not valid UTF-8")?;
        verify_sha256(&bytes, &sum_text, asset_name)?;
    }

    on_stage(UpdateStatus::Installing);
    install_macos_archive(&bytes)
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let res = client
        .get(url)
        .header(USER_AGENT, format!("t3agent37/{CURRENT_VERSION}"))
        .timeout(Duration::from_secs(300))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("HTTP {} downloading {url}", res.status());
    }
    Ok(res.bytes().await?.to_vec())
}

fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    for ancestor in exe.ancestors() {
        if ancestor.extension().and_then(|e| e.to_str()) == Some("app") {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn find_extracted_app(dir: &Path) -> Result<PathBuf> {
    let preferred = dir.join("t3agent37.app");
    if preferred.is_dir() {
        return Ok(preferred);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() && path.extension().and_then(|e| e.to_str()) == Some("app") {
            return Ok(path);
        }
    }
    bail!("downloaded update archive did not contain a .app bundle")
}

fn install_macos_archive(archive_bytes: &[u8]) -> Result<InstallOutcome> {
    let stage = std::env::temp_dir().join(format!("t3agent37-update-{}", random_token()));
    let extract_dir = stage.join("extract");
    std::fs::create_dir_all(&extract_dir).context("creating update staging directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700));
    }

    let archive_path = stage.join("update.app.tar.gz");
    std::fs::write(&archive_path, archive_bytes).context("writing downloaded update archive")?;

    let status = std::process::Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .context("running tar to extract update")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&stage);
        bail!("failed to extract downloaded update archive");
    }

    let new_app = match find_extracted_app(&extract_dir) {
        Ok(app) => app,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };

    // Strip macOS quarantine attribute so the self-updated unsigned app launches cleanly.
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&new_app)
        .status();

    if let Some(target_app) = current_app_bundle() {
        let helper = stage.join("swap.sh");
        let script = r#"#!/bin/sh
set -u
pid="$1"
target="$2"
src="$3"
stage="$4"
i=0
while [ "$i" -lt 100 ] && kill -0 "$pid" 2>/dev/null; do
  sleep 0.1
  i=$((i + 1))
done
backup="${target}.bak.$$"
rm -rf "$backup"
if [ -e "$target" ]; then
  if ! mv "$target" "$backup"; then
    /usr/bin/open -n "$target" >/dev/null 2>&1 || true
    rm -rf "$stage"
    exit 1
  fi
fi
if mv "$src" "$target"; then
  rm -rf "$backup"
  /usr/bin/xattr -dr com.apple.quarantine "$target" >/dev/null 2>&1 || true
  /usr/bin/open -n "$target" >/dev/null 2>&1 || true
else
  [ -e "$backup" ] && mv "$backup" "$target"
  /usr/bin/open -n "$target" >/dev/null 2>&1 || true
fi
rm -rf "$stage"
"#;
        std::fs::write(&helper, script).context("writing update swap script")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700));
        }
        std::process::Command::new("/bin/sh")
            .arg(&helper)
            .arg(std::process::id().to_string())
            .arg(&target_app)
            .arg(&new_app)
            .arg(&stage)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawning macOS update helper")?;
        return Ok(InstallOutcome::RestartRequired);
    }

    // Running from a dev binary outside a `.app` bundle: install into ~/Applications or /Applications.
    let apps_dir = dirs::home_dir()
        .map(|h| h.join("Applications"))
        .unwrap_or_else(|| PathBuf::from("/Applications"));
    std::fs::create_dir_all(&apps_dir)?;
    let dest = apps_dir.join("t3agent37.app");
    if dest.exists() {
        std::fs::remove_dir_all(&dest).with_context(|| format!("replacing {}", dest.display()))?;
    }
    let mv_status = std::process::Command::new("/bin/mv")
        .arg(&new_app)
        .arg(&dest)
        .status()
        .context("moving updated app into Applications")?;
    let _ = std::fs::remove_dir_all(&stage);
    if !mv_status.success() {
        bail!("could not move updated app into {}", dest.display());
    }
    let _ = std::process::Command::new("/usr/bin/open").arg("-n").arg(&dest).status();
    Ok(InstallOutcome::OpenedExternal(dest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    #[test]
    fn semver_comparison_handles_tags_and_prereleases() {
        assert!(is_newer("v0.2.1", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(is_newer("0.2.0-rc.2", "0.2.0-rc.1"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn sha256_verification_matches_and_rejects_mismatch() {
        let data = b"hello t3agent37 release";
        let hex = format!("{:x}", Sha256::digest(data));
        let sums = format!(
            "0000000000000000000000000000000000000000000000000000000000000000  other.dmg\n\
             {hex}  t3agent37-macos-aarch64.app.tar.gz\n"
        );
        verify_sha256(data, &sums, "t3agent37-macos-aarch64.app.tar.gz").unwrap();
        assert!(verify_sha256(b"tampered", &sums, "t3agent37-macos-aarch64.app.tar.gz").is_err());
        assert!(verify_sha256(data, &sums, "missing.tar.gz").is_err());
    }

    #[tokio::test]
    async fn checks_github_release_matches_arch_and_handles_404() {
        let app = Router::new()
            .route(
                "/latest",
                get(|| async {
                    Json(json!({
                        "tag_name": "v0.3.0",
                        "html_url": "https://github.com/07rjain/t3agent37/releases/tag/v0.3.0",
                        "body": "### Added\n- Auto-update from GitHub",
                        "published_at": "2026-11-22T10:00:00Z",
                        "assets": [
                            {
                                "name": "t3agent37-macos-aarch64.app.tar.gz",
                                "browser_download_url": "https://example.test/aarch64.tar.gz"
                            },
                            {
                                "name": "t3agent37-macos-x86_64.app.tar.gz",
                                "browser_download_url": "https://example.test/x86_64.tar.gz"
                            },
                            {
                                "name": "SHA256SUMS.txt",
                                "browser_download_url": "https://example.test/SHA256SUMS.txt"
                            }
                        ]
                    }))
                }),
            )
            .route("/empty", get(|| async { StatusCode::NOT_FOUND }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = http_client().unwrap();
        let info = check_latest_from(&client, &format!("http://{addr}/latest"), "0.2.0", "macos", "aarch64")
            .await
            .unwrap();
        assert!(info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("0.3.0"));
        assert_eq!(info.asset_name.as_deref(), Some("t3agent37-macos-aarch64.app.tar.gz"));
        assert_eq!(info.asset_url.as_deref(), Some("https://example.test/aarch64.tar.gz"));
        assert_eq!(info.checksums_url.as_deref(), Some("https://example.test/SHA256SUMS.txt"));
        assert!(info.can_self_update);
        assert_eq!(info.status, UpdateStatus::Available);

        let same = check_latest_from(&client, &format!("http://{addr}/latest"), "0.3.0", "macos", "aarch64")
            .await
            .unwrap();
        assert!(!same.update_available);
        assert_eq!(same.status, UpdateStatus::UpToDate);

        let none = check_latest_from(&client, &format!("http://{addr}/empty"), "0.2.0", "macos", "aarch64")
            .await
            .unwrap();
        assert!(!none.update_available);
        assert_eq!(none.status, UpdateStatus::UpToDate);
    }
}
