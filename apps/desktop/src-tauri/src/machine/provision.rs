//! Creating and upgrading Agent37 machines: everything that runs on the sandbox goes through
//! `provision.sh`, which is idempotent and works on both our custom `t3agent37` template and
//! stock `agent37-base` containers (no systemd required).

use crate::agent37::{
    self, Agent37, CreateSpec, InstanceType, Region, ShapeInfo, SHAPES,
};
use crate::proxy::auth::random_token;
use crate::store::Machine;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::Deserialize;

const PROVISION_SH: &str = include_str!("provision.sh");
const WATCHDOG_MJS: &str = include_str!("watchdog.mjs");
const SUPERVISOR_MJS: &str = include_str!("../../../../../template/supervisor.mjs");
pub const T3_PORT: u16 = 3773;
pub const WATCHDOG_PORT: u16 = 3774;

/// Re-export Agent37 shapes for UI and MCP tool schemas.
pub type SizeInfo = ShapeInfo;
pub const SIZES: &[SizeInfo] = SHAPES;

pub fn size(id: &str) -> Result<&'static SizeInfo> {
    agent37::shape(id)
}

/// Identifies the provisioning scripts; a machine provisioned with others gets them on wake.
pub fn revision() -> String {
    let hash = fnv1a(
        PROVISION_SH
            .bytes()
            .chain(WATCHDOG_MJS.bytes())
            .chain(SUPERVISOR_MJS.bytes())
            .chain(SLEEP_AFTER_SECS.to_string().bytes()),
    );
    format!("{hash:016x}")
}

/// FNV-1a: stable across builds, unlike std's hasher.
pub fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes
        .into_iter()
        .fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provisioned {
    pub environment_id: String,
    pub upstream_token: Option<String>,
}

/// How long a machine stays up after the last agent turn and the last action through t3agent37.
pub const SLEEP_AFTER_SECS: u64 = 180;

/// Uploads the provisioning bundle (`provision.sh`, `watchdog.mjs`, `supervisor.mjs`) without
/// leaking the watchdog secret into any command line, and runs `provision.sh`.
pub async fn run(
    agent37: &Agent37,
    instance_id: &str,
    name: &str,
    t3_version: &str,
    watchdog_secret: &str,
    mint_upstream_token: bool,
) -> Result<Provisioned> {
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let script = format!(
        "set -e; d=$(mktemp -d); trap 'rm -rf \"$d\" \"$0\"' EXIT\n\
         echo {p} | base64 -d > \"$d/provision.sh\"\n\
         echo {w} | base64 -d > \"$d/watchdog.mjs\"\n\
         echo {sup} | base64 -d > \"$d/supervisor.mjs\"\n\
         T3BOAT_SECRET={s} bash \"$d/provision.sh\" {v} {sleep} {mint} {name}\n",
        p = b64(PROVISION_SH),
        w = b64(WATCHDOG_MJS),
        sup = b64(SUPERVISOR_MJS),
        v = shell_word(t3_version)?,
        s = shell_word(watchdog_secret)?,
        sleep = SLEEP_AFTER_SECS,
        mint = u8::from(mint_upstream_token),
        name = shell_word(name)?,
    );
    let remote = format!("/tmp/t3agent37-provision-{}.sh", random_token());
    agent37.upload_small(instance_id, script.as_bytes(), &remote).await?;
    let out = agent37.exec_ok(instance_id, &format!("bash {remote}"), 600).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .ok_or_else(|| anyhow!("provisioning printed no result"))?;
    Ok(serde_json::from_str(line)?)
}

/// Ensures the lightweight supervisor is running after a cold-storage / fresh boot on stock
/// `agent37-base` instances whose container entrypoint didn't start `supervisor.mjs`.
pub async fn ensure_supervisor_running(agent37: &Agent37, instance_id: &str) -> Result<()> {
    let script = r#"
        set -e
        dir="$HOME/.t3agent37"
        pidfile="$dir/supervisor.pid"
        if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile" 2>/dev/null)" 2>/dev/null; then
            exit 0
        fi
        if [ -f "$dir/supervisor.mjs" ]; then
            nohup node --no-warnings --max-old-space-size=64 "$dir/supervisor.mjs" >> "$dir/supervisor.log" 2>&1 </dev/null &
        fi
    "#;
    agent37.exec_ok(instance_id, script, 30).await?;
    Ok(())
}

/// Only non-empty `[A-Za-z0-9._-]` reaches the remote shell.
fn shell_word(s: &str) -> Result<&str> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        bail!("unsafe value for the remote shell: {s}");
    }
    Ok(s)
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    agent37: &Agent37,
    name: &str,
    size_id: &str,
    instance_type: InstanceType,
    region: Region,
    disk_gb: Option<u32>,
    template: &str,
    auto_sleep_fuse_minutes: Option<u32>,
    t3_version: &str,
    local_port: u16,
    created: impl FnOnce(&str),
    progress: impl Fn(&str),
) -> Result<Machine> {
    let shape = agent37::shape(size_id)?;
    let resources = agent37::validate_shape_and_disk(instance_type, shape.id, disk_gb)?;
    progress("Creating instance on Agent37");
    let spec = CreateSpec {
        template: template.to_owned(),
        instance_type,
        region,
        resources,
        auto_sleep: auto_sleep_fuse_minutes.is_some(),
        idle_timeout_seconds: auto_sleep_fuse_minutes.map(|m| m.saturating_mul(60)),
    };
    let inst = agent37
        .create_raw(&spec)
        .await
        .context("creating the Agent37 instance")?;
    let id = inst.id.clone();
    // Register with the orphan reaper IMMEDIATELY before waiting for boot so a failed/timed-out
    // readiness poll never leaks a billable instance.
    created(&id);
    if !inst.is_running() {
        agent37
            .wait_until_running(&id)
            .await
            .context("waiting for Agent37 instance to reach running")?;
    }

    progress("Installing T3 Code");
    let secret = random_token();
    let provisioned = run(agent37, &id, name, t3_version, &secret, true).await?;

    let upstream_url = agent37.preview_url(&id, T3_PORT);
    let watchdog_url = agent37.preview_url(&id, WATCHDOG_PORT);
    Ok(Machine {
        id: id.clone(),
        sandbox_id: id,
        name: name.to_owned(),
        size: shape.id.to_owned(),
        instance_type,
        region,
        disk_gb: resources.disk,
        template: template.to_owned(),
        upstream_url,
        upstream_token: provisioned
            .upstream_token
            .ok_or_else(|| anyhow!("provisioning issued no token"))?,
        watchdog_url,
        watchdog_secret: secret,
        local_port,
        t3_version: t3_version.to_owned(),
        provision_revision: revision(),
        environment_id: provisioned.environment_id,
        client_tokens: Vec::new(),
        link_tokens: Vec::new(),
        created_at: chrono::Utc::now().timestamp(),
        next_due_at: None,
        connect: false,
        github_signed_in: false,
    })
}
