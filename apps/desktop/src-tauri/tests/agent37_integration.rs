use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State, WebSocketUpgrade, ws::Message as UpMessage},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use t3boat_lib::{
    agent37::Agent37,
    machine::{MachineRuntime, Power, Shared},
    proxy,
    store::{LinkToken, Machine, Settings},
    t3desktop::T3Desktop,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, protocol::frame::coding::CloseCode},
};

const EDGE_KEY: &str = "test-agent37-key";
const UPSTREAM: &str = "test-upstream-token";
const WATCHDOG: &str = "test-watchdog-secret";
const LINK: &str = "test-link-token";
const SHELL: &str = "/api/orchestration/shell";
const INSTANCE: &str = "/v1/instances/test-instance";
const LIMIT: Duration = Duration::from_secs(10);
const LOCAL_REPLY: Duration = Duration::from_secs(1);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Control {
    status: &'static str,
    start_attempts: usize,
}

struct Mock {
    control: Mutex<Control>,
    events: Mutex<Vec<&'static str>>,
    commands: Mutex<Vec<Value>>,
    start_allowed: Semaphore,
    awaiting_activity: AtomicBool,
    null_manifest: AtomicBool,
    reject_sleep_ack: AtomicBool,
    fail_resize: AtomicBool,
}

impl Mock {
    fn event(&self, event: &'static str) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

fn bearer(headers: &HeaderMap, token: &str) {
    assert_eq!(
        headers.get("authorization").unwrap(),
        format!("Bearer {token}").as_str()
    );
}

fn edge(headers: &HeaderMap) {
    assert_eq!(
        headers.get_all("x-agent37-key").iter().collect::<Vec<_>>(),
        [EDGE_KEY]
    );
}

fn instance(status: &str) -> Value {
    json!({"id": "test-instance", "status": status})
}

async fn control(State(mock): State<Arc<Mock>>, req: Request) -> Response {
    bearer(req.headers(), EDGE_KEY);
    let path = req.uri().path().to_owned();
    match (req.method(), path.as_str()) {
        (&Method::GET, "/v1/billing") => Json(json!({"balance_cents": 1250})).into_response(),
        (&Method::GET, "/v1/instances") => {
            let status = mock.control.lock().unwrap().status;
            Json(if status == "deleted" {
                json!([])
            } else {
                json!([instance(status)])
            })
            .into_response()
        }
        (&Method::GET, INSTANCE) => {
            let mut control = mock.control.lock().unwrap();
            let status = control.status;
            match status {
                "deleted" => return StatusCode::NOT_FOUND.into_response(),
                "starting" | "updating" => {
                    mock.event(status);
                    control.status = "running";
                }
                "running" => mock.event("running"),
                _ => {}
            }
            Json(instance(status)).into_response()
        }
        (&Method::DELETE, INSTANCE) => {
            mock.control.lock().unwrap().status = "deleted";
            StatusCode::NO_CONTENT.into_response()
        }
        (&Method::POST, "/v1/instances/test-instance/start") => {
            mock.event("start");
            let retry = {
                let mut control = mock.control.lock().unwrap();
                assert_eq!(control.status, "sleeping");
                control.start_attempts += 1;
                control.start_attempts <= 2
            };
            if retry {
                return (StatusCode::CONFLICT, Json(json!({"error": "try_again"}))).into_response();
            }
            mock.start_allowed.acquire().await.unwrap().forget();
            mock.awaiting_activity.store(true, Ordering::SeqCst);
            mock.control.lock().unwrap().status = "starting";
            (StatusCode::ACCEPTED, Json(instance("starting"))).into_response()
        }
        (&Method::POST, "/v1/instances/test-instance/sleep") => {
            mock.event("sleep");
            let mut control = mock.control.lock().unwrap();
            assert_eq!(control.status, "running");
            control.status = "sleeping";
            (StatusCode::ACCEPTED, Json(instance("sleeping"))).into_response()
        }
        (&Method::POST, "/v1/instances/test-instance/resize") => {
            if mock.fail_resize.load(Ordering::SeqCst) {
                mock.event("resize-failed");
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": {"code": "capacity_unavailable", "message": "no host room"}})),
                )
                    .into_response();
            }
            let body = to_bytes(req.into_body(), 4096).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"disk": 20})
            );
            mock.event("resize");
            let mut control = mock.control.lock().unwrap();
            assert_eq!(control.status, "running");
            control.status = "updating";
            (StatusCode::ACCEPTED, Json(instance("updating"))).into_response()
        }
        _ => panic!("unexpected Agent37 request: {} {path}", req.method()),
    }
}

fn snapshot() -> Value {
    json!({"snapshotSequence": 42, "projects": [], "threads": []})
}

async fn upstream_http(State(mock): State<Arc<Mock>>, req: Request) -> Response {
    assert_eq!(req.method(), Method::GET);
    edge(req.headers());
    assert_eq!(mock.control.lock().unwrap().status, "running");
    match req.uri().path() {
        "/.well-known/t3/environment" => Json(json!({"environmentId": "fixture"})).into_response(),
        "/api/auth/session" => {
            bearer(req.headers(), UPSTREAM);
            Json(json!({"scopes": ["rpc"]})).into_response()
        }
        SHELL => {
            bearer(req.headers(), UPSTREAM);
            mock.event("shell");
            Json(snapshot()).into_response()
        }
        path => panic!("unexpected upstream HTTP request: {path}"),
    }
}

fn success(id: &Value, value: Value) -> Value {
    json!({"_tag": "Exit", "requestId": id, "exit": {"_tag": "Success", "value": value}})
}

async fn upstream_ws(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    edge(&headers);
    bearer(&headers, UPSTREAM);
    assert_eq!(mock.control.lock().unwrap().status, "running");
    mock.event("ws-upgrade");
    upgrade.on_upgrade(move |mut socket| async move {
        while let Some(Ok(message)) = socket.next().await {
            let UpMessage::Text(text) = message else { continue };
            let req: Value = serde_json::from_str(text.as_str()).unwrap();
            if req["_tag"] == "Ping" {
                socket.send(UpMessage::text(json!({"_tag": "Pong"}).to_string())).await.unwrap();
                continue;
            }
            let id = &req["id"];
            let answer = match req["tag"].as_str().unwrap() {
                "subscribeServerConfig" => json!({
                    "_tag": "Chunk", "requestId": id,
                    "values": [{"config": {"directEndpoints": ["https://private.invalid"], "fixture": true}}]
                }),
                "orchestration.dispatchCommand" => {
                    mock.commands.lock().unwrap().push(req["payload"].clone());
                    mock.event("dispatch");
                    success(id, json!({"sequence": 43}))
                }
                "server.probe" => {
                    mock.event("upstream-probe");
                    success(id, json!({"upstream": true}))
                }
                tag => panic!("unexpected upstream RPC: {tag}"),
            };
            socket.send(UpMessage::text(answer.to_string())).await.unwrap();
        }
    })
}

async fn watchdog(State(mock): State<Arc<Mock>>, req: Request) -> Response {
    edge(req.headers());
    bearer(req.headers(), WATCHDOG);
    assert_eq!(mock.control.lock().unwrap().status, "running");
    let (phase, busy) = match (req.method(), req.uri().path(), req.uri().query()) {
        (&Method::GET, "/t3boat-status", None) => ("active", false),
        (&Method::GET, "/t3boat-status", Some("fresh")) => {
            mock.event("fresh");
            // A resumed run defers the CLI upgrade until the client announces activity.
            ("active", mock.awaiting_activity.load(Ordering::SeqCst))
        }
        (&Method::POST, "/activity", None) => {
            let body = to_bytes(req.into_body(), 4096).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"resumed": true})
            );
            mock.awaiting_activity.store(false, Ordering::SeqCst);
            mock.event("activity");
            ("active", false)
        }
        (&Method::POST, "/drain", None) => {
            mock.event("drain");
            ("draining", false)
        }
        (&Method::POST, "/sleep-ack", None) => {
            let body = to_bytes(req.into_body(), 4096).await.unwrap();
            let expected = if mock.null_manifest.load(Ordering::SeqCst) {
                json!({"sequence": Value::Null})
            } else {
                json!({"sequence": 42})
            };
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
            mock.event("sleep-ack");
            if mock.reject_sleep_ack.load(Ordering::SeqCst) {
                ("active", false)
            } else {
                ("readyToSleep", false)
            }
        }
        _ => panic!(
            "unexpected watchdog request: {} {}",
            req.method(),
            req.uri()
        ),
    };
    let manifest = if phase != "active" && !mock.null_manifest.load(Ordering::SeqCst) {
        Some(json!({"sequence": 42, "threads": {}}))
    } else {
        None
    };
    Json(json!({
        "phase": phase, "busy": busy, "workingThreads": u32::from(busy), "workingSubagents": 0,
        "cpuBusy": false, "agentsUnknown": false, "keepAwakeSecs": 0, "keepAwakeForever": false,
        "idleSecs": 0, "sleepAfterSecs": 60, "readyToSleep": phase == "readyToSleep",
        "manifest": manifest
    }))
    .into_response()
}

struct Fixture {
    tasks: JoinSet<()>,
    dir: PathBuf,
    mock: Arc<Mock>,
    machine: Arc<MachineRuntime>,
    http: reqwest::Client,
    base: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.tasks.abort_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start_mock(tasks: &mut JoinSet<()>, router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tasks.spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
}

impl Fixture {
    async fn new(status: &'static str) -> Self {
        let mut tasks = JoinSet::new();
        let mock = Arc::new(Mock {
            control: Mutex::new(Control {
                status,
                start_attempts: 0,
            }),
            events: Mutex::default(),
            commands: Mutex::default(),
            start_allowed: Semaphore::new(0),
            awaiting_activity: AtomicBool::new(false),
            null_manifest: AtomicBool::new(false),
            reject_sleep_ack: AtomicBool::new(false),
            fail_resize: AtomicBool::new(false),
        });
        let api = start_mock(
            &mut tasks,
            Router::new().fallback(control).with_state(mock.clone()),
        )
        .await;
        let upstream = start_mock(
            &mut tasks,
            Router::new()
                .route("/ws", get(upstream_ws))
                .fallback(upstream_http)
                .with_state(mock.clone()),
        )
        .await;
        let watchdog = start_mock(
            &mut tasks,
            Router::new().fallback(watchdog).with_state(mock.clone()),
        )
        .await;
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        let dir =
            std::env::temp_dir().join(format!("t3boat-agent37-{}", proxy::auth::random_token()));
        std::fs::create_dir(&dir).unwrap();
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(LIMIT)
            .build()
            .unwrap();
        let shared = Arc::new(Shared {
            agent37: Agent37::with_endpoints(EDGE_KEY.into(), api.clone(), api, upstream.clone())
                .unwrap(),
            t3: T3Desktop::at(dir.join("t3")),
            settings: RwLock::new(Settings::default()),
            events: mpsc::unbounded_channel().0,
            http: http.clone(),
        });
        let record: Machine = serde_json::from_value(json!({
            "id": "test-instance", "sandboxId": "test-instance", "name": "Fixture",
            "size": "medium", "diskGb": 12, "upstreamUrl": upstream, "upstreamToken": UPSTREAM,
            "watchdogUrl": watchdog, "watchdogSecret": WATCHDOG, "localPort": address.port(),
            "t3Version": "fixture", "provisionRevision": "fixture", "environmentId": "fixture",
            "clientTokens": [], "linkTokens": [], "createdAt": 0
        }))
        .unwrap();
        let machine = Arc::new(MachineRuntime::new(shared, record, dir.clone()));
        let fixture = Self {
            tasks,
            dir,
            mock,
            machine,
            http,
            base: format!("http://{address}"),
        };
        fixture.machine.refresh_power().await.unwrap();
        drop(reserved);
        let mut fixture = fixture;
        let machine = fixture.machine.clone();
        fixture
            .tasks
            .spawn(async move { proxy::serve(machine).await.unwrap() });
        timeout(LIMIT, async {
            loop {
                if TcpStream::connect(address).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("proxy did not bind");
        fixture
    }

    async fn pair(&self) -> String {
        let code = self.machine.auth.new_pairing_code();
        let send = || {
            self.http
                .post(format!("{}/oauth/token", self.base))
                .form(&[("subject_token", &code)])
        };
        let body: Value = send()
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            send().send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        body["access_token"].as_str().unwrap().to_owned()
    }

    async fn ws(&self, token: &str) -> Socket {
        let ticket: Value = self
            .http
            .post(format!("{}/api/auth/websocket-ticket/rpc", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let url = format!(
            "{}/ws?wsTicket={}",
            self.base.replacen("http:", "ws:", 1),
            ticket["ticket"].as_str().unwrap()
        );
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("x-agent37-key", "fake-key".parse().unwrap());
        timeout(LIMIT, connect_async(req)).await.unwrap().unwrap().0
    }

    async fn wait_running(&self) {
        let mut power = self.machine.subscribe_power();
        timeout(LIMIT, power.wait_for(|p| *p == Power::Running))
            .await
            .expect("machine did not wake")
            .unwrap();
    }
}

async fn rpc(socket: &mut Socket, id: &str, tag: &str, payload: Value) {
    let req = json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []});
    socket.send(Message::text(req.to_string())).await.unwrap();
}

async fn frame(socket: &mut Socket, limit: Duration) -> Message {
    timeout(limit, socket.next())
        .await
        .expect("timed out waiting for proxy WebSocket frame")
        .expect("proxy WebSocket ended")
        .unwrap()
}

async fn reply(socket: &mut Socket, limit: Duration) -> Value {
    serde_json::from_str(frame(socket, limit).await.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn pairing_injects_edge_credentials_and_strips_direct_endpoints() {
    let f = Fixture::new("running").await;
    f.machine.prime().await.unwrap();
    let token = f.pair().await;
    assert_eq!(f.machine.record().client_tokens, [token.clone()]);

    let shell: Value = f
        .http
        .get(format!("{}{SHELL}", f.base))
        .bearer_auth(&token)
        .header("X-Agent37-Key", "fake-key")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shell, snapshot());

    let mut ws = f.ws(&token).await;
    rpc(&mut ws, "config", "subscribeServerConfig", json!({})).await;
    assert_eq!(
        reply(&mut ws, LIMIT).await,
        json!({
            "_tag": "Chunk", "requestId": "config", "values": [{"config": {"fixture": true}}]
        })
    );
    assert_eq!(f.mock.events(), ["running", "shell", "ws-upgrade"]);
}

#[tokio::test]
async fn sleep_keeps_reads_local_and_dispatch_wakes_with_checkpoint_retries() {
    let f = Fixture::new("running").await;
    let token = f.pair().await;
    timeout(LIMIT, f.machine.sleep_now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.machine.power(), Power::Stopped);
    let offline_events = ["running", "drain", "shell", "sleep-ack", "sleep"];
    assert_eq!(f.mock.events(), offline_events);

    let shell: Value = f
        .http
        .get(format!("{}{SHELL}", f.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shell, snapshot());

    let mut ws = f.ws(&token).await;
    let ping = bytes::Bytes::from_static(b"offline");
    ws.send(Message::Ping(ping.clone())).await.unwrap();
    assert_eq!(frame(&mut ws, LOCAL_REPLY).await, Message::Pong(ping));
    ws.send(Message::text(r#"{"_tag":"Ping"}"#)).await.unwrap();
    assert_eq!(reply(&mut ws, LOCAL_REPLY).await, json!({"_tag": "Pong"}));
    rpc(&mut ws, "probe", "server.probe", json!({})).await;
    assert_eq!(
        reply(&mut ws, LOCAL_REPLY).await,
        success(&json!("probe"), json!({}))
    );
    // A cached subscription makes the client reconnect to resubscribe after the wake.
    rpc(
        &mut ws,
        "shell",
        "orchestration.subscribeShell",
        json!({"afterSequence": 42}),
    )
    .await;
    assert_eq!(
        reply(&mut ws, LOCAL_REPLY).await,
        json!({
            "_tag": "Chunk", "requestId": "shell", "values": [{"kind": "synchronized"}]
        })
    );
    assert_eq!(f.mock.events(), offline_events);

    let command =
        json!({"type": "message.dispatch", "commandId": "send-1", "threadId": "thread-1"});
    rpc(
        &mut ws,
        "send",
        "orchestration.dispatchCommand",
        command.clone(),
    )
    .await;
    assert_eq!(
        reply(&mut ws, LOCAL_REPLY).await,
        success(&json!("send"), json!({"sequence": 42}))
    );
    assert!(f.mock.commands.lock().unwrap().is_empty());
    f.mock.start_allowed.add_permits(1);
    let Message::Close(Some(close)) = frame(&mut ws, LIMIT).await else {
        panic!("expected a reconnect close frame after the wake");
    };
    assert_eq!(close.code, CloseCode::Restart);
    assert_eq!(*f.mock.commands.lock().unwrap(), [command]);
    assert_eq!(
        &f.mock.events()[offline_events.len()..][..8],
        [
            "start",
            "start",
            "start",
            "starting",
            "running",
            "fresh",
            "ws-upgrade",
            "dispatch"
        ]
    );
    assert_eq!(f.machine.power(), Power::Running);
}

#[tokio::test]
async fn sleeping_link_handshake_replies_before_agent37_finishes_waking() {
    let f = Fixture::new("sleeping").await;
    f.machine.update_record(|r| {
        r.link_tokens.push(LinkToken {
            token: LINK.into(),
            expires_at: chrono::Utc::now().timestamp() + 600,
        })
    });
    let post = || f.http.post(format!("{}/mcp", f.base)).bearer_auth(LINK);
    timeout(LOCAL_REPLY, async {
        let initialized = post().json(&json!({
            "jsonrpc": "2.0", "id": 13, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}
        })).send().await.unwrap();
        assert_eq!(initialized.status(), StatusCode::OK);
        let session = initialized.headers()["mcp-session-id"].clone();
        assert!(!session.is_empty());
        assert_eq!(initialized.json::<Value>().await.unwrap(), json!({
            "jsonrpc": "2.0", "id": 13, "result": {
                "protocolVersion": "2025-03-26", "capabilities": {},
                "serverInfo": {"name": "t3agent37", "version": env!("CARGO_PKG_VERSION")}
            }
        }));
        let ack = post().header("mcp-session-id", session)
            .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .send().await.unwrap();
        assert_eq!(ack.status(), StatusCode::ACCEPTED);
    }).await.expect("sleeping MCP handshake exceeded one second");

    f.mock.start_allowed.add_permits(1);
    f.wait_running().await;
    assert_eq!(
        &f.mock.events()[..6],
        ["start", "start", "start", "starting", "running", "fresh"]
    );
}

#[tokio::test]
async fn disk_resize_rejects_shrinking_and_wakes_before_growing() {
    let f = Fixture::new("sleeping").await;
    let error = f.machine.resize(None, Some(11)).await.unwrap_err();
    assert!(error.to_string().contains("can only grow"));
    assert_eq!(f.machine.power(), Power::Stopped);
    assert_eq!(f.machine.record().disk_gb, 12);
    assert!(f.mock.events().is_empty());

    f.mock.start_allowed.add_permits(1);
    timeout(LIMIT, f.machine.resize(None, Some(20)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.machine.power(), Power::Running);
    assert_eq!(f.machine.record().disk_gb, 20);
    assert_eq!(
        f.mock.events(),
        [
            "start", "start", "start", "starting", "running", "fresh", "activity", "fresh",
            "resize", "updating", "running", "activity"
        ]
    );
}

#[tokio::test]
async fn control_plane_billing_list_and_delete() {
    let f = Fixture::new("sleeping").await;
    let api = &f.machine.shared.agent37;
    assert_eq!(api.billing().await.unwrap().balance_dollars, 12.50);
    let instances = api.list().await.unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].id, f.machine.sandbox_id());
    api.delete(&f.machine.sandbox_id()).await.unwrap();
    assert!(api.list().await.unwrap().is_empty());
    assert_eq!(f.machine.refresh_power().await.unwrap(), Power::Missing);
}

#[tokio::test]
async fn null_manifest_sleep_ack_aborts_when_watchdog_returns_active() {
    let f = Fixture::new("running").await;
    f.mock.null_manifest.store(true, Ordering::SeqCst);
    f.mock.reject_sleep_ack.store(true, Ordering::SeqCst);
    f.mock.events.lock().unwrap().clear();

    // Watchdog returns draining with manifest=null, then reverts to active on /sleep-ack {sequence: null}.
    f.machine.sleep_now().await.unwrap();
    assert_eq!(f.machine.power(), Power::Running);
    assert_eq!(f.mock.events(), ["drain", "sleep-ack"]);

    // Once watchdog accepts /sleep-ack {sequence: null}, Agent37 /sleep runs.
    f.mock.reject_sleep_ack.store(false, Ordering::SeqCst);
    f.machine.sleep_now().await.unwrap();
    assert_eq!(f.machine.power(), Power::Stopped);
    assert_eq!(f.mock.events(), ["drain", "sleep-ack", "drain", "sleep-ack", "sleep"]);
}

#[tokio::test]
async fn resize_failure_recovers_power_from_starting() {
    let f = Fixture::new("running").await;
    f.mock.fail_resize.store(true, Ordering::SeqCst);
    f.mock.events.lock().unwrap().clear();

    let err = f.machine.resize(None, Some(20)).await.unwrap_err();
    assert!(format!("{err:#}").contains("capacity_unavailable"), "unexpected error: {err:#}");
    // Power must recover back to Running instead of staying stuck in Starting.
    assert_eq!(f.machine.power(), Power::Running);
    assert_eq!(f.machine.record().disk_gb, 12);
}

/// Live end-to-end test against real Agent37 using the locally saved API key
/// (`~/.local/share/t3agent37/agent37-key` or `AGENT37_API_KEY`).
/// Run explicitly with: `cargo test --no-default-features --test agent37_integration live_agent37_roundtrip -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn live_agent37_roundtrip() {
    use t3boat_lib::agent37::{InstanceType, Region};
    use t3boat_lib::machine::provision;

    let data_dir = dirs::data_dir().unwrap().join("t3agent37");
    let a37 = Agent37::locate(Some(&data_dir)).expect("Agent37 API key not found");
    let t3 = T3Desktop::locate().expect("T3 CLI not found");
    let t3_version = t3.version().await.expect("t3 --version failed");
    println!("[live] local T3 version: {t3_version}");

    // 1. Billing
    let billing = a37.billing().await.expect("GET /v1/billing failed");
    println!("[live] billing: {billing:?}");
    assert!(billing.balance_dollars > 0.0, "expected positive wallet balance");

    // 2. Create tiny default container (2 vCPU / 4 GB / 4 GB disk) + provision T3 + supervisor + watchdog
    let created_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let id_capture = created_id.clone();
    let dir = std::env::temp_dir().join(format!("t3agent37-live-{}", proxy::auth::random_token()));
    std::fs::create_dir_all(&dir).unwrap();

    let test_result: anyhow::Result<()> = async {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0")?;
        let local_port = reserved.local_addr()?.port();
        let record = provision::create(
            &a37,
            "live-test",
            "small",
            InstanceType::Default,
            Region::Us,
            Some(4),
            t3boat_lib::agent37::DEFAULT_TEMPLATE,
            None,
            &t3_version,
            local_port,
            |id| {
                println!("[live] created Agent37 instance: {id}");
                *id_capture.lock().unwrap() = Some(id.to_owned());
            },
            |step| println!("[live] step: {step}"),
        )
        .await?;
        println!(
            "[live] provisioned! env_id={} upstream={} watchdog={}",
            record.environment_id, record.upstream_url, record.watchdog_url
        );

        // 3. Verify unauthenticated edge access is rejected (401)
        let plain_http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let unauth = plain_http
            .get(format!("{}/.well-known/t3/environment", record.upstream_url))
            .send()
            .await?;
        println!("[live] unauthenticated edge status: {}", unauth.status());
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        // 4. Inspect sandbox runtime processes & RSS memory
        let ps = a37
            .exec_ok(&record.sandbox_id, "whoami; node -v; ps -eo pid,rss,comm,args --sort=-rss | head -n 12", 30)
            .await?;
        println!("[live] sandbox processes:\n{ps}");

        // 5. Start real localhost proxy and prime
        drop(reserved);
        let shared = Arc::new(Shared {
            agent37: a37.clone(),
            t3,
            settings: RwLock::new(Settings::default()),
            events: mpsc::unbounded_channel().0,
            http: plain_http.clone(),
        });
        let machine = Arc::new(MachineRuntime::new(shared, record.clone(), dir.clone()));
        machine.refresh_power().await?;
        assert_eq!(machine.power(), Power::Running);
        machine.prime().await?;

        let proxy_machine = machine.clone();
        let proxy_task = tokio::spawn(async move {
            let _ = proxy::serve(proxy_machine).await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let base = format!("http://127.0.0.1:{local_port}");

        // 6. Pair through localhost proxy & open RPC WS
        let code = machine.auth.new_pairing_code();
        let tok: Value = plain_http
            .post(format!("{base}/oauth/token"))
            .form(&[("subject_token", code.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let client_token = tok["access_token"].as_str().unwrap().to_owned();
        let ticket: Value = plain_http
            .post(format!("{base}/api/auth/websocket-ticket/rpc"))
            .bearer_auth(&client_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let ws_url = format!("ws://127.0.0.1:{local_port}/ws?wsTicket={}", ticket["ticket"].as_str().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        ws.send(tokio_tungstenite::tungstenite::Message::text(
            json!({"_tag": "Request", "id": 1, "tag": "subscribeServerConfig", "payload": {}, "headers": []}).to_string(),
        ))
        .await?;
        let first = tokio::time::timeout(Duration::from_secs(15), ws.next()).await?.unwrap()?;
        let cfg: Value = serde_json::from_str(first.to_text()?)?;
        println!("[live] subscribeServerConfig chunk received; has directEndpoints={}", cfg["values"][0]["config"].get("directEndpoints").is_some());
        assert!(cfg["values"][0]["config"].get("directEndpoints").is_none());
        let _ = ws.close(None).await;

        // 7. Proxy-driven CRIU sleep
        let sleep_start = std::time::Instant::now();
        machine.sleep_now().await?;
        println!("[live] sleep_now completed in {:?}, power={:?}", sleep_start.elapsed(), machine.power());
        assert_eq!(machine.power(), Power::Stopped);
        let inst_asleep = a37.get(&record.sandbox_id).await?;
        println!("[live] Agent37 instance status after sleep: {} (slept_at={:?})", inst_asleep.status, inst_asleep.slept_at);
        assert!(inst_asleep.is_asleep_or_stopped());

        // 8. Offline reads through proxy while asleep (must NOT wake Agent37!)
        let desc = plain_http.get(format!("{base}/.well-known/t3/environment")).send().await?;
        assert_eq!(desc.status(), StatusCode::OK);
        let shell = plain_http
            .get(format!("{base}/api/orchestration/shell"))
            .bearer_auth(&client_token)
            .header("x-t3-orchestration-protocol", "2")
            .send()
            .await?;
        println!("[live] offline shell status while asleep: {}", shell.status());
        assert_eq!(shell.status(), StatusCode::OK);
        let still_asleep = a37.get(&record.sandbox_id).await?;
        assert!(still_asleep.is_asleep_or_stopped(), "offline read unexpectedly woke instance!");

        // 9. Wake from CRIU checkpoint
        let wake_start = std::time::Instant::now();
        machine.ensure_awake().await?;
        println!("[live] CRIU wake completed in {:?}, power={:?}", wake_start.elapsed(), machine.power());
        assert_eq!(machine.power(), Power::Running);

        proxy_task.abort();
        Ok(())
    }
    .await;

    // 10. ALWAYS delete test instance so nothing leaks!
    if let Some(id) = created_id.lock().unwrap().clone() {
        println!("[live] cleaning up Agent37 instance {id}...");
        if let Err(e) = a37.delete(&id).await {
            eprintln!("[live] WARNING: delete failed: {e:#}");
        } else {
            let gone = a37.confirmed_gone(&id).await;
            println!("[live] instance {id} deleted; confirmed_gone={gone}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);

    test_result.expect("live_agent37_roundtrip failed");
}
