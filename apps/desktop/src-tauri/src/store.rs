//! Persistent app state: machines, settings and this month's spend, one JSON file in the app
//! data dir.

use crate::agent37::{self, InstanceType, Region};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Never changes: the id of the Agent37 instance the machine was created on.
    pub id: String,
    /// Agent37 instance id (same as `id` on Agent37; kept for clarity).
    pub sandbox_id: String,
    pub name: String,
    /// Shape id: small | medium | large | xlarge | max.
    pub size: String,
    #[serde(default)]
    pub instance_type: InstanceType,
    #[serde(default)]
    pub region: Region,
    #[serde(default = "default_disk_gb")]
    pub disk_gb: u32,
    #[serde(default = "default_template")]
    pub template: String,
    /// Authenticated HTTPS URL of the T3 server (`https://{id}-3773.agent37.app`).
    pub upstream_url: String,
    /// Bearer token the proxy uses upstream. Never leaves this computer except to that server.
    pub upstream_token: String,
    /// Authenticated HTTPS URL of the read-only watchdog (`https://{id}-3774.agent37.app`).
    pub watchdog_url: String,
    pub watchdog_secret: String,
    /// Loopback port the proxy serves this machine on. Stable, T3 Code stores it.
    pub local_port: u16,
    /// T3 server version installed on the machine.
    pub t3_version: String,
    /// `provision::revision()` of the scripts the machine runs.
    pub provision_revision: String,
    pub environment_id: String,
    /// Bearer tokens the proxy issued to T3 Code when it paired.
    pub client_tokens: Vec<String>,
    /// Bearer tokens the machine's server issued to T3 servers that linked to it through the
    /// proxy. The proxy only recognizes them: a call bearing one may wake the machine.
    pub link_tokens: Vec<LinkToken>,
    pub created_at: i64,
    /// When T3 on the machine next starts work on its own (an automation, a held webhook run, an
    /// agent resuming after a usage limit), as its watchdog last said: ms since epoch.
    #[serde(default)]
    pub next_due_at: Option<i64>,
    /// T3 Connect is on there: other devices reach it with the user's T3 account.
    #[serde(default)]
    pub connect: bool,
    /// Whether `gh` is authenticated on the instance for private repository clones.
    #[serde(default)]
    pub github_signed_in: bool,
}

fn default_disk_gb() -> u32 {
    12
}

fn default_template() -> String {
    agent37::DEFAULT_TEMPLATE.to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkToken {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub start_at_login: bool,
    /// Agent37 template used for newly created machines (`agent37-base` or custom `t3agent37`).
    pub template: String,
    /// Optional platform-side `auto_sleep` safety fuse in minutes (30..1440) for when the laptop
    /// closes; `None` disables platform auto-sleep so quiet long-running agents are never frozen.
    pub auto_sleep_fuse_minutes: Option<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_at_login: true,
            template: agent37::DEFAULT_TEMPLATE.to_owned(),
            auto_sleep_fuse_minutes: None,
        }
    }
}

/// One instance's estimated spend tracked locally (Agent37's per-instance ledger stays in its
/// dashboard). Kept after deletion so monthly totals stay accurate until the month rolls over.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// Id of the machine that ran on the instance.
    pub machine: String,
    /// Start of the local day of the last accrual, unix seconds.
    pub day: i64,
    /// Start of the local month of the last accrual, unix seconds.
    #[serde(default)]
    pub month_start: i64,
    /// Dollars accrued since `day` (compute + disk).
    pub today: f64,
    pub awake_secs_today: u64,
    /// Dollars accrued since `month_start` (compute + disk).
    pub month: f64,
    /// Unix seconds of the last accrual tick.
    #[serde(default)]
    pub last_tick_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub machines: Vec<Machine>,
    pub settings: Settings,
    /// By instance id.
    pub spend: HashMap<String, Spend>,
    /// Local ports of deleted machines, never given out again: T3 Code may still list one (it
    /// cannot be told to forget a backend), and would reach a new machine there as the old.
    pub retired_ports: Vec<u16>,
    /// Instances to keep deleting on Agent37 until `GET /v1/instances` confirms each gone.
    pub doomed: Vec<Doomed>,
}

/// A deleted machine's instance, or one from a create that failed or was interrupted: an instance
/// left behind bills retained disk forever until deleted.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Doomed {
    pub sandbox: Option<String>,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join("state.json") }
    }

    pub fn load(&self) -> Result<State> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// The file holds bearer tokens.
    pub fn save(&self, state: &State) -> Result<()> {
        write_private(&self.path, &serde_json::to_vec_pretty(state)?)
    }
}

/// Atomic write of a file only its owner can read, never readable by others even briefly.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().expect("path has a parent"))?;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes everything in `dir` but `keep`, whatever an interrupted write left behind included.
pub fn sweep(dir: &Path, keep: &HashSet<PathBuf>) {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    for path in entries.map(|e| e.path()).filter(|p| !keep.contains(p)) {
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}
