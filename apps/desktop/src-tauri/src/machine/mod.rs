//! One remote Agent37 machine at runtime: its power state on Agent37, its read-only watchdog,
//! waking it on demand (`POST /start` with `409 try_again` backoff), capturing its final state,
//! and proxy-driven sleeping (`POST /sleep`).

mod capture;
mod connect;
pub mod github;
mod link;
pub mod projects;
pub mod provision;
pub mod signin;
mod work;

pub use connect::HandoffStatus;
pub(crate) use connect::deregister_for_delete;

use crate::agent37::{self, Agent37, Instance};
use crate::proxy::auth::Auth;
use crate::proxy::cache::{self, Cache};
use crate::proxy::classify;
use crate::proxy::threads;
use crate::proxy::upstream;
use crate::store::{Machine, Settings};
use crate::t3desktop::T3Desktop;
use crate::telemetry::{Outcome, Telemetry};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, watch};

const WATCHDOG_POLL: Duration = Duration::from_secs(5);
const API_POLL: Duration = Duration::from_secs(60);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(20);
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);
/// After a failed wake, requests fail fast instead of each retrying `POST /start`.
const WAKE_COOLDOWN: Duration = Duration::from_secs(30);
/// After a failed sleep, back off before retrying `POST /sleep` so outages don't hammer the API.
const SLEEP_COOLDOWN: Duration = Duration::from_secs(30);
/// The watchdog's limit too.
pub const MAX_KEEP_AWAKE_MINUTES: u32 = 24 * 60;
/// A machine woken for work due is up this much ahead of it.
const DUE_LEAD: chrono::TimeDelta = chrono::TimeDelta::minutes(2);
const DUE_WAKE_RETRY: Duration = Duration::from_secs(5 * 60);
/// A running machine's link is checked this often: links can be turned on, fail or near expiry
/// while it stays up.
const LINK_CHECK: Duration = Duration::from_secs(10 * 60);
/// If a woken instance doesn't answer on `:3773` within this window, kick `supervisor.mjs`
/// (handles cold-storage fresh boots on stock `agent37-base` containers).
const SUPERVISOR_KICK_AFTER: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Power {
    Unknown,
    Starting,
    Running,
    Stopping,
    Stopped,
    /// The instance is gone on Agent37, or the machine was deleted here. Final.
    Missing,
}

impl Power {
    fn from_instance(inst: &Instance) -> Self {
        if inst.is_running() {
            Power::Running
        } else if inst.is_asleep_or_stopped() {
            Power::Stopped
        } else if inst.is_stopping() {
            Power::Stopping
        } else if inst.is_starting() {
            Power::Starting
        } else {
            Power::Unknown
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub sequence: u64,
    pub threads: HashMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchdogPhase {
    Active,
    Draining,
    /// Capture acked; the local proxy/daemon calls `POST /v1/instances/{id}/sleep`.
    #[serde(alias = "ready_to_sleep", alias = "stopping")]
    ReadyToSleep,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchdogStatus {
    pub phase: WatchdogPhase,
    pub busy: bool,
    /// Agent runs in flight.
    pub working_threads: u32,
    pub working_subagents: u32,
    /// Agent runs waiting on the user (an approval, a question): not busy, but a T3 restart
    /// would expire what they wait on.
    #[serde(default)]
    pub waiting_threads: u32,
    /// Busy only because of CPU use (a build or job outside an agent turn).
    pub cpu_busy: bool,
    /// T3 is not answering, so the watchdog keeps it up (at most 30 min) in case an agent works.
    pub agents_unknown: bool,
    /// Seconds left of a "keep awake"; 0 when off or until stopped.
    pub keep_awake_secs: u64,
    /// Kept awake until someone stops it: no auto sleep at all.
    pub keep_awake_forever: bool,
    pub idle_secs: u64,
    pub sleep_after_secs: u64,
    /// When T3 next starts work on its own, ms since epoch.
    #[serde(default)]
    pub next_due_at: Option<i64>,
    /// That is soon: the machine waits for it instead of sleeping.
    #[serde(default)]
    pub work_due: bool,
    #[serde(default)]
    pub ready_to_sleep: bool,
    #[serde(default)]
    pub disk: Option<DiskUsage>,
    pub manifest: Option<Manifest>,
    /// When this app got the status: what its second counts are relative to.
    #[serde(skip, default = "SystemTime::now")]
    pub received: SystemTime,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Something visible changed.
    Changed,
    /// A machine record changed and must be saved.
    Persist,
    Notify { title: String, body: String },
    /// One-time device sign-in code (e.g. GitHub `gh auth login --web`), shown inline in UI.
    SignInCode(Option<String>),
    /// Active browser OAuth sign-in URL (e.g. Codex / Claude), shown inline in UI as a clickable fallback.
    SignInUrl(Option<String>),
}

/// What every machine shares: the Agent37 API client, local T3 desktop CLI, settings, and events.
pub struct Shared {
    pub agent37: Agent37,
    pub t3: T3Desktop,
    pub settings: RwLock<Settings>,
    pub events: mpsc::UnboundedSender<Event>,
    pub http: reqwest::Client,
    pub telemetry: Option<Arc<Telemetry>>,
    /// Cancels a prior in-flight browser OAuth sign-in so a retry immediately frees the local port.
    pub sign_in_cancel: Mutex<Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>>,
}

impl Shared {
    pub fn changed(&self) {
        let _ = self.events.send(Event::Changed);
    }
    /// Saved by the event loop, the state file's one writer.
    pub fn persist(&self) {
        let _ = self.events.send(Event::Persist);
    }
    pub fn notify(&self, title: impl Into<String>, body: impl Into<String>) {
        let _ = self.events.send(Event::Notify { title: title.into(), body: body.into() });
    }
    pub fn set_sign_in_code(&self, code: Option<String>) {
        let _ = self.events.send(Event::SignInCode(code));
    }
    pub fn set_sign_in_url(&self, url: Option<String>) {
        let _ = self.events.send(Event::SignInUrl(url));
    }
}

pub struct MachineRuntime {
    pub shared: Arc<Shared>,
    record: RwLock<Machine>,
    dir: PathBuf,
    pub cache: Mutex<Cache>,
    pub auth: Auth,
    power: watch::Sender<Power>,
    stopped_at: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    watchdog: Mutex<Option<WatchdogStatus>>,
    last_disk: Mutex<Option<DiskUsage>>,
    /// One wake/resize/delete transition at a time.
    pub(crate) waking: tokio::sync::Mutex<()>,
    /// One capture + proxy-driven sleep transition at a time.
    capturing: tokio::sync::Mutex<()>,
    last_heartbeat: Mutex<Option<Instant>>,
    /// When proxy-driven sleep was initiated; cleared once Agent37 reports asleep/stopped or active.
    stop_seen: Mutex<Option<Instant>>,
    /// The last failed wake: its message and when.
    wake_failure: Mutex<Option<(String, Instant)>>,
    /// When the last `agent37.sleep()` failed, used to back off retries on API outages.
    sleep_failure: Mutex<Option<Instant>>,
    /// Its T3 projects as last read, and those t3agent37 is adding.
    projects: Mutex<Vec<projects::MachineProject>>,
    adding: Mutex<Vec<projects::MachineProject>>,
    handoff: Mutex<HandoffStatus>,
    connect_operation: tokio::sync::Mutex<()>,
    pub(crate) peer_link_operation: tokio::sync::Mutex<()>,
}

impl MachineRuntime {
    pub fn new(shared: Arc<Shared>, record: Machine, dir: PathBuf) -> Self {
        let cache = Cache::load(&cache::cache_path(&dir));
        let shell = cache.capture.as_ref().map(|c| c.shell.clone());
        let this = Self {
            shared,
            record: RwLock::new(record),
            dir,
            cache: Mutex::new(cache),
            auth: Auth::default(),
            power: watch::channel(Power::Unknown).0,
            stopped_at: Mutex::new(None),
            watchdog: Mutex::new(None),
            last_disk: Mutex::new(None),
            waking: tokio::sync::Mutex::new(()),
            capturing: tokio::sync::Mutex::new(()),
            last_heartbeat: Mutex::new(None),
            stop_seen: Mutex::new(None),
            wake_failure: Mutex::new(None),
            sleep_failure: Mutex::new(None),
            projects: Mutex::new(Vec::new()),
            adding: Mutex::new(Vec::new()),
            handoff: Mutex::new(HandoffStatus::default()),
            connect_operation: tokio::sync::Mutex::new(()),
            peer_link_operation: tokio::sync::Mutex::new(()),
        };
        if let Some(shell) = shell {
            this.set_projects_from(&shell);
        }
        this
    }

    pub fn record(&self) -> Machine {
        self.record.read().unwrap().clone()
    }

    pub fn update_record(&self, f: impl FnOnce(&mut Machine)) {
        f(&mut self.record.write().unwrap());
        self.shared.persist();
        self.shared.changed();
    }

    pub fn id(&self) -> String {
        self.record.read().unwrap().id.clone()
    }

    /// The Agent37 instance id.
    pub fn sandbox_id(&self) -> String {
        self.record.read().unwrap().sandbox_id.clone()
    }

    /// Where this machine's cache, chats and assets are kept on this computer.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Agent37 edge authentication header (`X-Agent37-Key`) injected on every upstream request.
    pub fn edge_headers(&self) -> HeaderMap {
        self.shared.agent37.edge_headers().unwrap_or_default()
    }

    pub fn power(&self) -> Power {
        *self.power.borrow()
    }

    pub fn subscribe_power(&self) -> watch::Receiver<Power> {
        self.power.subscribe()
    }

    pub fn disk_usage(&self) -> Option<DiskUsage> {
        self.last_disk.lock().unwrap().clone()
    }

    /// Deleted by this app: nothing may wake it or write to its dir again.
    pub fn retire(&self) {
        let _cache = self.cache.lock().unwrap();
        self.set_power(Power::Missing);
    }

    fn set_power(&self, p: Power) {
        let changed = self.power.send_if_modified(|cur| {
            if *cur == Power::Missing || *cur == p {
                return false;
            }
            *cur = p;
            true
        });
        if changed {
            if p != Power::Running {
                *self.watchdog.lock().unwrap() = None;
                let mut handoff = self.handoff.lock().unwrap();
                handoff.reachable = None;
                handoff.check_after = Some(chrono::Utc::now());
            }
            self.shared.changed();
        }
    }

    pub fn watchdog_status(&self) -> Option<WatchdogStatus> {
        self.watchdog.lock().unwrap().clone()
    }

    const STOP_SETTLE: Duration = Duration::from_secs(120);

    pub fn last_error(&self) -> Option<String> {
        self.wake_failure.lock().unwrap().as_ref().map(|(e, _)| e.clone())
    }

    pub fn is_stopping(&self) -> bool {
        matches!(self.power(), Power::Stopping | Power::Stopped) || self.stop_pending()
    }

    fn stop_pending(&self) -> bool {
        self.stop_seen.lock().unwrap().is_some_and(|t| t.elapsed() < Self::STOP_SETTLE)
    }

    pub fn save_cache(&self) {
        let result = {
            let cache = self.cache.lock().unwrap();
            if self.power() == Power::Missing {
                return;
            }
            cache.to_bytes().and_then(|b| crate::store::write_private(&cache::cache_path(&self.dir), &b))
        };
        if let Err(e) = result {
            tracing::warn!("saving cache for {}: {e:#}", self.id());
        }
    }

    pub async fn refresh_power(&self) -> Result<Power> {
        let id = self.sandbox_id();
        match self.shared.agent37.get(&id).await {
            Ok(inst) => {
                if let Some(slept_at) = inst.slept_at {
                    *self.stopped_at.lock().unwrap() = Some(slept_at);
                } else if inst.is_asleep_or_stopped() && self.stopped_at.lock().unwrap().is_none() {
                    *self.stopped_at.lock().unwrap() = Some(chrono::Utc::now());
                }
                let mut p = Power::from_instance(&inst);
                if p == Power::Stopped {
                    *self.stop_seen.lock().unwrap() = None;
                } else if p == Power::Running && self.stop_pending() {
                    p = Power::Stopping;
                } else if p == Power::Running && self.waking.try_lock().is_err() {
                    return Ok(p);
                }
                self.set_power(p);
                Ok(p)
            }
            Err(e) => {
                let gone = self.shared.agent37.confirmed_gone(&id).await;
                if gone {
                    self.set_power(Power::Missing);
                    return Ok(Power::Missing);
                }
                Err(e)
            }
        }
    }

    pub async fn settled_power(&self) -> Power {
        let mut rx = self.subscribe_power();
        let settled = |p: &Power| matches!(p, Power::Running | Power::Stopped | Power::Missing);
        let _ = tokio::time::timeout(SETTLE_TIMEOUT, rx.wait_for(settled)).await;
        self.power()
    }

    pub fn upstream_url(&self) -> String {
        self.record.read().unwrap().upstream_url.trim_end_matches('/').to_owned()
    }

    pub fn upstream_token(&self) -> String {
        self.record.read().unwrap().upstream_token.clone()
    }

    /// Reads `/.well-known/t3/environment` through Agent37's authenticated edge URL (never `/health`,
    /// which Agent37 intercepts at the edge).
    pub async fn fetch_descriptor(&self) -> Option<Value> {
        let url = format!("{}/.well-known/t3/environment", self.upstream_url());
        let res = self
            .shared
            .http
            .get(url)
            .headers(self.edge_headers())
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        let v: Value = res.json().await.ok()?;
        if v["environmentId"].as_str() != Some(&self.record().environment_id) {
            return None;
        }
        self.cache.lock().unwrap().descriptor = Some(v.clone());
        Some(v)
    }

    /// Brings the machine up if needed and returns once its T3 server answers and will stay up.
    /// Concurrent callers share one wake.
    pub async fn ensure_awake(&self) -> Result<()> {
        let _guard = self.waking.lock().await;
        if self.power() == Power::Missing {
            bail!("the instance no longer exists on Agent37");
        }
        if let Some((e, at)) = self.wake_failure.lock().unwrap().clone() {
            if at.elapsed() < WAKE_COOLDOWN {
                return Err(anyhow!(e));
            }
        }
        if self.power() == Power::Running && !self.is_stopping() && self.fetch_descriptor().await.is_some() {
            if self
                .watchdog_status()
                .is_some_and(|w| matches!(w.phase, WatchdogPhase::Draining | WatchdogPhase::ReadyToSleep))
            {
                self.send_activity(false).await;
            }
            if !self.is_stopping() {
                if self.wake_failure.lock().unwrap().take().is_some() {
                    self.shared.changed();
                }
                self.flush_deferred_commands().await;
                return Ok(());
            }
        }
        let attempt = self.shared.telemetry.as_ref().and_then(|telemetry| telemetry.attempt());
        let result = self.wake_locked().await;
        if let Some(attempt) = attempt {
            attempt.wake(if result.is_ok() { Outcome::Succeeded } else { Outcome::Failed });
        }
        let was_failing = self.wake_failure.lock().unwrap().is_some();
        let failed = match &result {
            Ok(()) => {
                *self.wake_failure.lock().unwrap() = None;
                false
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if !was_failing {
                    self.shared.notify(format!("{} could not wake", self.record().name), msg.clone());
                }
                *self.wake_failure.lock().unwrap() = Some((msg, Instant::now()));
                true
            }
        };
        drop(_guard);
        if failed {
            let _ = self.refresh_power().await;
        }
        self.shared.changed();
        result
    }

    async fn wake_locked(&self) -> Result<()> {
        let id = self.id();
        let started = Instant::now();
        let fresh = self.fresh_capture().is_some();
        self.set_power(Power::Starting);
        let mut resumed = false;
        loop {
            match self.refresh_power().await? {
                Power::Running => break,
                Power::Stopped => {
                    tracing::info!("waking Agent37 instance {id}");
                    self.set_power(Power::Starting);
                    self.shared.agent37.start(&self.sandbox_id()).await?;
                    resumed = true;
                    break;
                }
                Power::Missing => bail!("the instance no longer exists on Agent37"),
                _ => {}
            }
            if started.elapsed() > READY_TIMEOUT {
                bail!("the instance is stuck between states on Agent37");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if !fresh {
            self.cache.lock().unwrap().bump_epoch();
        }
        self.drop_capture();
        self.set_power(Power::Starting);
        // Fresh readiness budget after `start()` returns (a `202` host move may take >180s).
        self.wait_ready_with_supervisor_fallback(Instant::now()).await?;
        if resumed {
            if self.watchdog_get("/t3boat-status?fresh").await.is_ok_and(|s| s.busy || s.waiting_threads > 0) {
                tracing::info!("{id} is busy: upgrade deferred to the next wake");
            } else if let Err(e) = self.upgrade_if_needed().await {
                tracing::warn!("upgrading {id}: {e:#}");
            }
        }
        self.on_ready().await;
        self.set_power(Power::Running);
        tracing::info!("{id} awake after {:?}", started.elapsed());
        self.send_activity(resumed).await;
        Ok(())
    }

    async fn on_ready(&self) {
        self.flush_deferred_commands().await;
        if let Err(e) = self.prime().await {
            tracing::warn!("refreshing {}'s session: {e:#}", self.id());
        }
    }

    /// Fills what the proxy must answer before the machine can sleep: descriptor and session.
    pub async fn prime(&self) -> Result<()> {
        self.fetch_descriptor().await.ok_or_else(|| anyhow!("T3 on the machine did not answer"))?;
        let session = self
            .shared
            .http
            .get(format!("{}/api/auth/session", self.upstream_url()))
            .headers(self.edge_headers())
            .bearer_auth(self.upstream_token())
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json::<Map<String, Value>>()
            .await
            .context("T3 on the machine sent a malformed auth session")?;
        self.cache.lock().unwrap().auth_session = Some(session);
        self.save_cache();
        Ok(())
    }

    /// Waits for `t3 serve` to answer. If the wake was a cold-storage fresh boot on a stock
    /// `agent37-base` image, kicks `supervisor.mjs` via `exec` once if `:3773` hasn't come up yet.
    async fn wait_ready_with_supervisor_fallback(&self, started: Instant) -> Result<()> {
        let mut kicked = false;
        let wait_start = Instant::now();
        while self.fetch_descriptor().await.is_none() {
            if !kicked && wait_start.elapsed() >= SUPERVISOR_KICK_AFTER {
                kicked = true;
                tracing::info!("kicking supervisor on {} after fresh boot", self.id());
                let _ = provision::ensure_supervisor_running(&self.shared.agent37, &self.sandbox_id()).await;
            }
            if started.elapsed() > READY_TIMEOUT {
                bail!("T3 on the machine did not answer");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Ok(())
    }

    /// Keeps the machine on the desktop app's T3 version and on this app's scripts.
    pub async fn upgrade_if_needed(&self) -> Result<()> {
        let desktop = self.shared.t3.version().await?;
        let revision = provision::revision();
        let record = self.record();
        let new_server = desktop != record.t3_version;
        if !new_server && revision == record.provision_revision {
            return Ok(());
        }
        tracing::info!("upgrading {} (T3 {desktop}, scripts {revision})", self.id());
        provision::run(
            &self.shared.agent37,
            &record.sandbox_id,
            &record.name,
            &desktop,
            &record.watchdog_secret,
            false,
        )
        .await?;
        self.update_record(|m| {
            m.t3_version = desktop;
            m.provision_revision = revision;
        });
        {
            let mut cache = self.cache.lock().unwrap();
            cache.bump_epoch();
            if new_server {
                cache.streams.clear();
            }
        }
        if new_server {
            threads::sweep(&self.dir, &HashSet::new());
            self.save_cache();
        }
        self.wait_ready_with_supervisor_fallback(Instant::now()).await
    }

    async fn flush_deferred_commands(&self) {
        let commands = self.cache.lock().unwrap().deferred_commands.clone();
        if commands.is_empty() {
            return;
        }
        let requests = commands
            .iter()
            .map(|c| ("orchestration.dispatchCommand".to_owned(), c.clone()))
            .collect();
        match upstream::call_many_with_headers(
            &self.upstream_url(),
            &self.upstream_token(),
            &self.edge_headers(),
            requests,
        )
        .await
        {
            Ok(results) => {
                self.cache.lock().unwrap().deferred_commands.retain(|c| !commands.contains(c));
                for (command, result) in commands.iter().zip(results) {
                    if let Err(e) = result
                        && command["type"] == "message.dispatch"
                    {
                        tracing::warn!("message to {} refused: {e}", self.id());
                        self.shared.notify(format!("{}: a message was not sent", self.record().name), e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("sending deferred commands: {e:#}");
                return;
            }
        }
        self.save_cache();
    }

    /// Someone used the machine: it stays awake, and recorded answers may be outdated.
    pub fn user_action(self: &Arc<Self>) {
        self.cache.lock().unwrap().bump_epoch();
        if self.power() != Power::Running {
            return;
        }
        {
            let mut last = self.last_heartbeat.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < HEARTBEAT_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let this = self.clone();
        tokio::spawn(async move { this.send_activity(false).await });
    }

    async fn send_activity(&self, resumed: bool) {
        *self.last_heartbeat.lock().unwrap() = Some(Instant::now());
        if let Ok(s) = self.watchdog_post("/activity", json!({ "resumed": resumed })).await {
            self.record_watchdog_sample(&s);
            *self.watchdog.lock().unwrap() = Some(s);
            self.shared.changed();
        }
    }

    async fn watchdog_get(&self, path: &str) -> Result<WatchdogStatus> {
        self.watchdog_request(reqwest::Method::GET, path, None).await
    }

    async fn watchdog_post(&self, path: &str, body: Value) -> Result<WatchdogStatus> {
        self.watchdog_request(reqwest::Method::POST, path, Some(body)).await
    }

    async fn watchdog_request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<WatchdogStatus> {
        let (url, secret) = {
            let m = self.record.read().unwrap();
            (format!("{}{path}", m.watchdog_url.trim_end_matches('/')), m.watchdog_secret.clone())
        };
        let refreshes = matches!(path, "/t3boat-status?fresh" | "/status?fresh" | "/drain" | "/sleep-ack");
        let timeout = Duration::from_secs(if refreshes { 15 } else { 4 });
        let mut req = self
            .shared
            .http
            .request(method, url)
            .headers(self.edge_headers())
            .bearer_auth(secret)
            .timeout(timeout);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await?;
        let status = res.status();
        if status.as_u16() == 409 {
            let value: Value = res.json().await.unwrap_or_default();
            bail!("{}", value["error"].as_str().unwrap_or("not now"));
        }
        if !status.is_success() {
            bail!("watchdog {path}: HTTP {status}");
        }
        Ok(res.json().await?)
    }

    /// "Sleep now": asks the read-only watchdog to drain and publish its manifest; once captured
    /// and confirmed, the proxy calls `POST /v1/instances/{id}/sleep` using its own `sk_live_` key.
    pub async fn sleep_now(&self) -> Result<()> {
        if !self.awake_now().await? {
            return Ok(());
        }
        // Explicit user "Sleep now" overrides any background retry cooldown.
        *self.sleep_failure.lock().unwrap() = None;
        let attempt = self.shared.telemetry.as_ref().and_then(|telemetry| telemetry.attempt());
        let s = match self.watchdog_post("/drain", json!({})).await {
            Ok(status) => status,
            Err(error) => {
                if let Some(attempt) = attempt {
                    attempt.sleep(Outcome::WatchdogFailed);
                }
                return Err(error);
            }
        };
        *self.last_heartbeat.lock().unwrap() = None;
        self.on_watchdog_status(s).await;
        Ok(())
    }

    /// Keeps the machine up for `minutes` (`None`: until stopped; `Some(0)`: re-enable auto-sleep).
    pub async fn keep_awake(&self, minutes: Option<u32>) -> Result<()> {
        if minutes.is_some_and(|m| m > MAX_KEEP_AWAKE_MINUTES) {
            bail!("keep awake takes at most {MAX_KEEP_AWAKE_MINUTES} minutes, or until stopped");
        }
        if minutes == Some(0) && !self.awake_now().await? {
            return Ok(());
        }
        self.ensure_awake().await?;
        let body = json!({ "minutes": minutes });
        let s = self.watchdog_post("/keep-awake", body).await?;
        self.on_watchdog_status(s).await;
        Ok(())
    }

    /// Resizes CPU/RAM (`size_id`) and/or grows disk (`disk_gb`) via Agent37's
    /// `POST /v1/instances/{id}/resize`.
    ///
    /// Agent37 requires the instance to be **running** to resize, and recreates the container
    /// (losing in-memory state while keeping disk, ID, and URLs). Refuses mid-turn or while an
    /// agent waits on a user approval.
    pub async fn resize(&self, size_id: Option<&str>, disk_gb: Option<u32>) -> Result<()> {
        // Fast pre-check before waking (authoritative read & validation happen under `waking`).
        {
            let current = self.record();
            let target_size = agent37::shape(size_id.unwrap_or(&current.size))?.id;
            let current_size = agent37::shape(&current.size).map(|s| s.id).unwrap_or(current.size.as_str());
            let target_disk = disk_gb.unwrap_or(current.disk_gb);
            if target_disk < current.disk_gb {
                bail!("Agent37 disks can only grow, never shrink (current: {} GB)", current.disk_gb);
            }
            agent37::validate_shape_and_disk(current.instance_type, target_size, Some(target_disk))?;
            if target_size == current_size && target_disk == current.disk_gb {
                return Ok(());
            }
        }

        // Agent37 returns 400 on resize while asleep/stopped: wake first.
        self.ensure_awake().await?;
        let outcome = {
            let _guard = self.waking.lock().await;
            if self.power() != Power::Running || self.is_stopping() {
                bail!("machine stopped before resize could start; try again");
            }
            // Authoritative record snapshot under `waking` so concurrent resizes never clobber
            // each other's shape or disk size.
            let current = self.record();
            let target_size = agent37::shape(size_id.unwrap_or(&current.size))?.id;
            let current_size = agent37::shape(&current.size).map(|s| s.id).unwrap_or(current.size.as_str());
            let target_disk = disk_gb.unwrap_or(current.disk_gb);
            if target_disk < current.disk_gb {
                bail!("Agent37 disks can only grow, never shrink (current: {} GB)", current.disk_gb);
            }
            let resources = agent37::validate_shape_and_disk(current.instance_type, target_size, Some(target_disk))?;
            let size_changed = target_size != current_size;
            let disk_changed = target_disk != current.disk_gb;
            if !size_changed && !disk_changed {
                return Ok(());
            }

            let s = self.watchdog_get("/t3boat-status?fresh").await?;
            if s.busy {
                bail!("an agent or build is working; wait for it to finish before resizing");
            }
            if s.waiting_threads > 0 {
                bail!("an agent is waiting on you; answer it before resizing (resizing restarts the container)");
            }

            self.set_power(Power::Starting);
            let (cpu, mem) = if size_changed { (Some(resources.cpu), Some(resources.memory)) } else { (None, None) };
            let disk = disk_changed.then_some(resources.disk);
            let step: Result<()> = async {
                let updated = self.shared.agent37.resize(&current.sandbox_id, cpu, mem, disk).await?;
                let confirmed_disk = updated
                    .resources
                    .filter(|r| r.disk > 0)
                    .map(|r| r.disk)
                    .unwrap_or(resources.disk);
                self.update_record(|m| {
                    if size_changed {
                        m.size = target_size.to_owned();
                    }
                    if disk_changed || confirmed_disk > m.disk_gb {
                        m.disk_gb = confirmed_disk;
                    }
                });
                self.cache.lock().unwrap().bump_epoch();
                self.drop_capture();
                self.wait_ready_with_supervisor_fallback(Instant::now()).await?;
                self.on_ready().await;
                self.set_power(Power::Running);
                self.send_activity(true).await;
                Ok(())
            }
            .await;
            step
        };
        if outcome.is_err() {
            let _ = self.refresh_power().await;
        }
        outcome
    }

    /// Updates Agent37's platform-side `auto_sleep` safety fuse without restarting the instance.
    pub async fn sync_auto_sleep_fuse(&self, fuse_minutes: Option<u32>) -> Result<()> {
        let on = fuse_minutes.is_some();
        let secs = fuse_minutes.map(|m| m.saturating_mul(60));
        self.shared.agent37.patch_auto_sleep(&self.sandbox_id(), on, secs).await
    }

    async fn awake_now(&self) -> Result<bool> {
        match self.settled_power().await {
            Power::Running => Ok(true),
            Power::Stopped | Power::Missing => Ok(false),
            _ => bail!("still waking; try again in a moment"),
        }
    }

    fn record_watchdog_sample(&self, s: &WatchdogStatus) {
        if let Some(disk) = &s.disk {
            *self.last_disk.lock().unwrap() = Some(disk.clone());
        }
    }

    async fn on_watchdog_status(&self, s: WatchdogStatus) {
        self.record_watchdog_sample(&s);
        if s.busy {
            self.cache.lock().unwrap().bump_epoch();
        }
        if s.phase == WatchdogPhase::Active {
            self.drop_capture();
            // Hold `waking` while checking power and clearing `stop_seen` so a concurrent
            // `drive_proxy_sleep` cannot acquire `waking` and set `stop_seen` in between.
            if let Ok(_waking) = self.waking.try_lock()
                && self.power() == Power::Running
            {
                *self.stop_seen.lock().unwrap() = None;
            }
        }
        if self.record.read().unwrap().next_due_at != s.next_due_at {
            self.update_record(|r| r.next_due_at = s.next_due_at);
        }
        let manifest = s.manifest.clone();
        let phase = s.phase;
        *self.watchdog.lock().unwrap() = Some(s);
        self.shared.changed();

        if matches!(phase, WatchdogPhase::Draining | WatchdogPhase::ReadyToSleep) {
            self.drive_proxy_sleep(manifest).await;
        }
    }

    /// Proxy-driven sleep: captures shell/threads/assets at the watchdog's manifest, acquires
    /// `self.waking` (in fixed order `capturing` -> `waking`) so `ensure_awake`/`resize` cannot
    /// interleave, re-confirms `/sleep-ack` (even when `manifest` is `None`), and calls Agent37
    /// `POST /sleep`.
    async fn drive_proxy_sleep(&self, manifest: Option<Manifest>) {
        if self.sleep_failure.lock().unwrap().is_some_and(|at| at.elapsed() < SLEEP_COOLDOWN) {
            return;
        }
        let _capturing = self.capturing.lock().await;
        if self.power() != Power::Running || self.is_stopping() {
            return;
        }
        let attempt = self.shared.telemetry.as_ref().and_then(|telemetry| telemetry.attempt());
        if let Some(ref m) = manifest {
            if self.captured_sequence() != Some(m.sequence) {
                self.drop_capture();
                if let Err(e) = self.capture(m).await {
                    if let Some(attempt) = attempt {
                        attempt.sleep(Outcome::CaptureFailed);
                    }
                    tracing::warn!("capture failed for {}: {e:#}", self.id());
                    return;
                }
            }
        }

        // Serialize final ack + Agent37 sleep with `ensure_awake` / `resize` / `delete`.
        let _waking = self.waking.lock().await;
        if self.power() != Power::Running || self.is_stopping() {
            return;
        }
        let seq = manifest.as_ref().map(|m| m.sequence);
        match self.watchdog_post("/sleep-ack", json!({ "sequence": seq })).await {
            Ok(ack) if ack.phase == WatchdogPhase::Active => {
                // Activity or new events arrived before ack: abort sleep cleanly.
                self.drop_capture();
                *self.watchdog.lock().unwrap() = Some(ack);
                self.shared.changed();
                return;
            }
            Ok(ack)
                if (ack.ready_to_sleep || ack.phase == WatchdogPhase::ReadyToSleep)
                    && ack.manifest.as_ref().map(|m| m.sequence) == seq =>
            {
                if let Some(s) = seq {
                    self.confirm_capture(s);
                }
                *self.watchdog.lock().unwrap() = Some(ack);
            }
            Ok(ack) => {
                // Watchdog is not ready or its manifest moved ahead of what we captured: abort
                // this sleep attempt so the next poll recaptures the newer sequence.
                *self.watchdog.lock().unwrap() = Some(ack);
                return;
            }
            Err(e) => {
                if let Some(attempt) = attempt {
                    attempt.sleep(Outcome::WatchdogFailed);
                }
                tracing::warn!("sleep-ack failed for {}: {e:#}", self.id());
                return;
            }
        }

        // Now ask Agent37 to CRIU-checkpoint the instance (compute billing drops to $0).
        // Keep `stop_seen` set until `refresh_power()` sees Agent37 report `Stopped` so a
        // stale `"running"` poll during checkpoint write cannot flip power back to Running.
        self.mark_stopping();
        let id = self.sandbox_id();
        tracing::info!("putting Agent37 instance {} to sleep (checkpoint)", self.id());
        let result = self.shared.agent37.sleep(&id).await;
        if let Some(attempt) = attempt {
            attempt.sleep(if result.is_ok() { Outcome::Succeeded } else { Outcome::Failed });
        }
        match result {
            Ok(()) => {
                *self.sleep_failure.lock().unwrap() = None;
                *self.stopped_at.lock().unwrap() = Some(chrono::Utc::now());
                self.set_power(Power::Stopped);
            }
            Err(e) => {
                tracing::warn!("Agent37 sleep failed for {}: {e:#}", self.id());
                *self.sleep_failure.lock().unwrap() = Some(Instant::now());
                *self.stop_seen.lock().unwrap() = None;
                drop(_waking);
                let _ = self.refresh_power().await;
            }
        }
    }

    fn mark_stopping(&self) {
        self.stop_seen.lock().unwrap().get_or_insert_with(Instant::now);
        self.set_power(Power::Stopping);
    }

    /// Background loop: follows the machine's power state on Agent37 and polls its read-only watchdog.
    pub async fn run(self: Arc<Self>) {
        let unprimed = {
            let c = self.cache.lock().unwrap();
            c.descriptor.is_none() || c.auth_session.is_none()
        };
        if unprimed {
            let primed = async {
                self.ensure_awake().await?;
                self.prime().await
            };
            if let Err(e) = primed.await {
                tracing::warn!("priming {}: {e:#}", self.id());
            }
        }
        let mut last_api_poll: Option<Instant> = None;
        let mut last_due_wake: Option<Instant> = None;
        let mut last_link_check: Option<Instant> = None;
        let mut last_connect_check: Option<Instant> = None;
        let mut power_rx = self.subscribe_power();
        let mut watchdog_failures = 0u32;
        let mut was_busy = false;
        let mut was_running = false;
        loop {
            let transitioning = matches!(self.power(), Power::Stopping | Power::Starting | Power::Unknown);
            if transitioning || last_api_poll.is_none_or(|t| t.elapsed() >= API_POLL) || watchdog_failures >= 2 {
                last_api_poll = Some(Instant::now());
                watchdog_failures = 0;
                if let Err(e) = self.refresh_power().await {
                    tracing::warn!("agent37 info {}: {e:#}", self.id());
                }
            }
            if self.power() == Power::Missing {
                return;
            }
            let due = self.scheduled_wake().is_some_and(|due| chrono::Utc::now() >= due - DUE_LEAD);
            if (due || self.has_queued_wake()) && last_due_wake.is_none_or(|t| t.elapsed() >= DUE_WAKE_RETRY) {
                last_due_wake = Some(Instant::now());
                tracing::info!("waking {} for work due", self.id());
                if let Err(e) = self.ensure_awake().await {
                    tracing::warn!("waking {} for work due: {e:#}", self.id());
                }
            }
            let running = self.power() == Power::Running;
            let came_up = running && !was_running;
            was_running = running;
            if running {
                if came_up || last_connect_check.is_none_or(|t| t.elapsed() >= API_POLL) {
                    last_connect_check = Some(Instant::now());
                    let this = self.clone();
                    tokio::spawn(async move { this.refresh_connect().await });
                }
                if came_up || last_link_check.is_none_or(|t| t.elapsed() >= LINK_CHECK) {
                    last_link_check = Some(Instant::now());
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = this.ensure_linked().await {
                            tracing::warn!("linking {} into T3 Code: {e:#}", this.id());
                        }
                        if came_up {
                            this.refresh_projects().await;
                            this.refresh_github().await;
                        }
                    });
                }
                self.flush_deferred_commands().await;
                match self.watchdog_get("/t3boat-status").await {
                    Ok(s) => {
                        watchdog_failures = 0;
                        if was_busy && !s.busy {
                            self.cache.lock().unwrap().bump_epoch();
                            let this = self.clone();
                            tokio::spawn(async move { this.refresh_projects().await });
                        }
                        was_busy = s.busy;
                        if !s.agents_unknown && self.wake_failure.lock().unwrap().take().is_some() {
                            self.shared.changed();
                            let this = self.clone();
                            tokio::spawn(async move { this.on_ready().await });
                        }
                        self.on_watchdog_status(s).await;
                    }
                    Err(_) => watchdog_failures += 1,
                }
            }
            // Read & mark seen BEFORE computing `sleep_for` so a concurrent Stopped -> Running
            // change can never be swallowed before entering `select!`.
            let current_power = *power_rx.borrow_and_update();
            let sleep_for = if current_power == Power::Stopped {
                let until_due = self
                    .scheduled_wake()
                    .and_then(|d| ((d - DUE_LEAD) - chrono::Utc::now()).to_std().ok())
                    .unwrap_or(API_POLL);
                until_due.min(API_POLL).max(Duration::from_secs(1))
            } else {
                WATCHDOG_POLL
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep_for) => {}
                _ = power_rx.changed() => {}
            }
        }
    }

    pub fn scheduled_wake(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let due = chrono::DateTime::from_timestamp_millis(self.record.read().unwrap().next_due_at?)?;
        let stopped_at = (*self.stopped_at.lock().unwrap())?;
        (self.power() == Power::Stopped && due > stopped_at).then_some(due)
    }

    fn has_queued_wake(&self) -> bool {
        self.power() == Power::Stopped
            && self
                .cache
                .lock()
                .unwrap()
                .deferred_commands
                .iter()
                .any(|c| c["type"].as_str().is_some_and(|k| classify::WAKING_COMMANDS.contains(&k)))
    }
}
