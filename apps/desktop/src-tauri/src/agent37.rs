//! Agent37 REST API client, pricing model, API key storage, and HTTPS OAuth port forwarding.
//!
//! Uses pure HTTPS/JSON (`reqwest` with rustls) so no external CLI is required for lifecycle,
//! exec, billing, or OAuth callback forwarding. The workspace `sk_live_...` key lives only on
//! the local host (never inside sandboxes) and is injected on upstream requests via `X-Agent37-Key`.

use crate::store;
use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body as AxumBody;
use axum::extract::Request as AxumRequest;
use axum::response::{IntoResponse, Response as AxumResponse};
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEFAULT_API_BASE: &str = "https://api.agent37.com";
pub const DEFAULT_SANDBOX_BASE: &str = "https://sandbox.agent37.app";
pub const DEFAULT_PREVIEW_TEMPLATE: &str = "https://{id}-{port}.agent37.app";
pub const HEADER_API_KEY: &str = "x-agent37-key";
pub const DEFAULT_TEMPLATE: &str = "agent37-base";

/// Retry budget for `409 try_again` while Agent37 finishes writing a sleep checkpoint.
const CHECKPOINT_RETRY_WINDOW: Duration = Duration::from_secs(90);
/// Wait budget for `202` host-move transitions during `start` or `resize`.
const MOVE_POLL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HOURS_PER_MONTH: f64 = 730.0;
const CPU_MONTHLY_PER_CORE: f64 = 0.80;
const RAM_MONTHLY_PER_GB: f64 = 0.70;
pub const DISK_MONTHLY_PER_GB: f64 = 0.09;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstanceType {
    #[default]
    Default,
    Vm,
    Performance,
}

impl InstanceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Vm => "vm",
            Self::Performance => "performance",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "default" | "container" => Ok(Self::Default),
            "vm" => Ok(Self::Vm),
            "performance" | "perf" => Ok(Self::Performance),
            other => bail!("unknown Agent37 instance type {other} (expected default, vm, or performance)"),
        }
    }

    /// Compute price multiplier relative to `default` container pricing.
    pub fn compute_multiplier(self) -> f64 {
        match self {
            Self::Default => 1.0,
            Self::Vm => 2.5,
            Self::Performance => 4.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Region {
    #[default]
    #[serde(rename = "us")]
    Us,
    #[serde(rename = "eu")]
    Eu,
    #[serde(rename = "ap-south")]
    ApSouth,
}

impl Region {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Eu => "eu",
            Self::ApSouth => "ap-south",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "us" => Ok(Self::Us),
            "eu" => Ok(Self::Eu),
            "ap-south" => Ok(Self::ApSouth),
            other => bail!("unknown Agent37 region {other} (expected us, eu, or ap-south)"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Resources {
    pub cpu: u32,
    pub memory: u32,
    pub disk: u32,
}

/// Valid Agent37 compute shapes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapeInfo {
    pub id: &'static str,
    pub spec: &'static str,
    pub cpu: u32,
    pub memory: u32,
    pub min_disk: u32,
    pub max_disk: u32,
    pub default_disk: u32,
    /// Hourly compute rate for `default` container (multiply by `InstanceType::compute_multiplier`
    /// for `vm` or `performance`; disk bills separately at `$0.09/GB/mo`).
    pub hourly: f64,
    pub vm_only: bool,
}

pub const SHAPES: &[ShapeInfo] = &[
    ShapeInfo {
        id: "small",
        spec: "2 vCPU · 4 GB",
        cpu: 2,
        memory: 4,
        min_disk: 2,
        max_disk: 12,
        default_disk: 12,
        hourly: (2.0 * CPU_MONTHLY_PER_CORE + 4.0 * RAM_MONTHLY_PER_GB) / HOURS_PER_MONTH,
        vm_only: false,
    },
    ShapeInfo {
        id: "medium",
        spec: "4 vCPU · 8 GB",
        cpu: 4,
        memory: 8,
        min_disk: 2,
        max_disk: 20,
        default_disk: 12,
        hourly: (4.0 * CPU_MONTHLY_PER_CORE + 8.0 * RAM_MONTHLY_PER_GB) / HOURS_PER_MONTH,
        vm_only: false,
    },
    ShapeInfo {
        id: "large",
        spec: "4 vCPU · 16 GB",
        cpu: 4,
        memory: 16,
        min_disk: 2,
        max_disk: 40,
        default_disk: 20,
        hourly: (4.0 * CPU_MONTHLY_PER_CORE + 16.0 * RAM_MONTHLY_PER_GB) / HOURS_PER_MONTH,
        vm_only: false,
    },
    ShapeInfo {
        id: "xlarge",
        spec: "4 vCPU · 32 GB",
        cpu: 4,
        memory: 32,
        min_disk: 2,
        max_disk: 80,
        default_disk: 32,
        hourly: (4.0 * CPU_MONTHLY_PER_CORE + 32.0 * RAM_MONTHLY_PER_GB) / HOURS_PER_MONTH,
        vm_only: false,
    },
    ShapeInfo {
        id: "max",
        spec: "8 vCPU · 64 GB",
        cpu: 8,
        memory: 64,
        min_disk: 2,
        max_disk: 80,
        default_disk: 40,
        hourly: (8.0 * CPU_MONTHLY_PER_CORE + 64.0 * RAM_MONTHLY_PER_GB) / HOURS_PER_MONTH,
        vm_only: true,
    },
];

pub fn shape(id: &str) -> Result<&'static ShapeInfo> {
    // Accept Boat's legacy "default" alias as "small" (entry 2vCPU/4GB) or "medium" if needed.
    let normalized = match id {
        "default" => "small",
        other => other,
    };
    SHAPES
        .iter()
        .find(|s| s.id == normalized)
        .ok_or_else(|| anyhow!("unknown size {id} (valid: small, medium, large, xlarge, max)"))
}

/// Compute-only hourly rate in USD while awake.
pub fn compute_hourly(instance_type: InstanceType, cpu: u32, memory_gb: u32) -> f64 {
    let base_monthly = f64::from(cpu) * CPU_MONTHLY_PER_CORE + f64::from(memory_gb) * RAM_MONTHLY_PER_GB;
    (base_monthly * instance_type.compute_multiplier()) / HOURS_PER_MONTH
}

/// Disk monthly cost in USD (billed continuously, awake or asleep, until deleted).
pub fn disk_monthly(disk_gb: u32) -> f64 {
    f64::from(disk_gb) * DISK_MONTHLY_PER_GB
}

/// Disk hourly cost in USD.
pub fn disk_hourly(disk_gb: u32) -> f64 {
    disk_monthly(disk_gb) / HOURS_PER_MONTH
}

/// Total hourly rate in USD while awake (compute + disk).
pub fn total_hourly(instance_type: InstanceType, resources: Resources) -> f64 {
    compute_hourly(instance_type, resources.cpu, resources.memory) + disk_hourly(resources.disk)
}

pub fn validate_shape_and_disk(instance_type: InstanceType, size_id: &str, disk_gb: Option<u32>) -> Result<Resources> {
    let s = shape(size_id)?;
    if s.vm_only && instance_type == InstanceType::Default {
        bail!("size {} requires instance type `vm` or `performance`", s.id);
    }
    let disk = disk_gb.unwrap_or(s.default_disk);
    if disk < s.min_disk || disk > s.max_disk {
        bail!("disk for size {} must be between {} and {} GB (got {} GB)", s.id, s.min_disk, s.max_disk, disk);
    }
    Ok(Resources { cpu: s.cpu, memory: s.memory, disk })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSpec {
    pub template: String,
    pub instance_type: InstanceType,
    pub region: Region,
    pub resources: Resources,
    pub auto_sleep: bool,
    pub idle_timeout_seconds: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Instance {
    pub id: String,
    pub status: String,
    #[serde(rename = "type", default)]
    pub instance_type: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub url: Option<String>,
    /// Unix seconds or RFC3339 timestamp when the instance went to sleep/stopped.
    #[serde(default, deserialize_with = "deserialize_opt_timestamp")]
    pub slept_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub auto_sleep: Option<bool>,
    #[serde(default)]
    pub idle_timeout_seconds: Option<u32>,
}

impl Instance {
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "running" | "ready")
    }

    pub fn is_asleep_or_stopped(&self) -> bool {
        matches!(self.status.as_str(), "sleeping" | "stopped" | "suspended")
    }

    pub fn is_starting(&self) -> bool {
        matches!(self.status.as_str(), "starting" | "creating" | "waking" | "updating" | "provisioning")
    }

    pub fn is_stopping(&self) -> bool {
        matches!(self.status.as_str(), "stopping" | "checkpointing")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BillingInfo {
    /// Current wallet balance in USD.
    pub balance_dollars: f64,
    /// Maximum concurrent awake memory across all instances in GB.
    pub memory_limit_gb: u32,
    /// Workspace tier label, if reported.
    pub tier: Option<String>,
    pub auto_top_up: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecResult {
    #[serde(alias = "exitCode", alias = "exit_code")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

#[derive(Clone)]
pub struct Agent37 {
    api_key: String,
    api_base: String,
    sandbox_base: String,
    preview_template: String,
    http: reqwest::Client,
    retry_base_delay: Duration,
}

/// Never leak `api_key` into logs or panic messages.
impl fmt::Debug for Agent37 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent37")
            .field("api_key", &"[REDACTED]")
            .field("api_base", &self.api_base)
            .field("sandbox_base", &self.sandbox_base)
            .finish()
    }
}

impl Agent37 {
    pub fn new(api_key: String) -> Result<Self> {
        Self::with_endpoints(
            api_key,
            DEFAULT_API_BASE.to_owned(),
            DEFAULT_SANDBOX_BASE.to_owned(),
            DEFAULT_PREVIEW_TEMPLATE.to_owned(),
        )
    }

    pub fn with_endpoints(
        api_key: String,
        api_base: String,
        sandbox_base: String,
        preview_template: String,
    ) -> Result<Self> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            bail!("Agent37 API key is empty");
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(4)
            .pool_idle_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            api_key,
            api_base: api_base.trim_end_matches('/').to_owned(),
            sandbox_base: sandbox_base.trim_end_matches('/').to_owned(),
            preview_template,
            http,
            retry_base_delay: Duration::from_millis(500),
        })
    }

    #[cfg(test)]
    pub fn with_fast_retries(mut self) -> Self {
        self.retry_base_delay = Duration::from_millis(10);
        self
    }

    /// Builds an `Agent37` client from an explicit key, honoring optional `AGENT37_*` endpoint env overrides.
    pub fn from_key_with_env(key: String) -> Result<Self> {
        let api_base = std::env::var("AGENT37_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_owned());
        let sandbox_base = std::env::var("AGENT37_SANDBOX_BASE").unwrap_or_else(|_| DEFAULT_SANDBOX_BASE.to_owned());
        let preview_template =
            std::env::var("AGENT37_PREVIEW_TEMPLATE").unwrap_or_else(|_| DEFAULT_PREVIEW_TEMPLATE.to_owned());
        Self::with_endpoints(key, api_base, sandbox_base, preview_template)
    }

    /// Loads an API key from `AGENT37_API_KEY`, `<data_dir>/agent37-key`, or
    /// `~/.config/agent37/config.json`.
    pub fn locate(data_dir: Option<&Path>) -> Option<Self> {
        let key = load_api_key(data_dir)?;
        Self::from_key_with_env(key).ok()
    }

    /// Header value for `X-Agent37-Key` used by the localhost proxy on upstream requests & WS upgrades.
    pub fn edge_key_header(&self) -> Result<(HeaderName, HeaderValue)> {
        let name = HeaderName::from_static(HEADER_API_KEY);
        let value = HeaderValue::from_str(&self.api_key).context("invalid Agent37 API key header value")?;
        Ok((name, value))
    }

    pub fn edge_headers(&self) -> Result<HeaderMap> {
        let mut map = HeaderMap::new();
        let (k, v) = self.edge_key_header()?;
        map.insert(k, v);
        Ok(map)
    }

    /// Preview URL for a given port on an instance: `https://{id}-{port}.agent37.app` in production.
    pub fn preview_url(&self, instance_id: &str, port: u16) -> String {
        self.preview_template
            .replace("{id}", instance_id)
            .replace("{port}", &port.to_string())
            .trim_end_matches('/')
            .to_owned()
    }

    /// Validates the API key and reads wallet balance + concurrency tier via `GET /v1/billing`.
    pub async fn billing(&self) -> Result<BillingInfo> {
        let v = self
            .api_json(Method::GET, "/v1/billing", None, Duration::from_secs(20))
            .await
            .context("reading Agent37 billing")?;
        Ok(parse_billing(&v))
    }

    /// Submits `POST /v1/instances` and returns the created instance object immediately (before
    /// waiting for `running`) so callers can persist its ID for orphan cleanup right away.
    pub async fn create_raw(&self, spec: &CreateSpec) -> Result<Instance> {
        let mut body = json!({
            "template": spec.template,
            "type": spec.instance_type.as_str(),
            "region": spec.region.as_str(),
            "resources": {
                "cpu": spec.resources.cpu,
                "memory": spec.resources.memory,
                "disk": spec.resources.disk,
            },
            "auto_sleep": spec.auto_sleep,
        });
        if let Some(idle) = spec.idle_timeout_seconds {
            body["idle_timeout_seconds"] = json!(idle.clamp(300, 86_400));
        }
        let v = self
            .api_json(Method::POST, "/v1/instances", Some(body), Duration::from_secs(300))
            .await
            .context("creating Agent37 instance")?;
        parse_instance(v)
    }

    pub async fn create(&self, spec: &CreateSpec) -> Result<Instance> {
        let inst = self.create_raw(spec).await?;
        if !inst.is_running() {
            return self.wait_until_running(&inst.id).await;
        }
        Ok(inst)
    }

    pub async fn wait_until_running(&self, id: &str) -> Result<Instance> {
        self.wait_for_running(id, MOVE_POLL_TIMEOUT).await
    }

    pub async fn get(&self, id: &str) -> Result<Instance> {
        let v = self
            .api_json(Method::GET, &format!("/v1/instances/{id}"), None, Duration::from_secs(20))
            .await
            .with_context(|| format!("fetching Agent37 instance {id}"))?;
        parse_instance(v)
    }

    pub async fn list(&self) -> Result<Vec<Instance>> {
        let v = self
            .api_json(Method::GET, "/v1/instances", None, Duration::from_secs(20))
            .await
            .context("listing Agent37 instances")?;
        let arr = v
            .as_array()
            .or_else(|| v.get("instances").and_then(Value::as_array))
            .or_else(|| v.get("data").and_then(Value::as_array))
            .ok_or_else(|| anyhow!("unexpected Agent37 list response"))?;
        arr.iter().cloned().map(parse_instance).collect()
    }

    /// Confirmed gone only when Agent37's list succeeds and does not include `id`.
    pub async fn confirmed_gone(&self, id: &str) -> bool {
        matches!(self.list().await, Ok(list) if list.iter().all(|i| i.id != id))
    }

    /// Wakes a sleeping/stopped instance, retrying `409 try_again` (in-flight checkpoint write)
    /// and polling `202` host-move migrations until `running`.
    pub async fn start(&self, id: &str) -> Result<Instance> {
        let (status, body) = self
            .api_with_409_retry(Method::POST, &format!("/v1/instances/{id}/start"), None, Duration::from_secs(120))
            .await
            .with_context(|| format!("starting Agent37 instance {id}"))?;
        if status == StatusCode::ACCEPTED || body["status"].as_str().is_some_and(|s| s != "running") {
            return self.wait_for_running(id, MOVE_POLL_TIMEOUT).await;
        }
        self.get(id).await
    }

    /// Checkpoints memory & processes to disk (`POST /v1/instances/{id}/sleep`) so billing drops
    /// to disk-only. Retries `409 try_again`.
    pub async fn sleep(&self, id: &str) -> Result<()> {
        self.api_with_409_retry(Method::POST, &format!("/v1/instances/{id}/sleep"), None, Duration::from_secs(120))
            .await
            .with_context(|| format!("sleeping Agent37 instance {id}"))?;
        Ok(())
    }

    /// Halts the instance (`POST /v1/instances/{id}/stop`), discarding any checkpoint while keeping disk.
    pub async fn stop(&self, id: &str) -> Result<()> {
        self.api_with_409_retry(Method::POST, &format!("/v1/instances/{id}/stop"), None, Duration::from_secs(120))
            .await
            .with_context(|| format!("stopping Agent37 instance {id}"))?;
        Ok(())
    }

    /// Updates `auto_sleep` and/or `idle_timeout_seconds` without restarting the instance.
    pub async fn patch_auto_sleep(&self, id: &str, auto_sleep: bool, idle_timeout_seconds: Option<u32>) -> Result<()> {
        let mut body = json!({ "auto_sleep": auto_sleep });
        if let Some(secs) = idle_timeout_seconds {
            body["idle_timeout_seconds"] = json!(secs.clamp(300, 86_400));
        }
        self.api_with_409_retry(Method::PATCH, &format!("/v1/instances/{id}"), Some(body), Duration::from_secs(30))
            .await
            .with_context(|| format!("updating auto_sleep on {id}"))?;
        Ok(())
    }

    /// Resizes a **running** instance (`POST /v1/instances/{id}/resize`). Keeps ID and URLs;
    /// recreates container (in-memory state is lost). Handles `202` host-move migrations.
    pub async fn resize(&self, id: &str, cpu: Option<u32>, memory: Option<u32>, disk: Option<u32>) -> Result<Instance> {
        let mut body = serde_json::Map::new();
        if let Some(c) = cpu {
            body.insert("cpu".into(), json!(c));
        }
        if let Some(m) = memory {
            body.insert("memory".into(), json!(m));
        }
        if let Some(d) = disk {
            body.insert("disk".into(), json!(d));
        }
        if body.is_empty() {
            bail!("resize requires at least one of cpu, memory, or disk");
        }
        let (status, resp) = self
            .api_with_409_retry(
                Method::POST,
                &format!("/v1/instances/{id}/resize"),
                Some(Value::Object(body)),
                Duration::from_secs(180),
            )
            .await
            .with_context(|| format!("resizing Agent37 instance {id}"))?;
        if status == StatusCode::ACCEPTED || resp["status"].as_str().is_some_and(|s| s != "running") {
            return self.wait_for_running(id, MOVE_POLL_TIMEOUT).await;
        }
        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let path = format!("/v1/instances/{id}");
        let started = Instant::now();
        let mut delay = self.retry_base_delay;
        loop {
            let url = format!("{}{path}", self.api_base);
            let res = self
                .http
                .delete(&url)
                .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .with_context(|| format!("deleting Agent37 instance {id}"))?;
            let status = res.status();
            if status == StatusCode::NOT_FOUND || status.is_success() {
                return Ok(());
            }
            let text = res.text().await.unwrap_or_default();
            if should_retry_try_again(status, &text) && started.elapsed() < CHECKPOINT_RETRY_WINDOW {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
                continue;
            }
            bail!("Agent37 DELETE {id} failed ({status}): {}", extract_error_message(&text));
        }
    }

    /// Runs a shell command inside the instance via `https://sandbox.agent37.app/v1/instances/{id}/exec`
    /// with `X-Agent37-Key`, retrying transient `409`/`503 try_again` responses while a checkpoint
    /// or host routing transition is finishing.
    pub async fn exec(
        &self,
        id: &str,
        script: &str,
        user: Option<&str>,
        timeout_secs: u64,
    ) -> Result<ExecResult> {
        let url = format!("{}/v1/instances/{id}/exec", self.sandbox_base);
        let mut payload = json!({ "command": script });
        if let Some(u) = user {
            payload["user"] = json!(u);
        }
        let started = Instant::now();
        let mut delay = self.retry_base_delay;
        loop {
            let res = self
                .http
                .post(&url)
                .header(HEADER_API_KEY, &self.api_key)
                .json(&payload)
                .timeout(Duration::from_secs(timeout_secs.saturating_add(30)))
                .send()
                .await
                .with_context(|| format!("exec on Agent37 instance {id}"))?;
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            if should_retry_try_again(status, &text) && started.elapsed() < CHECKPOINT_RETRY_WINDOW {
                tracing::debug!("Agent37 exec on {id} returned {status} try_again; retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
                continue;
            }
            if !status.is_success() {
                bail!("Agent37 exec failed ({status}): {}", extract_error_message(&text));
            }
            let parsed: ExecResult = serde_json::from_str(&text)
                .with_context(|| format!("parsing Agent37 exec response: {text}"))?;
            return Ok(parsed);
        }
    }

    pub async fn exec_ok(&self, id: &str, script: &str, timeout_secs: u64) -> Result<String> {
        let r = self.exec(id, script, None, timeout_secs).await?;
        if r.exit_code != Some(0) {
            let detail = if r.stderr.trim().is_empty() { r.stdout.trim() } else { r.stderr.trim() };
            bail!("command on the machine failed ({:?}): {detail}", r.exit_code);
        }
        Ok(r.stdout)
    }

    /// Uploads a small text/script payload by base64-streaming over `exec` (stays well under the
    /// 1 MB exec body limit; no separate `scp` dependency needed for provisioning scripts).
    pub async fn upload_small(&self, id: &str, bytes: &[u8], remote_path: &str) -> Result<()> {
        use base64::Engine;
        if bytes.len() > 512 * 1024 {
            bail!("upload_small payload too large ({} bytes); use scp over agent37 ssh", bytes.len());
        }
        if remote_path.is_empty()
            || !remote_path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
        {
            bail!("unsafe remote upload path: {remote_path}");
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let cmd = format!(
            "umask 077; printf '%s' '{encoded}' | base64 -d > '{remote_path}' && chmod 600 '{remote_path}'"
        );
        self.exec_ok(id, &cmd, 60).await?;
        Ok(())
    }

    /// Forwards HTTP OAuth callbacks on `127.0.0.1:<port>` (and `[::1]:<port>` when available)
    /// to `127.0.0.1:<port>` inside the Agent37 instance over Agent37's authenticated HTTPS
    /// preview proxy — no external `agent37` CLI or SSH setup required.
    pub async fn forward(&self, id: &str, port: u16) -> Result<PortForward> {
        let v4 = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|_| anyhow!("port {port} is in use on this computer; finish or close the other sign-in first"))?;
        // Best-effort IPv6 loopback so `http://localhost:<port>` works even when resolved to `::1`.
        let v6 = tokio::net::TcpListener::bind(("::1", port)).await.ok();

        let mut secret_bytes = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut secret_bytes);
        let relay_secret: String = secret_bytes.iter().map(|b| format!("{b:02x}")).collect();

        use base64::Engine;
        let script_b64 = base64::engine::general_purpose::STANDARD.encode(OAUTH_RELAY_JS.as_bytes());
        let start_cmd = format!(
            "set -e; umask 077; d=$(mktemp -d); \
             printf '%s' '{script_b64}' | base64 -d > \"$d/relay.js\"; \
             printf '%s' '{relay_secret}' > \"$d/secret\"; \
             PATH=\"$HOME/.local/bin:/usr/local/bin:$PATH\" nohup node \"$d/relay.js\" {port} \"$d/secret\" \"$d/port\" </dev/null >\"$d/log\" 2>&1 & \
             pid=$!; \
             for i in $(seq 1 40); do \
               [ -s \"$d/port\" ] && break; \
               kill -0 \"$pid\" 2>/dev/null || break; \
               sleep 0.1; \
             done; \
             if [ ! -s \"$d/port\" ]; then \
               cat \"$d/log\" >&2; rm -rf \"$d\"; exit 1; \
             fi; \
             printf 'RELAY %s %s %s\\n' \"$(cat \"$d/port\")\" \"$pid\" \"$d\""
        );
        let out = self
            .exec_ok(id, &start_cmd, 30)
            .await
            .context("starting OAuth loopback relay inside the machine")?;
        let (bridge_port, remote_pid, remote_dir) = parse_relay_start(&out)?;

        let upstream_base = self.preview_url(id, bridge_port);
        let proxy_http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .context("building OAuth forward HTTP client")?;
        let state = Arc::new(OAuthForwardState {
            http: proxy_http,
            upstream_base,
            api_key: self.api_key.clone(),
            relay_secret,
            default_host: format!("127.0.0.1:{port}"),
        });

        let mut tasks = Vec::with_capacity(2);
        let router = axum::Router::new().fallback(proxy_oauth_request).with_state(state.clone());
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(v4, router).await;
        }));
        if let Some(v6_listener) = v6 {
            let router6 = axum::Router::new().fallback(proxy_oauth_request).with_state(state);
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(v6_listener, router6).await;
            }));
        }

        Ok(PortForward {
            tasks,
            cleanup: Some(RelayCleanup {
                agent37: self.clone(),
                instance_id: id.to_owned(),
                pid: remote_pid,
                dir: remote_dir,
            }),
        })
    }

    async fn wait_for_running(&self, id: &str, timeout: Duration) -> Result<Instance> {
        let started = Instant::now();
        let mut delay = self.retry_base_delay;
        loop {
            let inst = self.get(id).await?;
            if inst.is_running() {
                return Ok(inst);
            }
            if started.elapsed() > timeout {
                bail!("timed out waiting for Agent37 instance {id} to reach running (last status: {})", inst.status);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(3));
        }
    }

    async fn api_json(&self, method: Method, path: &str, body: Option<Value>, timeout: Duration) -> Result<Value> {
        let (_, v) = self.api_raw(method, path, body, timeout).await?;
        Ok(v)
    }

    async fn api_with_409_retry(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<(StatusCode, Value)> {
        let started = Instant::now();
        let mut delay = self.retry_base_delay;
        loop {
            let url = format!("{}{path}", self.api_base);
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                .timeout(timeout);
            if let Some(ref b) = body {
                req = req.json(b);
            }
            let res = req.send().await?;
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            if should_retry_try_again(status, &text) && started.elapsed() < CHECKPOINT_RETRY_WINDOW {
                tracing::debug!("Agent37 {method} {path} returned {status} try_again; retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
                continue;
            }
            if !status.is_success() {
                bail!("Agent37 {method} {path} failed ({status}): {}", extract_error_message(&text));
            }
            let value = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text)? };
            return Ok((status, value));
        }
    }

    async fn api_raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<(StatusCode, Value)> {
        let url = format!("{}{path}", self.api_base);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .timeout(timeout);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Agent37 {method} {path} failed ({status}): {}", extract_error_message(&text));
        }
        let value = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text)? };
        Ok((status, value))
    }
}

/// Saves the Agent37 API key with `0600` permissions in `<data_dir>/agent37-key`.
pub fn save_api_key(data_dir: &Path, key: &str) -> Result<()> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        bail!("API key cannot be empty");
    }
    store::write_private(&api_key_path(data_dir), trimmed.as_bytes())
}

pub fn api_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("agent37-key")
}

pub fn load_api_key(data_dir: Option<&Path>) -> Option<String> {
    if let Ok(env_key) = std::env::var("AGENT37_API_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Some(dir) = data_dir {
        if let Ok(text) = std::fs::read_to_string(api_key_path(dir)) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    // Fall back to `~/.config/agent37/config.json` written by `agent37 ssh setup`.
    let cli_cfg = dirs::home_dir()?.join(".config").join("agent37").join("config.json");
    let bytes = std::fs::read(cli_cfg).ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    for field in ["apiKey", "api_key", "key"] {
        if let Some(k) = json[field].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            return Some(k.to_owned());
        }
    }
    None
}

fn parse_instance(v: Value) -> Result<Instance> {
    let inner = v.get("instance").cloned().unwrap_or(v);
    serde_json::from_value(inner).context("parsing Agent37 instance JSON")
}

fn parse_billing(v: &Value) -> BillingInfo {
    let balance_dollars = v
        .get("balance_usd")
        .or_else(|| v.get("balanceUsd"))
        .or_else(|| v.get("balance_dollars"))
        .or_else(|| v.get("balanceDollars"))
        .or_else(|| v.get("balance"))
        .and_then(Value::as_f64)
        .or_else(|| {
            v.get("balance_cents")
                .or_else(|| v.get("balanceCents"))
                .and_then(Value::as_f64)
                .map(|c| c / 100.0)
        })
        .unwrap_or(0.0);
    let memory_limit_gb = v
        .get("memory_limit_gb")
        .or_else(|| v.get("memoryLimitGb"))
        .or_else(|| v.get("memory_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(32) as u32;
    let tier = v.get("tier").and_then(|t| {
        t.as_str()
            .map(str::to_owned)
            .or_else(|| t.as_u64().map(|n| format!("Tier {n}")))
    });
    let auto_top_up = v
        .get("auto_top_up")
        .or_else(|| v.get("autoTopUp"))
        .and_then(|a| a.as_bool().or_else(|| a.as_object().map(|_| true)));
    BillingInfo { balance_dollars, memory_limit_gb, tier, auto_top_up }
}

fn should_retry_try_again(status: StatusCode, body: &str) -> bool {
    matches!(status, StatusCode::CONFLICT | StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY)
        && is_try_again(body)
}

fn is_try_again(body: &str) -> bool {
    serde_json::from_str::<Value>(body).ok().is_some_and(|v| {
        v["error"].as_str() == Some("try_again") || v["error"]["code"].as_str() == Some("try_again")
    })
}

/// Handles both Agent37's flat sandbox transport errors (`{"error": "not_found"}`) and structured
/// API errors (`{"error": {"code": "...", "message": "..."}}`).
fn extract_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(s) = v["error"].as_str() {
            return s.to_owned();
        }
        if let Some(msg) = v["error"]["message"].as_str() {
            let code = v["error"]["code"].as_str();
            return match code {
                Some(c) => format!("{c}: {msg}"),
                None => msg.to_owned(),
            };
        }
        if let Some(c) = v["error"]["code"].as_str() {
            return c.to_owned();
        }
        if let Some(msg) = v["message"].as_str() {
            return msg.to_owned();
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() { "empty error response".into() } else { trimmed.to_owned() }
}

fn deserialize_opt_timestamp<'de, D>(deserializer: D) -> std::result::Result<Option<chrono::DateTime<chrono::Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = Option::<Value>::deserialize(deserializer)?;
    let Some(v) = val else { return Ok(None) };
    if let Some(secs) = v.as_i64() {
        // Distinguish unix seconds vs unix milliseconds.
        let dt = if secs > 10_000_000_000 {
            chrono::DateTime::from_timestamp_millis(secs)
        } else {
            chrono::DateTime::from_timestamp(secs, 0)
        };
        return Ok(dt);
    }
    if let Some(s) = v.as_str() {
        return Ok(chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc)));
    }
    Ok(None)
}

const HEADER_RELAY_SECRET: &str = "x-t3agent37-relay";
const HEADER_ORIG_HOST: &str = "x-t3agent37-host";

/// Minimal in-sandbox HTTP loopback relay so Agent37's authenticated preview edge
/// (`https://{id}-{bridge_port}.agent37.app`, which connects over the container's non-loopback
/// interface) can reach OAuth servers bound to `127.0.0.1:<port>` or `[::1]:<port>`.
const OAUTH_RELAY_JS: &str = r#"const fs = require("fs");
const http = require("http");
const targetPort = Number(process.argv[2]);
const secretPath = process.argv[3];
const portPath = process.argv[4];
const secret = fs.readFileSync(secretPath, "utf8").trim();
try { fs.unlinkSync(secretPath); } catch {}
const HOP = new Set([
  "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
  "te", "trailer", "transfer-encoding", "upgrade",
  "x-agent37-key", "x-t3agent37-relay", "x-t3agent37-host",
  "x-forwarded-for", "x-forwarded-host", "x-forwarded-proto",
]);
function forward(req, res, body, hosts, idx) {
  const headers = {};
  for (const [k, v] of Object.entries(req.headers)) {
    const lk = k.toLowerCase();
    if (!HOP.has(lk) && lk !== "content-length") headers[k] = v;
  }
  headers["host"] = req.headers["x-t3agent37-host"] || ("127.0.0.1:" + targetPort);
  headers["connection"] = "close";
  if (body.length > 0) headers["content-length"] = String(body.length);
  const up = http.request(
    { host: hosts[idx], port: targetPort, method: req.method, path: req.url, headers },
    (upRes) => {
      const out = {};
      for (const [k, v] of Object.entries(upRes.headers)) {
        if (!HOP.has(k.toLowerCase())) out[k] = v;
      }
      res.writeHead(upRes.statusCode || 502, out);
      upRes.pipe(res);
    }
  );
  up.on("error", (err) => {
    if (idx + 1 < hosts.length && (err.code === "ECONNREFUSED" || err.code === "EADDRNOTAVAIL")) {
      forward(req, res, body, hosts, idx + 1);
      return;
    }
    if (!res.headersSent) {
      res.writeHead(502, { "content-type": "text/plain" });
      res.end("OAuth loopback target unreachable: " + err.message);
    }
  });
  up.end(body);
}
const srv = http.createServer((req, res) => {
  if (req.headers["x-t3agent37-relay"] !== secret) {
    res.writeHead(403, { "content-type": "text/plain" });
    res.end("forbidden");
    return;
  }
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  req.on("end", () => forward(req, res, Buffer.concat(chunks), ["127.0.0.1", "::1"], 0));
});
srv.listen(0, "0.0.0.0", () => {
  fs.writeFileSync(portPath, String(srv.address().port) + "\n");
});
setTimeout(() => process.exit(0), 660000).unref();
"#;

struct RelayCleanup {
    agent37: Agent37,
    instance_id: String,
    pid: u32,
    dir: String,
}

impl RelayCleanup {
    async fn run(self) {
        let cmd = format!("kill {} 2>/dev/null || true; rm -rf '{}'", self.pid, self.dir);
        let _ = self.agent37.exec(&self.instance_id, &cmd, None, 15).await;
    }
}

/// Active local-to-sandbox HTTP OAuth port forward. Aborts local listeners and removes the
/// sandbox loopback relay when stopped or dropped.
pub struct PortForward {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    cleanup: Option<RelayCleanup>,
}

impl PortForward {
    pub async fn stop(mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.run().await;
        }
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(cleanup) = self.cleanup.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(cleanup.run());
            }
        }
    }
}

struct OAuthForwardState {
    http: reqwest::Client,
    upstream_base: String,
    api_key: String,
    relay_secret: String,
    default_host: String,
}

fn parse_relay_start(stdout: &str) -> Result<(u16, u32, String)> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("RELAY "))
        .ok_or_else(|| anyhow!("unexpected OAuth relay output: {}", stdout.trim()))?;
    let mut parts = line.split_whitespace();
    let _tag = parts.next();
    let port: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&p| p > 0)
        .ok_or_else(|| anyhow!("invalid OAuth relay port in output: {line}"))?;
    let pid: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&p| p > 0)
        .ok_or_else(|| anyhow!("invalid OAuth relay pid in output: {line}"))?;
    let dir = parts.next().unwrap_or_default();
    let Some(name) = dir.strip_prefix("/tmp/") else {
        bail!("unexpected OAuth relay directory: {dir}");
    };
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains("..")
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("unexpected OAuth relay directory: {dir}");
    }
    Ok((port, pid, dir.to_owned()))
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

async fn proxy_oauth_request(
    axum::extract::State(state): axum::extract::State<Arc<OAuthForwardState>>,
    req: AxumRequest,
) -> AxumResponse {
    let (parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target_url = format!("{}{path_and_query}", state.upstream_base);
    let orig_host = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&state.default_host)
        .to_owned();

    let body_bytes = match axum::body::to_bytes(body, 2 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("reading callback request: {e}")).into_response(),
    };

    let Ok(method) = Method::from_bytes(parts.method.as_str().as_bytes()) else {
        return (StatusCode::METHOD_NOT_ALLOWED, "unsupported method").into_response();
    };
    let mut up = state.http.request(method, &target_url);
    for (name, value) in &parts.headers {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower)
            || matches!(
                lower.as_str(),
                "host" | "content-length" | "x-agent37-key" | "x-t3agent37-relay" | "x-t3agent37-host"
            )
        {
            continue;
        }
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(name.as_str().as_bytes()), HeaderValue::from_bytes(value.as_bytes())) {
            up = up.header(k, v);
        }
    }
    up = up
        .header(HEADER_API_KEY, &state.api_key)
        .header(HEADER_RELAY_SECRET, &state.relay_secret)
        .header(HEADER_ORIG_HOST, &orig_host);
    if !body_bytes.is_empty() {
        up = up.body(body_bytes);
    }

    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("OAuth callback relay failed: {e}")).into_response(),
    };

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let mut builder = axum::http::Response::builder().status(status);
    for (name, value) in resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) || lower == "content-length" {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let resp_bytes = resp.bytes().await.unwrap_or_default();
    builder
        .body(AxumBody::from(resp_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as AxumPath, State};
    use axum::http::HeaderMap as AxumHeaders;
    use axum::routing::{delete, get, patch, post};
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn pricing_matches_published_agent37_rates() {
        // Entry 2 vCPU / 4 GB / 4 GB disk = $4.76 per 730h month
        let entry = Resources { cpu: 2, memory: 4, disk: 4 };
        let monthly = total_hourly(InstanceType::Default, entry) * HOURS_PER_MONTH;
        assert!((monthly - 4.76).abs() < 1e-6, "got {monthly}");
        assert!((disk_monthly(4) - 0.36).abs() < 1e-6);
        assert!((disk_monthly(12) - 1.08).abs() < 1e-6);
        // VM is 2.5x compute, disk unchanged
        let vm_compute = compute_hourly(InstanceType::Vm, 2, 4) * HOURS_PER_MONTH;
        assert!((vm_compute - 11.0).abs() < 1e-6, "got {vm_compute}");
        // Performance is 4x compute
        let perf_compute = compute_hourly(InstanceType::Performance, 2, 4) * HOURS_PER_MONTH;
        assert!((perf_compute - 17.6).abs() < 1e-6, "got {perf_compute}");
    }

    #[test]
    fn shape_validation_enforces_vm_and_disk_bounds() {
        assert!(validate_shape_and_disk(InstanceType::Default, "small", Some(12)).is_ok());
        assert!(validate_shape_and_disk(InstanceType::Default, "small", Some(13)).is_err());
        assert!(validate_shape_and_disk(InstanceType::Default, "max", Some(40)).is_err());
        assert!(validate_shape_and_disk(InstanceType::Vm, "max", Some(40)).is_ok());
    }

    #[test]
    fn debug_redacts_api_key() {
        let client = Agent37::new("sk_live_super_secret_12345".into()).unwrap();
        let dbg = format!("{client:?}");
        assert!(!dbg.contains("sk_live_super_secret_12345"));
        assert!(dbg.contains("[REDACTED]"));
    }

    #[test]
    fn parses_live_agent37_billing_schema() {
        let live = json!({
            "balance_usd": 98.495844,
            "tier": 2,
            "memory_limit_gb": 200,
            "instance_limit": 50,
            "auto_top_up": null,
            "past_due": false
        });
        let b = parse_billing(&live);
        assert!((b.balance_dollars - 98.495844).abs() < 1e-6);
        assert_eq!(b.memory_limit_gb, 200);
        assert_eq!(b.tier.as_deref(), Some("Tier 2"));
        assert_eq!(b.auto_top_up, None);
    }

    #[test]
    fn parses_flat_and_nested_error_envelopes() {
        assert_eq!(extract_error_message(r#"{"error":"not_found"}"#), "not_found");
        assert_eq!(
            extract_error_message(r#"{"error":{"code":"try_again","message":"checkpoint writing"}}"#),
            "try_again: checkpoint writing"
        );
        assert!(is_try_again(r#"{"error":{"code":"try_again"}}"#));
        assert!(is_try_again(r#"{"error":"try_again"}"#));
    }

    #[tokio::test]
    async fn start_and_exec_retry_409_try_again_and_poll_202() {
        #[derive(Default)]
        struct MockState {
            start_calls: AtomicUsize,
            get_calls: AtomicUsize,
            exec_calls: AtomicUsize,
        }
        let state = Arc::new(MockState::default());

        let app = Router::new()
            .route(
                "/v1/billing",
                get(|headers: AxumHeaders| async move {
                    assert_eq!(headers.get("authorization").unwrap(), "Bearer sk_live_test");
                    Json(json!({ "balance_cents": 1250, "memory_limit_gb": 40, "tier": "starter" }))
                }),
            )
            .route(
                "/v1/instances/{id}/start",
                post(|State(s): State<Arc<MockState>>, AxumPath(id): AxumPath<String>| async move {
                    let n = s.start_calls.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        (StatusCode::CONFLICT, Json(json!({ "error": { "code": "try_again", "message": "checkpointing" } })))
                    } else {
                        (StatusCode::ACCEPTED, Json(json!({ "id": id, "status": "starting" })))
                    }
                }),
            )
            .route(
                "/v1/instances/{id}",
                get(|State(s): State<Arc<MockState>>, AxumPath(id): AxumPath<String>| async move {
                    let n = s.get_calls.fetch_add(1, Ordering::SeqCst);
                    let status = if n == 0 { "starting" } else { "running" };
                    Json(json!({ "id": id, "status": status, "slept_at": 1781222400 }))
                }),
            )
            .route(
                "/v1/instances/{id}/exec",
                post(|State(s): State<Arc<MockState>>, headers: AxumHeaders| async move {
                    assert_eq!(headers.get(HEADER_API_KEY).unwrap(), "sk_live_test");
                    let n = s.exec_calls.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (StatusCode::CONFLICT, Json(json!({ "error": "try_again" })))
                    } else if n == 1 {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({ "error": { "code": "try_again", "message": "not on this host" } })),
                        )
                    } else {
                        (StatusCode::OK, Json(json!({ "exit_code": 0, "stdout": "ok\n", "stderr": "" })))
                    }
                }),
            )
            .route("/v1/instances/{id}/sleep", post(|| async { Json(json!({ "status": "sleeping" })) }))
            .route("/v1/instances/{id}", patch(|| async { Json(json!({ "ok": true })) }))
            .route("/v1/instances/{id}", delete(|| async { StatusCode::NO_CONTENT }))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        let client = Agent37::with_endpoints(
            "sk_live_test".into(),
            base.clone(),
            base.clone(),
            format!("{base}/preview/{{id}}/{{port}}"),
        )
        .unwrap()
        .with_fast_retries();

        let billing = client.billing().await.unwrap();
        assert!((billing.balance_dollars - 12.50).abs() < 1e-6);
        assert_eq!(billing.memory_limit_gb, 40);

        let inst = client.start("inst_1").await.unwrap();
        assert_eq!(inst.status, "running");
        assert_eq!(state.start_calls.load(Ordering::SeqCst), 3);
        assert!(state.get_calls.load(Ordering::SeqCst) >= 2);

        let out = client.exec_ok("inst_1", "echo ok", 10).await.unwrap();
        assert_eq!(out, "ok\n");
        assert_eq!(state.exec_calls.load(Ordering::SeqCst), 3);

        client.sleep("inst_1").await.unwrap();
        client.patch_auto_sleep("inst_1", true, Some(3600)).await.unwrap();
        client.delete("inst_1").await.unwrap();
    }

    #[test]
    fn parse_relay_start_validates_fields() {
        assert_eq!(
            parse_relay_start("noise\nRELAY 49152 1234 /tmp/tmp.abc123\n").unwrap(),
            (49152, 1234, "/tmp/tmp.abc123".to_owned())
        );
        assert!(parse_relay_start("RELAY 0 1234 /tmp/ok").is_err());
        assert!(parse_relay_start("RELAY 49152 1234 /etc/passwd").is_err());
        assert!(parse_relay_start("RELAY 49152 1234 /tmp/a/../b").is_err());
    }

    #[tokio::test]
    async fn forward_proxies_oauth_callback_without_cli_and_preserves_redirects() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct MockState {
            relay_secret: Mutex<Option<String>>,
            cleanup_cmds: Mutex<Vec<String>>,
            seen_query: Mutex<Option<String>>,
            seen_host: Mutex<Option<String>>,
        }
        let state = Arc::new(MockState::default());

        let app = Router::new()
            .route(
                "/v1/instances/{id}/exec",
                post(|State(s): State<Arc<MockState>>, Json(body): Json<Value>| async move {
                    let cmd = body["command"].as_str().unwrap_or_default().to_owned();
                    if cmd.contains("relay.js") {
                        // Extract the hex relay secret written to "$d/secret"
                        if let Some((_, rest)) = cmd.split_once("> \"$d/relay.js\"; printf '%s' '") {
                            if let Some((sec, _)) = rest.split_once('\'') {
                                *s.relay_secret.lock().unwrap() = Some(sec.to_owned());
                            }
                        }
                        Json(json!({ "exit_code": 0, "stdout": "RELAY 54321 777 /tmp/tmp.relay1\n", "stderr": "" }))
                    } else {
                        s.cleanup_cmds.lock().unwrap().push(cmd);
                        Json(json!({ "exit_code": 0, "stdout": "", "stderr": "" }))
                    }
                }),
            )
            .route(
                "/preview/{id}/{port}/auth/callback",
                get(|State(s): State<Arc<MockState>>, AxumPath((id, port)): AxumPath<(String, u16)>, headers: AxumHeaders, req: AxumRequest| async move {
                    assert_eq!(id, "inst_oauth");
                    assert_eq!(port, 54321);
                    assert_eq!(headers.get(HEADER_API_KEY).unwrap(), "sk_live_test");
                    let expected_secret = s.relay_secret.lock().unwrap().clone().unwrap();
                    assert_eq!(headers.get(HEADER_RELAY_SECRET).unwrap(), expected_secret.as_str());
                    *s.seen_query.lock().unwrap() = req.uri().query().map(str::to_owned);
                    *s.seen_host.lock().unwrap() = headers.get(HEADER_ORIG_HOST).and_then(|v| v.to_str().ok()).map(str::to_owned);
                    (
                        StatusCode::FOUND,
                        [("location", "/success?done=1")],
                        "redirecting",
                    )
                }),
            )
            .with_state(state.clone());

        let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(mock_listener, app).await.unwrap() });
        let base = format!("http://{mock_addr}");

        let client = Agent37::with_endpoints(
            "sk_live_test".into(),
            base.clone(),
            base.clone(),
            format!("{base}/preview/{{id}}/{{port}}"),
        )
        .unwrap()
        .with_fast_retries();

        // Pick an available ephemeral port to simulate the OAuth callback port (e.g. 1455).
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_port = probe.local_addr().unwrap().port();
        drop(probe);

        let forward = client.forward("inst_oauth", local_port).await.unwrap();

        let no_redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = no_redirect
            .get(format!("http://127.0.0.1:{local_port}/auth/callback?code=xyz&state=s1"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers().get("location").unwrap(), "/success?done=1");
        assert_eq!(state.seen_query.lock().unwrap().as_deref(), Some("code=xyz&state=s1"));
        assert_eq!(
            state.seen_host.lock().unwrap().as_deref(),
            Some(format!("127.0.0.1:{local_port}").as_str())
        );

        forward.stop().await;

        let cleanups = state.cleanup_cmds.lock().unwrap();
        assert_eq!(cleanups.len(), 1);
        assert!(cleanups[0].contains("kill 777") && cleanups[0].contains("/tmp/tmp.relay1"));
    }
}
