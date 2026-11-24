//! The app's model: machines, settings, setup checks, billing/spend accrual, and orphan reaper.
//! UI-agnostic; the Tauri tray app and the headless `t3agent37-daemon` both drive it.

use crate::agent37::{self, Agent37, BillingInfo, InstanceType, Region};
use crate::machine::{provision, DiskUsage, Event, MachineRuntime, Power, Shared, WatchdogPhase};
use crate::proxy;
use crate::store::{Doomed, Settings, Spend, State, Store};
use crate::t3desktop::T3Desktop;
use crate::telemetry::{Milestone, Telemetry};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

const FIRST_PORT: u16 = 47731;
/// Machines ever created here: deleted ones keep their port.
const PORTS: u16 = 1000;
/// How often a delete Agent37 hasn't confirmed is tried again.
const REAP_RETRY: Duration = Duration::from_secs(60);
/// Pending this long since this launch first tried it: the user is told, and it is tried on.
const REAP_WARN: Duration = Duration::from_secs(15 * 60);

/// Why the app cannot run yet, in the order the user has to fix it.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Setup {
    #[default]
    Starting,
    Ready,
    ApiKey,
    InstallT3,
}

pub async fn check_setup(data_dir: Option<&Path>) -> (Setup, Option<Agent37>, Option<T3Desktop>) {
    let Some(agent37) = Agent37::locate(data_dir) else {
        return (Setup::ApiKey, None, None);
    };
    if agent37.billing().await.is_err() {
        return (Setup::ApiKey, None, None);
    }
    let Some(t3) = T3Desktop::locate() else {
        return (Setup::InstallT3, Some(agent37), None);
    };
    (Setup::Ready, Some(agent37), Some(t3))
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Working,
    Idle,
    Sleeping,
    Asleep,
    Waking,
    Creating,
    Error,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    pub id: String,
    pub name: String,
    pub size: String,
    pub instance_type: InstanceType,
    pub region: Region,
    pub disk_gb: u32,
    pub idle_monthly: f64,
    pub hourly_rate: f64,
    pub disk_usage: Option<DiskUsage>,
    pub disk_warning: bool,
    pub github_signed_in: bool,
    pub state: MachineState,
    pub detail: String,
    pub countdown: Option<Countdown>,
    pub paired: bool,
    pub signed_out: Vec<String>,
    pub projects: Vec<crate::machine::projects::MachineProject>,
    pub keep_awake_until: Option<u64>,
    pub keep_awake_forever: bool,
    pub waiting: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Countdown {
    pub template: &'static str,
    pub ends_at: u64,
}

impl Countdown {
    fn new(template: &'static str, secs: u64, from: SystemTime) -> Self {
        Self { template, ends_at: unix_ms_after(from, secs) }
    }

    fn text(&self, secs: u64) -> String {
        self.template.replace("{left}", &human_secs(secs))
    }
}

fn unix_ms_after(from: SystemTime, secs: u64) -> u64 {
    let from_ms = from.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    from_ms.saturating_add(secs.saturating_mul(1000))
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineUsage {
    pub today: f64,
    pub month: f64,
    pub awake_secs_today: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Costs {
    pub today: f64,
    pub month: f64,
    pub idle_monthly: f64,
    pub wallet_balance: f64,
    pub awake_ram_gb: u32,
    pub memory_limit_gb: u32,
}

struct Creating {
    name: String,
    size: String,
    instance_type: InstanceType,
    region: Region,
    disk_gb: u32,
    step: String,
    port: u16,
    sandbox: Option<String>,
}

impl Creating {
    fn doomed(&self) -> Option<Doomed> {
        let id = self.sandbox.as_ref()?;
        Some(Doomed { sandbox: Some(id.clone()) })
    }
}

struct Reaping {
    since: Instant,
    told: bool,
    deleted: bool,
}

struct Running {
    machine: Arc<MachineRuntime>,
    tasks: Vec<JoinHandle<()>>,
}

pub struct App {
    pub shared: Arc<Shared>,
    store: Store,
    data_dir: PathBuf,
    machines: RwLock<Vec<Running>>,
    creating: Mutex<Vec<Creating>>,
    billing: Mutex<Option<BillingInfo>>,
    spend: Mutex<HashMap<String, Spend>>,
    retired_ports: Mutex<Vec<u16>>,
    doomed: Mutex<Vec<Doomed>>,
    reaper: Notify,
}

impl App {
    pub fn new(
        agent37: Agent37,
        t3: T3Desktop,
        data_dir: PathBuf,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Arc<Self>> {
        Self::with_telemetry(agent37, t3, data_dir, events, None)
    }

    pub(crate) fn with_telemetry(
        agent37: Agent37,
        t3: T3Desktop,
        data_dir: PathBuf,
        events: mpsc::UnboundedSender<Event>,
        telemetry: Option<Arc<Telemetry>>,
    ) -> Result<Arc<Self>> {
        let store = Store::new(&data_dir);
        let state = store.load().context("reading t3agent37 state")?;
        let doomed: Vec<Doomed> = state
            .doomed
            .into_iter()
            .filter(|d| !state.machines.iter().any(|m| d.sandbox.as_ref() == Some(&m.sandbox_id)))
            .collect();
        let shared = Arc::new(Shared {
            agent37,
            t3: t3.with_token_file(data_dir.join("t3-token")),
            settings: RwLock::new(state.settings.clone()),
            events,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(4)
                .pool_idle_timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            sign_in_cancel: Mutex::new(None),
            telemetry,
        });
        let app = Arc::new(Self {
            shared,
            store,
            data_dir,
            machines: RwLock::new(Vec::new()),
            creating: Mutex::new(Vec::new()),
            billing: Mutex::new(None),
            spend: Mutex::new(state.spend),
            retired_ports: Mutex::new(state.retired_ports),
            doomed: Mutex::new(doomed),
            reaper: Notify::new(),
        });
        for record in state.machines {
            app.spawn(app.runtime(record));
        }
        tokio::spawn(app.clone().reap());
        Ok(app)
    }

    /// Deletes what was given up until Agent37's `GET /v1/instances` confirms each gone:
    /// an orphaned instance bills retained disk forever until deleted.
    async fn reap(self: Arc<Self>) {
        let mut reaping: HashMap<String, Reaping> = HashMap::new();
        loop {
            if self.reap_pass(&mut reaping).await {
                tokio::select! {
                    _ = self.reaper.notified() => {}
                    _ = tokio::time::sleep(REAP_RETRY) => {}
                }
            } else {
                self.reaper.notified().await;
            }
        }
    }

    async fn reap_pass(&self, reaping: &mut HashMap<String, Reaping>) -> bool {
        let doomed = self.doomed.lock().unwrap().clone();
        if doomed.is_empty() {
            return false;
        }
        let a37 = &self.shared.agent37;
        let listed = a37
            .list()
            .await
            .map(|l| l.into_iter().map(|i| i.id).collect::<Vec<_>>())
            .inspect_err(|e| tracing::warn!("listing Agent37 instances to confirm deletes: {e:#}"))
            .ok();
        let mut confirmed = false;
        for item in doomed {
            let Some(id) = &item.sandbox else { continue };
            let done = self.reap_one(reaping, id, listed.as_deref(), a37.delete(id)).await;
            if done {
                confirmed = true;
                let mut doomed = self.doomed.lock().unwrap();
                if let Some(i) = doomed.iter().position(|d| *d == item) {
                    doomed.remove(i);
                }
            }
        }
        if confirmed {
            self.shared.persist();
        }
        !self.doomed.lock().unwrap().is_empty()
    }

    async fn reap_one(
        &self,
        reaping: &mut HashMap<String, Reaping>,
        key: &str,
        listed: Option<&[String]>,
        delete: impl Future<Output = Result<()>>,
    ) -> bool {
        let p = reaping
            .entry(key.to_owned())
            .or_insert(Reaping { since: Instant::now(), told: false, deleted: false });
        if p.deleted && listed.is_some_and(|l| !l.iter().any(|k| k == key)) {
            reaping.remove(key);
            return true;
        }
        if let Err(e) = delete.await {
            tracing::warn!("deleting Agent37 instance {key}: {e:#}");
        }
        p.deleted = true;
        if !p.told && p.since.elapsed() >= REAP_WARN {
            p.told = true;
            self.shared.notify(
                "Agent37 hasn't confirmed a delete",
                format!(
                    "Instance {key} may still be billing disk on Agent37. t3agent37 keeps trying; \
                     you can also delete it in the Agent37 dashboard."
                ),
            );
        }
        false
    }

    fn machine_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join("machines").join(id)
    }

    fn runtime(&self, record: crate::store::Machine) -> Arc<MachineRuntime> {
        let dir = self.machine_dir(&record.id);
        Arc::new(MachineRuntime::new(self.shared.clone(), record, dir))
    }

    fn spawn(&self, machine: Arc<MachineRuntime>) {
        let proxy_machine = machine.clone();
        let proxy = tokio::spawn(async move {
            let port = proxy_machine.record().local_port;
            if let Err(e) = proxy::serve(proxy_machine.clone()).await {
                tracing::error!("proxy on port {port} failed: {e:#}");
                proxy_machine.shared.notify(
                    "t3agent37",
                    format!("{} cannot listen on port {port}: {e}", proxy_machine.record().name),
                );
            }
        });
        let runner = tokio::spawn(machine.clone().run());
        self.machines.write().unwrap().push(Running { machine, tasks: vec![proxy, runner] });
    }

    pub fn machines(&self) -> Vec<Arc<MachineRuntime>> {
        self.machines.read().unwrap().iter().map(|r| r.machine.clone()).collect()
    }

    pub fn machine(&self, machine: &str) -> Result<Arc<MachineRuntime>> {
        self.machines()
            .into_iter()
            .find(|m| m.id() == machine || m.record().name.eq_ignore_ascii_case(machine))
            .ok_or_else(|| anyhow!("no machine {machine}"))
    }

    pub fn settings(&self) -> Settings {
        self.shared.settings.read().unwrap().clone()
    }

    pub fn billing(&self) -> Option<BillingInfo> {
        self.billing.lock().unwrap().clone()
    }

    pub fn save(&self) {
        let spend = self.spend.lock().unwrap().clone();
        let retired_ports = self.retired_ports.lock().unwrap().clone();
        let creating: Vec<Doomed> = self.creating.lock().unwrap().iter().filter_map(Creating::doomed).collect();
        let machines = self.machines().iter().map(|m| m.record()).collect();
        let doomed = self.doomed.lock().unwrap().iter().cloned().chain(creating).collect();
        let state = State { machines, settings: self.settings(), spend, retired_ports, doomed };
        if let Err(e) = self.store.save(&state) {
            tracing::error!("saving state: {e:#}");
        }
    }

    pub fn update_settings(&self, mut settings: Settings) {
        settings.auto_sleep_fuse_minutes = settings.auto_sleep_fuse_minutes.map(|m| m.clamp(5, 1440));
        let prev_fuse = self.settings().auto_sleep_fuse_minutes;
        *self.shared.settings.write().unwrap() = settings.clone();
        self.shared.persist();
        if prev_fuse != settings.auto_sleep_fuse_minutes {
            let fuse = settings.auto_sleep_fuse_minutes;
            for m in self.machines() {
                tokio::spawn(async move {
                    if let Err(e) = m.sync_auto_sleep_fuse(fuse).await {
                        tracing::warn!("updating auto_sleep fuse on {}: {e:#}", m.id());
                    }
                });
            }
        }
    }

    fn reserve(
        &self,
        name: &str,
        size: &str,
        instance_type: InstanceType,
        region: Region,
        disk_gb: u32,
    ) -> Result<u16> {
        let mut creating = self.creating.lock().unwrap();
        let records: Vec<_> = self.machines().iter().map(|m| m.record()).collect();
        let names = records.iter().map(|m| &m.name).chain(creating.iter().map(|c| &c.name));
        if names.into_iter().any(|n| n.eq_ignore_ascii_case(name)) {
            bail!("a machine called {name} already exists");
        }
        let retired = self.retired_ports.lock().unwrap();
        let used: Vec<u16> = records
            .iter()
            .map(|m| m.local_port)
            .chain(creating.iter().map(|c| c.port))
            .chain(retired.iter().copied())
            .collect();
        let port = (FIRST_PORT..FIRST_PORT + PORTS)
            .find(|p| !used.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .ok_or_else(|| anyhow!("no free local port"))?;
        creating.push(Creating {
            name: name.to_owned(),
            size: size.to_owned(),
            instance_type,
            region,
            disk_gb,
            step: "Preparing".into(),
            port,
            sandbox: None,
        });
        Ok(port)
    }

    fn set_creating_sandbox(&self, name: &str, sandbox: &str) {
        if let Some(c) = self.creating.lock().unwrap().iter_mut().find(|c| c.name == name) {
            c.sandbox = Some(sandbox.to_owned());
        }
        self.shared.persist();
    }

    fn set_creating_step(&self, name: &str, step: &str) {
        let mut creating = self.creating.lock().unwrap();
        let Some(c) = creating.iter_mut().find(|c| c.name == name) else {
            return;
        };
        if c.step == step {
            return;
        }
        c.step = step.to_owned();
        drop(creating);
        self.shared.changed();
    }

    /// Creates a new Agent37 machine, provisions T3 Code + supervisor + read-only watchdog,
    /// starts its localhost proxy, and optionally clones `projects`.
    pub async fn create_machine(
        self: &Arc<Self>,
        name: &str,
        size: &str,
        instance_type: InstanceType,
        region: Region,
        disk_gb: Option<u32>,
        projects: Vec<String>,
    ) -> Result<Arc<MachineRuntime>> {
        if !valid_name(name) {
            bail!("machine names are 1-32 lowercase letters, digits or dashes, starting with a letter or digit");
        }
        let shape = agent37::shape(size)?;
        let resources = agent37::validate_shape_and_disk(instance_type, shape.id, disk_gb)?;
        let port = self.reserve(name, shape.id, instance_type, region, resources.disk)?;
        self.shared.changed();
        let settings = self.settings();
        let telemetry = self.shared.telemetry.as_ref().and_then(|telemetry| Some((telemetry, telemetry.attempt()?)));
        let template = if settings.template.trim().is_empty() {
            agent37::DEFAULT_TEMPLATE.to_owned()
        } else {
            settings.template.trim().to_owned()
        };
        let result = async {
            let version = self.shared.t3.version().await?;
            let record = provision::create(
                &self.shared.agent37,
                name,
                shape.id,
                instance_type,
                region,
                Some(resources.disk),
                &template,
                settings.auto_sleep_fuse_minutes,
                &version,
                port,
                |sandbox| self.set_creating_sandbox(name, sandbox),
                |step| self.set_creating_step(name, step),
            )
            .await?;
            let machine = self.runtime(record);
            if let Err(e) = machine.prime().await {
                tracing::warn!("priming {}: {e:#}", machine.id());
            }
            self.spawn(machine.clone());
            Ok::<_, anyhow::Error>(machine)
        }
        .await;
        let failed = {
            let mut creating = self.creating.lock().unwrap();
            let c = creating.iter().position(|c| c.name == name).map(|i| creating.remove(i));
            let doomed = c.filter(|_| result.is_err()).as_ref().and_then(Creating::doomed);
            let failed = doomed.is_some();
            if let Some(d) = doomed {
                self.doomed.lock().unwrap().push(d);
            }
            failed
        };
        self.shared.persist();
        self.shared.changed();
        if failed {
            self.reaper.notify_one();
        }
        if let Err(ref e) = result {
            tracing::error!("creating machine {name} failed: {e:#}");
            self.shared.notify(format!("Couldn't create {name}"), format!("{e:#}"));
        }
        let machine = result?;
        if let Some((telemetry, attempt)) = telemetry {
            telemetry.milestone(Milestone::FirstProvision, &attempt);
        }
        let (app, m) = (self.clone(), machine.clone());
        tokio::spawn(async move {
            for id in projects {
                if let Err(e) = app.add_project(&m, &id).await {
                    app.shared.notify(format!("A project didn't reach {}", m.record().name), format!("{e:#}"));
                }
            }
        });
        if let Err(e) = self.pair(&machine) {
            self.shared.notify(format!("Couldn't open T3 Code for {name}"), format!("{e:#}; use Add to T3 Code"));
        }
        Ok(machine)
    }

    pub async fn local_projects(&self) -> Result<Vec<crate::projects::Project>> {
        self.shared.t3.projects().await
    }

    pub async fn add_project(&self, machine: &MachineRuntime, project: &str) -> Result<crate::projects::Project> {
        let project = self
            .local_projects()
            .await?
            .into_iter()
            .find(|p| p.id == project || p.path == project || p.title.eq_ignore_ascii_case(project))
            .ok_or_else(|| anyhow!("no project {project} with a git remote"))?;
        let (files, mut failed) = self.shared.t3.files_to_copy(&project).await?;
        failed.extend(machine.add_project(&project, files).await?);
        if !failed.is_empty() {
            self.shared.notify(
                format!("Some of {}'s files didn't reach {}", project.title, machine.record().name),
                failed.join("\n"),
            );
        }
        Ok(project)
    }

    pub fn pair(&self, machine: &MachineRuntime) -> Result<()> {
        let code = machine.auth.new_pairing_code();
        let pairing_url = format!("http://localhost:{}/pair#token={code}", machine.record().local_port);
        open_url(&format!("t3code://pair?pairingUrl={}", urlencoding::encode(&pairing_url)))
    }

    pub async fn delete_machine(&self, machine: &MachineRuntime, discard: bool) -> Result<()> {
        let (day, month) = periods();
        self.accrue_spend(machine, day, month, chrono::Utc::now().timestamp());
        if !discard {
            machine.check_idle_before_delete().await?;
        }
        let record = {
            let _waking = machine.waking.lock().await;
            machine.retire();
            machine.record()
        };
        let id = record.id.as_str();
        self.doomed.lock().unwrap().push(Doomed { sandbox: Some(record.sandbox_id.clone()) });
        let running = {
            let mut machines = self.machines.write().unwrap();
            let idx = machines.iter().position(|r| r.machine.id() == id);
            idx.map(|i| machines.remove(i))
        };
        if let Some(r) = running {
            r.tasks.iter().for_each(JoinHandle::abort);
        }
        self.retired_ports.lock().unwrap().push(record.local_port);
        self.shared.persist();
        self.shared.changed();
        self.reaper.notify_one();
        let _ = std::fs::remove_dir_all(machine.dir());
        if let Err(e) = self.shared.t3.unlink(&record.environment_id).await {
            tracing::warn!("unlinking {id} from T3 Code: {e:#}");
        }
        if !record.client_tokens.is_empty() {
            self.shared.notify(
                format!("Remove {} from T3 Code", record.name),
                "T3 Code still lists it. Remove it there under Settings → Environments.",
            );
            let _ = self.shared.t3.activate();
        }
        Ok(())
    }

    pub fn views(&self) -> Vec<MachineView> {
        let mut views: Vec<MachineView> = self.machines().iter().map(|m| view(m)).collect();
        for c in self.creating.lock().unwrap().iter() {
            let hourly_rate = agent37::shape(&c.size)
                .map(|s| agent37::total_hourly(c.instance_type, agent37::Resources { cpu: s.cpu, memory: s.memory, disk: c.disk_gb }))
                .unwrap_or(0.0);
            views.push(MachineView {
                id: String::new(),
                name: c.name.clone(),
                size: c.size.clone(),
                instance_type: c.instance_type,
                region: c.region,
                disk_gb: c.disk_gb,
                idle_monthly: agent37::disk_monthly(c.disk_gb),
                hourly_rate,
                disk_usage: None,
                disk_warning: false,
                github_signed_in: false,
                state: MachineState::Creating,
                detail: c.step.clone(),
                countdown: None,
                paired: false,
                signed_out: Vec::new(),
                projects: Vec::new(),
                keep_awake_until: None,
                keep_awake_forever: false,
                waiting: false,
            });
        }
        views
    }

    pub fn costs(&self) -> Costs {
        let (day, month) = periods();
        let billing = self.billing.lock().unwrap().clone().unwrap_or_default();
        let machines = self.machines();
        let mut idle_monthly = 0.0;
        let mut awake_ram_gb = 0u32;
        for m in &machines {
            if m.power() == Power::Missing {
                continue;
            }
            let r = m.record();
            idle_monthly += agent37::disk_monthly(r.disk_gb);
            if matches!(m.power(), Power::Running | Power::Starting | Power::Stopping) {
                if let Ok(s) = agent37::shape(&r.size) {
                    awake_ram_gb = awake_ram_gb.saturating_add(s.memory);
                }
            }
        }
        let mut costs = Costs {
            idle_monthly,
            wallet_balance: billing.balance_dollars,
            awake_ram_gb,
            memory_limit_gb: billing.memory_limit_gb,
            ..Costs::default()
        };
        for spend in self.spend.lock().unwrap().values() {
            let u = usage(spend, day, month);
            costs.today += u.today;
            costs.month += u.month;
        }
        costs
    }

    fn usage(&self, machine: &MachineRuntime) -> MachineUsage {
        let (day, month) = periods();
        let id = machine.id();
        let mut total = MachineUsage::default();
        for spend in self.spend.lock().unwrap().values().filter(|s| s.machine == id) {
            let u = usage(spend, day, month);
            total.today += u.today;
            total.month += u.month;
            total.awake_secs_today += u.awake_secs_today;
        }
        total
    }

    /// Accrues estimated Agent37 spend locally: disk accrues continuously ($0.09/GB/mo),
    /// compute accrues while `Running`.
    fn accrue_spend(&self, machine: &MachineRuntime, day: i64, month: i64, now: i64) {
        if machine.power() == Power::Missing {
            return;
        }
        let r = machine.record();
        let sandbox = r.sandbox_id.clone();
        let running = machine.power() == Power::Running;
        let compute_rate = agent37::shape(&r.size)
            .map(|s| agent37::compute_hourly(r.instance_type, s.cpu, s.memory))
            .unwrap_or(0.0);
        let disk_rate = agent37::disk_hourly(r.disk_gb);

        let mut map = self.spend.lock().unwrap();
        let entry = map.entry(sandbox).or_insert(Spend {
            machine: r.id.clone(),
            day,
            month_start: month,
            today: 0.0,
            awake_secs_today: 0,
            month: 0.0,
            last_tick_at: r.created_at.max(month).min(now),
        });
        accrue_entry(entry, day, month, now, running, compute_rate, disk_rate);
    }

    pub async fn refresh_costs(&self) {
        let (day, month) = periods();
        let now = chrono::Utc::now().timestamp();
        for m in self.machines() {
            self.accrue_spend(&m, day, month, now);
        }
        self.spend.lock().unwrap().retain(|_, s| s.day >= month);
        if let Ok(b) = self.shared.agent37.billing().await {
            *self.billing.lock().unwrap() = Some(b);
        }
        self.shared.persist();
        self.shared.changed();
    }
}

fn local_time(t: chrono::DateTime<chrono::Utc>) -> String {
    let t = t.with_timezone(&chrono::Local);
    if t.date_naive() == chrono::Local::now().date_naive() {
        t.format("at %H:%M").to_string()
    } else {
        t.format("%a %H:%M").to_string()
    }
}

fn periods() -> (i64, i64) {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    let start = |day| {
        Local
            .with_ymd_and_hms(now.year(), now.month(), day, 0, 0, 0)
            .single()
            .map(|t| t.timestamp())
            .unwrap_or_default()
    };
    (start(now.day()), start(1))
}

fn usage(spend: &Spend, day: i64, month: i64) -> MachineUsage {
    let today = spend.day >= day;
    MachineUsage {
        today: if today { spend.today } else { 0.0 },
        month: if spend.day >= month { spend.month } else { 0.0 },
        awake_secs_today: if today { spend.awake_secs_today } else { 0 },
    }
}

/// Splits accrual cleanly across day and month boundaries:
/// - Disk accrues continuously from `last_tick_at..now`, crediting only post-`month` seconds to
///   `month` and only post-`day` seconds to `today`.
/// - Compute accrues while `running`, capped at `MAX_COMPUTE_GAP_SECS` on offline restart gaps and
///   bounded within today (`now - day`).
fn accrue_entry(
    entry: &mut Spend,
    day: i64,
    month: i64,
    now: i64,
    running: bool,
    compute_rate: f64,
    disk_rate: f64,
) {
    const MAX_COMPUTE_GAP_SECS: u64 = 600;
    if entry.day < day {
        entry.day = day;
        entry.today = 0.0;
        entry.awake_secs_today = 0;
    }
    if entry.month_start < month {
        entry.month_start = month;
        entry.month = 0.0;
    }
    let prev = entry.last_tick_at.min(now);
    entry.last_tick_at = now;
    if now <= prev {
        return;
    }
    let disk_month_secs = (now - prev.max(month)).max(0) as u64;
    let disk_today_secs = (now - prev.max(day)).max(0) as u64;
    let total_elapsed = (now - prev) as u64;
    let compute_secs = if running { total_elapsed.min(MAX_COMPUTE_GAP_SECS) } else { 0 };
    let compute_month_secs = compute_secs.min(disk_month_secs);
    let compute_today_secs = compute_secs.min(disk_today_secs);

    entry.month += (disk_month_secs as f64 / 3600.0) * disk_rate + (compute_month_secs as f64 / 3600.0) * compute_rate;
    entry.today += (disk_today_secs as f64 / 3600.0) * disk_rate + (compute_today_secs as f64 / 3600.0) * compute_rate;
    entry.awake_secs_today = entry.awake_secs_today.saturating_add(compute_today_secs);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineDetails {
    pub id: String,
    pub instance_type: InstanceType,
    pub region: Region,
    pub disk_gb: u32,
    pub idle_monthly: f64,
    pub hourly_rate: f64,
    pub disk_usage: Option<DiskUsage>,
    pub github_signed_in: bool,
    pub cost_today: f64,
    pub cost_month: f64,
    pub awake_secs_today: u64,
    pub agents: Vec<AgentLogin>,
    pub t3_version: String,
    pub created_at: i64,
    pub connect: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLogin {
    pub name: String,
    pub signed_in: bool,
}

impl App {
    pub fn details(&self, machine: &str) -> Result<MachineDetails> {
        let machine = self.machine(machine)?;
        let id = machine.id();
        let record = machine.record();
        let usage = self.usage(&machine);
        let agents = machine.cache.lock().unwrap().agents();
        let hourly_rate = agent37::shape(&record.size)
            .map(|s| {
                agent37::total_hourly(
                    record.instance_type,
                    agent37::Resources { cpu: s.cpu, memory: s.memory, disk: record.disk_gb },
                )
            })
            .unwrap_or(0.0);
        Ok(MachineDetails {
            id,
            instance_type: record.instance_type,
            region: record.region,
            disk_gb: record.disk_gb,
            idle_monthly: agent37::disk_monthly(record.disk_gb),
            hourly_rate,
            disk_usage: machine.disk_usage(),
            github_signed_in: record.github_signed_in,
            cost_today: usage.today,
            cost_month: usage.month,
            awake_secs_today: usage.awake_secs_today,
            agents: agents
                .into_iter()
                .filter(|(_, status)| matches!(status.as_str(), "authenticated" | "unauthenticated"))
                .map(|(name, status)| AgentLogin { name, signed_in: status == "authenticated" })
                .collect(),
            t3_version: record.t3_version,
            created_at: record.created_at,
            connect: record.connect,
        })
    }

    pub fn open_in_t3(&self, machine: &MachineRuntime, thread: Option<&str>) -> Result<()> {
        let record = machine.record();
        if record.client_tokens.is_empty() {
            return self.pair(machine);
        }
        match thread {
            Some(thread) => self.shared.t3.open_thread(&record.environment_id, thread),
            None => self.shared.t3.activate(),
        }
    }
}

fn view(m: &MachineRuntime) -> MachineView {
    let r = m.record();
    let watchdog = m.watchdog_status();
    let mut countdown = None;
    let mut count_down = |template, secs, from| {
        let c = Countdown::new(template, secs, from);
        let text = c.text(secs);
        countdown = Some(c);
        text
    };
    let idle_monthly = agent37::disk_monthly(r.disk_gb);
    let hourly_rate = agent37::shape(&r.size)
        .map(|s| agent37::total_hourly(r.instance_type, agent37::Resources { cpu: s.cpu, memory: s.memory, disk: r.disk_gb }))
        .unwrap_or(0.0);
    use MachineState::*;
    let (state, detail) = match (m.power(), &watchdog) {
        (Power::Missing, _) => (Missing, "Deleted on Agent37".to_owned()),
        (_, _) if m.last_error().is_some() => (Error, m.last_error().unwrap_or_default()),
        (Power::Running, Some(w)) if w.agents_unknown => (Working, "T3 not answering · stays up 30m max".to_owned()),
        (Power::Running, Some(w)) if w.cpu_busy => (Working, "Working · busy CPU".to_owned()),
        (Power::Running, Some(w)) if w.busy => (Working, working_line(w.working_threads, w.working_subagents)),
        (Power::Running, Some(w)) if w.phase == WatchdogPhase::Active && w.keep_awake_forever => {
            (Idle, "Awake until you stop it".to_owned())
        }
        (Power::Running, Some(w)) if w.phase == WatchdogPhase::Active && w.keep_awake_secs > 0 => {
            (Idle, count_down("Kept awake · {left} left", w.keep_awake_secs, w.received))
        }
        (Power::Running, Some(w)) if w.phase == WatchdogPhase::Active && w.work_due => {
            let due = w.next_due_at.and_then(chrono::DateTime::from_timestamp_millis);
            (Idle, due.map_or("Idle · scheduled work due".to_owned(), |t| format!("Idle · scheduled work {}", local_time(t))))
        }
        (Power::Running, Some(w)) if w.phase == WatchdogPhase::Active => {
            let left = w.sleep_after_secs.saturating_sub(w.idle_secs);
            let template = if w.waiting_threads > 0 { "Waiting on you · sleeps in {left}" } else { "Idle · sleeps in {left}" };
            (Idle, count_down(template, left, w.received))
        }
        (Power::Running, Some(_)) | (Power::Stopping, _) => (Sleeping, "Going to sleep".to_owned()),
        (Power::Running, None) => (Idle, "Running".to_owned()),
        (Power::Starting, _) => (Waking, "Waking up".to_owned()),
        (Power::Stopped, _) => match m.scheduled_wake() {
            Some(due) => (Asleep, format!("Asleep · wakes {} for scheduled work", local_time(due))),
            None => (Asleep, format!("Asleep · ${idle_monthly:.2}/mo disk")),
        },
        (Power::Unknown, _) => (Unknown, "Checking".to_owned()),
    };
    let running_watchdog = watchdog.as_ref().filter(|_| m.power() == Power::Running);
    let disk_usage = m.disk_usage();
    let disk_warning = disk_usage.as_ref().is_some_and(|d| d.used_percent >= 80.0);
    MachineView {
        id: r.id.clone(),
        name: r.name.clone(),
        size: r.size.clone(),
        instance_type: r.instance_type,
        region: r.region,
        disk_gb: r.disk_gb,
        idle_monthly,
        hourly_rate,
        disk_usage,
        disk_warning,
        github_signed_in: r.github_signed_in,
        state,
        detail,
        countdown,
        paired: !r.client_tokens.is_empty(),
        signed_out: m.cache.lock().unwrap().signed_out_providers(),
        projects: m.projects(),
        keep_awake_until: running_watchdog
            .filter(|w| w.keep_awake_secs > 0)
            .map(|w| unix_ms_after(w.received, w.keep_awake_secs)),
        keep_awake_forever: running_watchdog.is_some_and(|w| w.keep_awake_forever),
        waiting: running_watchdog.is_some_and(|w| w.waiting_threads > 0),
    }
}

fn valid_name(name: &str) -> bool {
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    name.len() <= 32 && name.starts_with(ok) && name.chars().all(|c| ok(c) || c == '-')
}

fn working_line(threads: u32, subagents: u32) -> String {
    let count = |n: u32, one: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {one}s")),
    };
    std::iter::once("Working".to_owned())
        .chain(count(threads, "thread"))
        .chain(count(subagents, "subagent"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn human_secs(s: u64) -> String {
    let m = s.div_ceil(60);
    match (s, m) {
        (0..60, _) => format!("{s}s"),
        (_, 0..60) => format!("{m}m"),
        _ => format!("{}h {}m", m / 60, m % 60),
    }
}

#[cfg(all(target_os = "linux", feature = "desktop"))]
pub fn open_url(url: &str) -> Result<()> {
    use gtk::gio;
    gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>)
        .map_err(|e| anyhow::anyhow!("could not open {url}: {e}"))
}

#[cfg(all(target_os = "linux", not(feature = "desktop")))]
pub fn open_url(url: &str) -> Result<()> {
    // Spawn non-blocking so a hanging browser/xdg-open never stalls the single-threaded runtime.
    let mut child = std::process::Command::new("gio")
        .args(["open", url])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .or_else(|_| {
            std::process::Command::new("xdg-open")
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        })
        .with_context(|| format!("could not open {url}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn open_url(url: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).status()
    }?;
    if !status.success() {
        bail!("could not open {url}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as AxumPath, State as AxumState};
    use axum::http::StatusCode;
    use axum::routing::{delete, get};
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn valid_name() {
        for ok in ["dev", "0", "a-b-", &"a".repeat(32)] {
            assert!(super::valid_name(ok), "{ok}");
        }
        for bad in ["", "-dev", "Dev", "my dev", "dév", &"a".repeat(33)] {
            assert!(!super::valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn spend_counts_only_in_its_periods() {
        let spend = Spend {
            machine: "m".into(),
            day: 100,
            month_start: 0,
            today: 1.0,
            awake_secs_today: 60,
            month: 5.0,
            last_tick_at: 100,
        };
        let same_day = super::usage(&spend, 100, 0);
        assert_eq!((same_day.today, same_day.awake_secs_today, same_day.month), (1.0, 60, 5.0));
        let later_day = super::usage(&spend, 200, 0);
        assert_eq!((later_day.today, later_day.awake_secs_today, later_day.month), (0.0, 0, 5.0));
        let later_month = super::usage(&spend, 300, 300);
        assert_eq!((later_month.today, later_month.month), (0.0, 0.0));
    }

    #[test]
    fn accrue_entry_splits_midnight_rollover_and_caps_compute_gap() {
        // Month starts at t=0, day 1 at t=0, day 2 (midnight) at t=86400.
        // Last tick was 2 hours before midnight (79200); app reopens 1 hour after midnight (90000).
        let mut spend = Spend {
            machine: "m".into(),
            day: 0,
            month_start: 0,
            today: 1.0,
            awake_secs_today: 3600,
            month: 2.0,
            last_tick_at: 79_200,
        };
        let disk_rate = 1.0; // $1/hr for clean math
        let compute_rate = 6.0; // $6/hr -> $1 per 600s cap
        super::accrue_entry(&mut spend, 86_400, 0, 90_000, true, compute_rate, disk_rate);
        // Disk: 3h total in month ($3), only 1h after midnight in today ($1).
        // Compute: capped at 600s ($1) in both month and today.
        assert_eq!(spend.day, 86_400);
        assert_eq!(spend.awake_secs_today, 600);
        assert!((spend.today - 2.0).abs() < 1e-6, "today={}", spend.today);
        assert!((spend.month - 6.0).abs() < 1e-6, "month={}", spend.month);
    }

    #[tokio::test]
    async fn reaper_deletes_until_agent37_list_confirms() {
        struct MockApi {
            list_down: AtomicBool,
            a_gone: AtomicBool,
            deletes: Mutex<Vec<String>>,
        }
        let api = Arc::new(MockApi {
            list_down: AtomicBool::new(true),
            a_gone: AtomicBool::new(false),
            deletes: Mutex::new(Vec::new()),
        });

        let router = Router::new()
            .route(
                "/v1/instances",
                get(|AxumState(s): AxumState<Arc<MockApi>>| async move {
                    if s.list_down.load(Ordering::SeqCst) {
                        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "down" })));
                    }
                    let mut list = vec![json!({ "id": "inst_stuck", "status": "running" })];
                    if !s.a_gone.load(Ordering::SeqCst) {
                        list.push(json!({ "id": "inst_a", "status": "sleeping" }));
                    }
                    (StatusCode::OK, Json(json!({ "instances": list })))
                }),
            )
            .route(
                "/v1/instances/{id}",
                delete(|AxumState(s): AxumState<Arc<MockApi>>, AxumPath(id): AxumPath<String>| async move {
                    s.deletes.lock().unwrap().push(id.clone());
                    if id == "inst_a" && !s.list_down.load(Ordering::SeqCst) {
                        s.a_gone.store(true, Ordering::SeqCst);
                    }
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(api.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let base = format!("http://{addr}");

        let dir = std::env::temp_dir().join(format!("t3agent37-reaper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let a37 = Agent37::with_endpoints("sk_live_test".into(), base.clone(), base.clone(), format!("{base}/{{id}}/{{port}}"))
            .unwrap()
            .with_fast_retries();
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let t3 = T3Desktop::at(dir.join("t3"));
        let app = App::new(a37, t3, dir.clone(), events).unwrap();

        app.doomed.lock().unwrap().extend([
            Doomed { sandbox: Some("inst_a".into()) },
            Doomed { sandbox: Some("inst_stuck".into()) },
            Doomed { sandbox: Some("inst_ghost".into()) },
        ]);

        // Pass 1: list fails, all deletes still attempted, nothing dropped.
        let mut reaping = HashMap::new();
        assert!(app.reap_pass(&mut reaping).await);
        assert_eq!(app.doomed.lock().unwrap().len(), 3);

        // Pass 2: API back up; inst_ghost confirmed gone, inst_a deleted now (confirmed next pass).
        api.list_down.store(false, Ordering::SeqCst);
        assert!(app.reap_pass(&mut reaping).await);
        assert_eq!(app.doomed.lock().unwrap().len(), 2);

        // Pass 3: inst_a confirmed gone; only inst_stuck remains.
        assert!(app.reap_pass(&mut reaping).await);
        let remaining: Vec<_> = app.doomed.lock().unwrap().iter().filter_map(|d| d.sandbox.clone()).collect();
        assert_eq!(remaining, vec!["inst_stuck"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
