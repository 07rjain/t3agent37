//! Best-effort desktop metrics. The daemon supplies no telemetry instance.

use crate::{store::write_private, updater::CURRENT_VERSION};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::{sync::mpsc, task::JoinHandle};

const QUEUE_CAPACITY: usize = 32;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Consent {
    Undecided,
    Disabled,
    Enabled,
}

#[derive(Serialize, Deserialize)]
struct Preferences {
    installation: Option<Installation>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Installation {
    id: String,
    first_launch: bool,
    first_provision: bool,
    first_pair: bool,
    pending_update: Option<String>,
}

#[derive(Clone, Copy)]
pub enum Milestone {
    FirstProvision,
    FirstPair,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStage {
    Check,
    Download,
    Verify,
    Install,
    OpenReleasePage,
    Relaunch,
}

#[derive(Clone, Copy, Serialize)]
enum DurationBucket {
    #[serde(rename = "under_5s")]
    Under5s,
    #[serde(rename = "5_to_29s")]
    From5To29s,
    #[serde(rename = "30_to_119s")]
    From30To119s,
    #[serde(rename = "at_least_120s")]
    AtLeast120s,
}

impl From<Duration> for DurationBucket {
    fn from(duration: Duration) -> Self {
        match duration.as_secs() {
            0..=4 => Self::Under5s,
            5..=29 => Self::From5To29s,
            30..=119 => Self::From30To119s,
            _ => Self::AtLeast120s,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    CaptureFailed,
    WatchdogFailed,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    FirstLaunch,
    FirstMachineProvisioned,
    FirstT3Paired,
    MachineWake {
        outcome: Outcome,
        duration: DurationBucket,
    },
    MachineSleep {
        outcome: Outcome,
        duration: DurationBucket,
    },
    UpdateFailed {
        stage: UpdateStage,
    },
    UpdateSucceeded,
}

#[derive(Serialize)]
struct Payload<'a> {
    install_id: &'a str,
    app_version: &'static str,
    os: &'static str,
    os_major: Option<u16>,
    arch: &'static str,
    #[serde(flatten)]
    event: Event,
}

struct Transport {
    http: reqwest::Client,
    endpoint: url::Url,
}

struct Session {
    events: mpsc::Sender<Event>,
    task: JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Stop the receiver and discard everything that hasn't been sent, including a
        // request still being awaited. A request already received by the server may finish.
        self.task.abort();
    }
}

struct State {
    preferences: Option<Preferences>,
    session: Option<Session>,
}

pub struct Telemetry {
    path: PathBuf,
    transport: Transport,
    state: Mutex<State>,
}

/// Belongs to the consent session active when the operation began. Its sender becomes
/// unusable on opt-out, even if the user opts back in before the operation completes.
pub struct Attempt {
    events: mpsc::Sender<Event>,
    started: Instant,
}

impl Attempt {
    pub fn wake(self, outcome: Outcome) {
        let _ = self.events.try_send(Event::MachineWake {
            outcome,
            duration: self.started.elapsed().into(),
        });
    }

    pub fn sleep(self, outcome: Outcome) {
        let _ = self.events.try_send(Event::MachineSleep {
            outcome,
            duration: self.started.elapsed().into(),
        });
    }

    pub fn update_failed(self, stage: UpdateStage) {
        let _ = self.events.try_send(Event::UpdateFailed { stage });
    }
}

impl Telemetry {
    pub fn configured(dir: &Path) -> Option<Self> {
        let endpoint = option_env!("T3AGENT37_TELEMETRY_URL")?
            .trim()
            .parse::<url::Url>()
            .ok()?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.path() != "/v1/events"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return None;
        }
        Self::load(dir, endpoint).ok()
    }

    fn load(dir: &Path, endpoint: url::Url) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let path = dir.join("telemetry.json");
        let preferences = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Preferences>(&bytes).ok())
            .filter(|prefs| {
                prefs.installation.as_ref().is_none_or(|installation| {
                    installation.id.len() == 32
                        && installation
                            .id
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                })
            });
        Ok(Self {
            path,
            transport: Transport { http, endpoint },
            state: Mutex::new(State {
                preferences,
                session: None,
            }),
        })
    }

    pub fn consent(&self) -> Consent {
        match &self.state.lock().unwrap().preferences {
            None => Consent::Undecided,
            Some(prefs) if prefs.installation.is_some() => Consent::Enabled,
            Some(_) => Consent::Disabled,
        }
    }

    pub async fn set_consent(&self, enabled: bool) -> Result<()> {
        let retired = {
            let mut state = self.state.lock().unwrap();
            let is_enabled = state
                .preferences
                .as_ref()
                .is_some_and(|prefs| prefs.installation.is_some());
            let retired = if state.preferences.is_none() || enabled != is_enabled {
                let installation = enabled.then(|| Installation {
                    id: format!("{:032x}", rand::random::<u128>()),
                    first_launch: false,
                    first_provision: false,
                    first_pair: false,
                    pending_update: None,
                });
                let prefs = Preferences { installation };
                self.save(&prefs)?;
                state.preferences = Some(prefs);
                state.session.take()
            } else {
                None
            };
            self.start_session(&mut state);
            retired
        };
        if let Some(mut session) = retired {
            session.task.abort();
            let _ = (&mut session.task).await;
        }
        Ok(())
    }

    pub fn start(&self) {
        self.start_session(&mut self.state.lock().unwrap());
    }

    fn start_session(&self, state: &mut State) {
        if state.session.is_some() {
            return;
        }
        let Some(mut installation) = state
            .preferences
            .as_ref()
            .and_then(|prefs| prefs.installation.clone())
        else {
            return;
        };
        let (events, receive) = mpsc::channel(QUEUE_CAPACITY);
        let session = Session {
            events,
            task: tokio::spawn(send_events(
                self.transport.http.clone(),
                self.transport.endpoint.clone(),
                installation.id.clone(),
                receive,
            )),
        };
        let had_launch = installation.first_launch;
        if !had_launch && session.events.try_send(Event::FirstLaunch).is_ok() {
            installation.first_launch = true;
        }
        let had_update = installation.pending_update.is_some();
        if let Some(version) = installation.pending_update.take() {
            let event = if version == CURRENT_VERSION {
                Event::UpdateSucceeded
            } else {
                Event::UpdateFailed {
                    stage: UpdateStage::Relaunch,
                }
            };
            let _ = session.events.try_send(event);
        }
        if installation.first_launch != had_launch || had_update {
            self.save_progress(state, installation);
        }
        state.session = Some(session);
    }

    pub fn attempt(&self) -> Option<Attempt> {
        let state = self.state.lock().unwrap();
        Some(Attempt {
            events: state.session.as_ref()?.events.clone(),
            started: Instant::now(),
        })
    }

    pub fn milestone(&self, milestone: Milestone, attempt: &Attempt) {
        let mut state = self.state.lock().unwrap();
        let Some(mut installation) = state.installation_for(attempt) else {
            return;
        };
        let event = match milestone {
            Milestone::FirstProvision if !installation.first_provision => {
                installation.first_provision = true;
                Event::FirstMachineProvisioned
            }
            Milestone::FirstPair if installation.first_provision && !installation.first_pair => {
                installation.first_pair = true;
                Event::FirstT3Paired
            }
            _ => return,
        };
        if attempt.events.try_send(event).is_ok() {
            self.save_progress(&mut state, installation);
        }
    }

    pub fn pending_update(&self, attempt: &Attempt, target: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        let Some(mut installation) = state.installation_for(attempt) else {
            return;
        };
        installation.pending_update = target.map(str::to_owned);
        self.save_progress(&mut state, installation);
    }

    fn save_progress(&self, state: &mut State, installation: Installation) {
        let prefs = Preferences {
            installation: Some(installation),
        };
        if self.save(&prefs).is_err() {
            tracing::warn!("Could not save desktop telemetry progress");
        }
        state.preferences = Some(prefs);
    }

    fn save(&self, prefs: &Preferences) -> Result<()> {
        write_private(&self.path, &serde_json::to_vec(prefs)?)
            .context("Could not save desktop telemetry preferences")
    }
}

impl State {
    fn installation_for(&self, attempt: &Attempt) -> Option<Installation> {
        if !self.session.as_ref()?.events.same_channel(&attempt.events) {
            return None;
        }
        self.preferences.as_ref()?.installation.clone()
    }
}

async fn send_events(
    http: reqwest::Client,
    endpoint: url::Url,
    install_id: String,
    mut events: mpsc::Receiver<Event>,
) {
    let os_major = os_major().await;
    while let Some(event) = events.recv().await {
        let payload = Payload {
            install_id: &install_id,
            app_version: CURRENT_VERSION,
            os: std::env::consts::OS,
            os_major,
            arch: std::env::consts::ARCH,
            event,
        };
        let _ = http.post(endpoint.clone()).json(&payload).send().await;
    }
}

async fn os_major() -> Option<u16> {
    #[cfg(target_os = "macos")]
    {
        let output = tokio::time::timeout(
            Duration::from_secs(1),
            crate::process::command("/usr/bin/sw_vers")
                .arg("-productVersion")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if output.status.success() {
            return std::str::from_utf8(&output.stdout)
                .ok()?
                .trim()
                .split('.')
                .next()?
                .parse()
                .ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State as AxumState, routing::post};
    use serde_json::Value;
    use tokio::net::TcpListener;

    struct Collector {
        dir: PathBuf,
        url: url::Url,
        events: mpsc::UnboundedReceiver<Value>,
        task: JoinHandle<()>,
    }

    impl Collector {
        async fn start() -> Self {
            let (tx, events) = mpsc::unbounded_channel();
            let app = Router::new()
                .route(
                    "/v1/events",
                    post(
                        async |AxumState(tx): AxumState<mpsc::UnboundedSender<Value>>,
                               Json(event): Json<Value>| {
                            tx.send(event).unwrap();
                        },
                    ),
                )
                .with_state(tx);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1/events", listener.local_addr().unwrap())
                .parse()
                .unwrap();
            let dir = std::env::temp_dir().join(format!(
                "t3agent37-telemetry-{:032x}",
                rand::random::<u128>()
            ));
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self {
                dir,
                url,
                events,
                task,
            }
        }

        fn telemetry(&self) -> Telemetry {
            Telemetry::load(&self.dir, self.url.clone()).unwrap()
        }

        async fn next(&mut self, name: &str) -> Value {
            let event = tokio::time::timeout(Duration::from_secs(3), self.events.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event["event"], name);
            event
        }

        async fn expect_no_events(&mut self) {
            assert!(
                tokio::time::timeout(Duration::from_millis(60), self.events.recv())
                    .await
                    .is_err()
            );
        }
    }

    impl Drop for Collector {
        fn drop(&mut self) {
            self.task.abort();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn consent_defaults_off_and_opt_out_discards_queue_and_install_id() {
        let mut collector = Collector::start().await;
        let telemetry = collector.telemetry();
        telemetry.start();
        assert!(matches!(telemetry.consent(), Consent::Undecided));
        assert!(telemetry.attempt().is_none());
        assert!(!telemetry.path.exists());
        telemetry.set_consent(false).await.unwrap();
        assert!(matches!(collector.telemetry().consent(), Consent::Disabled));
        collector.expect_no_events().await;

        telemetry.set_consent(true).await.unwrap();
        let launch = collector.next("first_launch").await;
        assert_eq!(launch.as_object().unwrap().len(), 6);
        let original_id = launch["install_id"].as_str().unwrap();
        assert_eq!(original_id.len(), 32);
        let old_attempt = telemetry.attempt().unwrap();
        telemetry.attempt().unwrap().wake(Outcome::Succeeded);
        telemetry.set_consent(false).await.unwrap();
        assert!(telemetry.attempt().is_none());
        let preferences = std::fs::read_to_string(&telemetry.path).unwrap();
        assert!(!preferences.contains(original_id));
        collector.expect_no_events().await;

        telemetry.set_consent(true).await.unwrap();
        telemetry.milestone(Milestone::FirstProvision, &old_attempt);
        old_attempt.update_failed(UpdateStage::Install);
        let launch = collector.next("first_launch").await;
        assert_ne!(launch["install_id"], original_id);
        collector.expect_no_events().await;
    }

    #[tokio::test]
    async fn sends_milestones_once_and_confirms_update_on_next_launch() {
        let mut collector = Collector::start().await;
        let telemetry = collector.telemetry();
        telemetry.set_consent(true).await.unwrap();
        let launch = collector.next("first_launch").await;
        let attempt = telemetry.attempt().unwrap();
        telemetry.milestone(Milestone::FirstPair, &attempt);
        telemetry.milestone(Milestone::FirstProvision, &attempt);
        telemetry.milestone(Milestone::FirstProvision, &attempt);
        telemetry.milestone(Milestone::FirstPair, &attempt);
        telemetry.milestone(Milestone::FirstPair, &attempt);
        assert_eq!(
            collector.next("first_machine_provisioned").await["install_id"],
            launch["install_id"]
        );
        assert_eq!(
            collector.next("first_t3_paired").await["install_id"],
            launch["install_id"]
        );

        telemetry.pending_update(&attempt, Some(CURRENT_VERSION));
        collector.expect_no_events().await;
        drop(telemetry);

        let restarted = collector.telemetry();
        restarted.start();
        assert_eq!(
            collector.next("update_succeeded").await["install_id"],
            launch["install_id"]
        );
        let attempt = restarted.attempt().unwrap();
        restarted.milestone(Milestone::FirstProvision, &attempt);
        restarted.milestone(Milestone::FirstPair, &attempt);
        attempt.sleep(Outcome::WatchdogFailed);
        let sleep = collector.next("machine_sleep").await;
        assert_eq!(sleep["outcome"], "watchdog_failed");
        assert_eq!(sleep["duration"], "under_5s");
        collector.expect_no_events().await;
    }

    #[tokio::test]
    async fn full_queue_never_waits_for_the_collector() {
        let collector = Collector::start().await;
        collector.task.abort();
        let telemetry = collector.telemetry();
        telemetry.set_consent(true).await.unwrap();
        for _ in 0..(QUEUE_CAPACITY * 2) {
            telemetry.attempt().unwrap().wake(Outcome::Failed);
        }
        // This task hasn't yielded to the network worker. Excess events are discarded.
        assert_eq!(telemetry.attempt().unwrap().events.capacity(), 0);
        telemetry.set_consent(false).await.unwrap();
    }
}
