//! Lightweight localhost Web UI server embedded inside `t3agent37-daemon` (same port as MCP,
//! zero extra processes). Serves the compiled React frontend (`apps/desktop/dist`) plus:
//! - `GET /api/events` (SSE): pushes real-time `state` and `notify` events whenever machines are
//!   created, woken, slept, resized, or deleted (whether from the Web UI, CLI, or MCP agents).
//! - `POST /api/cmd/{cmd}`: JSON command endpoint matching `apps/desktop/src/types.ts`.
//!
//! Security: binds loopback only, checks `Host` against localhost/127.0.0.1/[::1], and requires
//! same-origin `Origin` on POST commands so external websites cannot trigger actions via CSRF.

use crate::agent37::{InstanceType, Region};
use crate::app::{App, Costs, MachineView, Setup};
use crate::machine::provision::{SizeInfo, SIZES};
use crate::machine::signin::Agent;
use crate::mcp;
use crate::store::Settings;
use crate::updater::{self, UpdateInfo, UpdateStatus};
use anyhow::Result;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::unfold;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub claude_code: Option<bool>,
    pub codex: Option<bool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub identifier: Option<String>,
    pub plan: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub setup: Setup,
    pub setup_busy: bool,
    pub sign_in_code: Option<String>,
    pub sign_in_url: Option<String>,
    pub account: Option<Account>,
    pub machines: Vec<MachineView>,
    pub costs: Costs,
    pub settings: Settings,
    pub sizes: &'static [SizeInfo],
    pub mcp: McpStatus,
    pub update: UpdateInfo,
}

#[derive(Clone, Debug)]
enum BroadcastEvent {
    State(String),
    Notify(String),
}

pub struct WebUi {
    app: Arc<App>,
    web_dir: Option<PathBuf>,
    sign_in_code: Mutex<Option<String>>,
    sign_in_url: Mutex<Option<String>>,
    mcp_status: Mutex<McpStatus>,
    update: Mutex<UpdateInfo>,
    tx: broadcast::Sender<BroadcastEvent>,
}

impl WebUi {
    pub fn new(app: Arc<App>, web_dir: Option<PathBuf>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        let ui = Arc::new(Self {
            app,
            web_dir,
            sign_in_code: Mutex::new(None),
            sign_in_url: Mutex::new(None),
            mcp_status: Mutex::new(McpStatus::default()),
            update: Mutex::new(UpdateInfo::default()),
            tx,
        });
        let bg = ui.clone();
        tokio::spawn(async move {
            bg.refresh_mcp().await;
            let _ = bg.refresh_update().await;
        });
        ui
    }

    pub fn snapshot(&self) -> UiState {
        let account = self.app.billing().map(|b| Account {
            identifier: Some(format!("Wallet ${:.2}", b.balance_dollars)),
            plan: b.tier.or_else(|| Some(format!("{} GB RAM limit", b.memory_limit_gb))),
        });
        UiState {
            setup: Setup::Ready,
            setup_busy: false,
            sign_in_code: self.sign_in_code.lock().unwrap().clone(),
            sign_in_url: self.sign_in_url.lock().unwrap().clone(),
            account,
            machines: self.app.views(),
            costs: self.app.costs(),
            settings: self.app.settings(),
            sizes: SIZES,
            mcp: self.mcp_status.lock().unwrap().clone(),
            update: self.update.lock().unwrap().clone(),
        }
    }

    pub async fn refresh_update(&self) -> Result<UpdateInfo> {
        {
            let mut u = self.update.lock().unwrap();
            u.status = UpdateStatus::Checking;
            u.error = None;
        }
        self.emit_state();
        match updater::check_latest().await {
            Ok(info) => {
                *self.update.lock().unwrap() = info.clone();
                self.emit_state();
                Ok(info)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let mut u = self.update.lock().unwrap();
                u.status = UpdateStatus::Error;
                u.error = Some(msg.clone());
                drop(u);
                self.emit_state();
                Err(e)
            }
        }
    }

    pub fn set_sign_in_code(&self, code: Option<String>) {
        *self.sign_in_code.lock().unwrap() = code;
        self.emit_state();
    }

    pub fn set_sign_in_url(&self, url: Option<String>) {
        *self.sign_in_url.lock().unwrap() = url;
        self.emit_state();
    }

    pub fn emit_state(&self) {
        if let Ok(json) = serde_json::to_string(&self.snapshot()) {
            let _ = self.tx.send(BroadcastEvent::State(json));
        }
    }

    pub fn emit_notify(&self, title: &str, body: &str) {
        let payload = json!({ "title": title, "body": body }).to_string();
        let _ = self.tx.send(BroadcastEvent::Notify(payload));
    }

    pub async fn refresh_mcp(&self) {
        let check = |client| async move {
            if mcp::installed(client).await { Some(mcp::registered(client).await) } else { None }
        };
        let (claude_code, codex) = tokio::join!(check(mcp::Client::ClaudeCode), check(mcp::Client::Codex));
        *self.mcp_status.lock().unwrap() = McpStatus { claude_code, codex };
        self.emit_state();
    }
}

/// Locates the compiled React frontend (`index.html` + `assets/`) via `T3AGENT37_WEB_DIR`,
/// `<exe_dir>/web`, or the repository's `apps/desktop/dist`.
pub fn locate_web_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("T3AGENT37_WEB_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("web"));
            candidates.push(dir.join("../../../dist"));
        }
    }
    if let Some(data) = dirs::data_dir() {
        candidates.push(data.join("t3agent37").join("web"));
    }
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../dist")));
    candidates.into_iter().find(|p| p.join("index.html").is_file())
}

pub fn router(ui: Arc<WebUi>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/events", get(events))
        .route("/api/cmd/{cmd}", post(command))
        .with_state(ui)
}

fn is_local_host(headers: &HeaderMap) -> bool {
    headers.get(HOST).and_then(|h| h.to_str().ok()).is_some_and(|host| {
        url::Url::parse(&format!("http://{host}"))
            .is_ok_and(|u| matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    })
}

/// Blocks cross-site POSTs: when a browser sends `Origin`, it must match the same loopback host
/// and port serving this Web UI.
fn same_local_origin(headers: &HeaderMap) -> bool {
    if !is_local_host(headers) {
        return false;
    }
    let Some(origin) = headers.get(ORIGIN).and_then(|h| h.to_str().ok()) else {
        return true;
    };
    let Ok(o) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host_hdr) = headers.get(HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Ok(h) = url::Url::parse(&format!("http://{host_hdr}")) else {
        return false;
    };
    o.scheme() == "http"
        && matches!(o.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        && o.port_or_known_default() == h.port_or_known_default()
}

async fn index(State(ui): State<Arc<WebUi>>, headers: HeaderMap) -> Response {
    if !is_local_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(dir) = &ui.web_dir else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "t3agent37 Web UI assets not found (run `bun --filter @t3boat/desktop build` or set T3AGENT37_WEB_DIR)",
        )
            .into_response();
    };
    let Ok(html) = std::fs::read_to_string(dir.join("index.html")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Mark the page so `apps/desktop/src/api.ts` connects to `/api/cmd` + `/api/events` instead of mock mode.
    let marker = r#"<script>window.__T3AGENT37_WEB__=true;</script></head>"#;
    let body = if html.contains("</head>") {
        html.replacen("</head>", marker, 1)
    } else {
        format!("<script>window.__T3AGENT37_WEB__=true;</script>{html}")
    };
    ([(CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

async fn asset(
    State(ui): State<Arc<WebUi>>,
    headers: HeaderMap,
    AxumPath(rel): AxumPath<String>,
) -> Response {
    if !is_local_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(dir) = &ui.web_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = safe_subpath(&dir.join("assets"), &rel) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = content_type_for(&path);
    ([(CONTENT_TYPE, mime), (CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response()
}

fn safe_subpath(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    Some(out)
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn events(State(ui): State<Arc<WebUi>>, headers: HeaderMap) -> Response {
    if !is_local_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let initial = serde_json::to_string(&ui.snapshot()).unwrap_or_else(|_| "{}".into());
    let rx = ui.tx.subscribe();
    let ui_clone = ui.clone();
    let s = unfold(
        (Some(initial), rx, ui_clone),
        |(mut init, mut rx, ui)| async move {
            if let Some(first) = init.take() {
                return Some((Ok::<_, Infallible>(SseEvent::default().event("state").data(first)), (None, rx, ui)));
            }
            loop {
                match rx.recv().await {
                    Ok(BroadcastEvent::State(data)) => {
                        return Some((Ok(SseEvent::default().event("state").data(data)), (None, rx, ui)));
                    }
                    Ok(BroadcastEvent::Notify(data)) => {
                        return Some((Ok(SseEvent::default().event("notify").data(data)), (None, rx, ui)));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Ok(data) = serde_json::to_string(&ui.snapshot()) {
                            return Some((Ok(SseEvent::default().event("state").data(data)), (None, rx, ui)));
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(s)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
        .into_response()
}

async fn command(
    State(ui): State<Arc<WebUi>>,
    headers: HeaderMap,
    AxumPath(cmd): AxumPath<String>,
    body: String,
) -> Response {
    if !same_local_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let args: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")).into_response(),
        }
    };
    match dispatch(&ui, &cmd, args).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

async fn dispatch(ui: &Arc<WebUi>, cmd: &str, args: Value) -> Result<Value> {
    let app = &ui.app;
    match cmd {
        "get_state" => Ok(serde_json::to_value(ui.snapshot())?),
        "create_machine" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                name: String,
                size: String,
                instance_type: Option<InstanceType>,
                region: Option<Region>,
                disk_gb: Option<u32>,
                #[serde(default)]
                projects: Vec<String>,
                #[serde(default)]
                setup_handoffs: bool,
            }
            let a: Args = serde_json::from_value(args)?;
            let m = app
                .create_machine(
                    &a.name,
                    &a.size,
                    a.instance_type.unwrap_or_default(),
                    a.region.unwrap_or_default(),
                    a.disk_gb,
                    crate::app::CreateOptions {
                        projects: a.projects,
                        setup_handoffs: a.setup_handoffs,
                    },
                )
                .await?;
            ui.emit_notify("Machine created", &format!("{} ({}) is up", m.record().name, m.id()));
            Ok(Value::Null)
        }
        "local_projects" => Ok(serde_json::to_value(app.local_projects().await?)?),
        "add_project" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                project_id: String,
            }
            let a: Args = serde_json::from_value(args)?;
            let m = app.machine(&a.id)?;
            app.add_project(&m, &a.project_id).await?;
            Ok(Value::Null)
        }
        "wake_machine" => {
            let id = str_arg(&args, "id")?;
            app.machine(id)?.ensure_awake().await?;
            Ok(Value::Null)
        }
        "sleep_machine" => {
            let id = str_arg(&args, "id")?;
            app.machine(id)?.sleep_now().await?;
            Ok(Value::Null)
        }
        "open_machine" => {
            let id = str_arg(&args, "id")?;
            let thread = args.get("thread").and_then(Value::as_str);
            app.open_in_t3(&*app.machine(id)?, thread)?;
            Ok(Value::Null)
        }
        "pair_machine" => {
            let id = str_arg(&args, "id")?;
            app.pair(&*app.machine(id)?)?;
            Ok(Value::Null)
        }
        "delete_machine" => {
            let id = str_arg(&args, "id")?;
            let discard = args.get("discard").and_then(Value::as_bool).unwrap_or(false);
            let m = app.machine(id)?;
            let name = m.record().name;
            app.delete_machine(&m, discard).await?;
            ui.emit_notify("Machine deleted", &format!("{name} was deleted"));
            Ok(Value::Null)
        }
        "resize_machine" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                size: Option<String>,
                disk_gb: Option<u32>,
            }
            let a: Args = serde_json::from_value(args)?;
            app.machine(&a.id)?.resize(a.size.as_deref(), a.disk_gb).await?;
            Ok(Value::Null)
        }
        "keep_awake" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                minutes: Option<u32>,
            }
            let a: Args = serde_json::from_value(args)?;
            app.machine(&a.id)?.keep_awake(a.minutes).await?;
            Ok(Value::Null)
        }
        "sign_in_agent" => {
            let id = str_arg(&args, "id")?;
            let agent = str_arg(&args, "agent")?;
            let a = Agent::from_name(agent)
                .ok_or_else(|| anyhow::anyhow!("{agent} has to be signed in from T3 Code's settings"))?;
            app.machine(id)?.sign_in(a).await?;
            Ok(Value::Null)
        }
        "sign_in_github" => {
            let id = str_arg(&args, "id")?;
            let token = args.get("token").and_then(Value::as_str);
            app.machine(id)?.sign_in_github(token).await?;
            Ok(Value::Null)
        }
        "turn_on_connect" => {
            let id = str_arg(&args, "id")?;
            app.machine(id)?.turn_on_connect().await?;
            Ok(Value::Null)
        }
        "latest_thread" => {
            let id = str_arg(&args, "id")?;
            Ok(serde_json::to_value(app.machine(id)?.latest_thread().await)?)
        }
        "get_machine_details" => {
            let id = str_arg(&args, "id")?;
            Ok(serde_json::to_value(app.details(id)?)?)
        }
        "add_mcp" => {
            #[derive(Deserialize)]
            struct Args {
                client: mcp::Client,
            }
            let a: Args = serde_json::from_value(args)?;
            let res = mcp::register(a.client).await;
            ui.refresh_mcp().await;
            res?;
            Ok(Value::Null)
        }
        "update_settings" => {
            #[derive(Deserialize)]
            struct Args {
                settings: Settings,
            }
            let a: Args = serde_json::from_value(args)?;
            app.update_settings(a.settings);
            ui.emit_state();
            Ok(Value::Null)
        }
        "setup_action" => {
            let action = str_arg(&args, "action")?;
            match action {
                "openApiKeys" => crate::app::open_url("https://www.agent37.com/dashboard/cloud/api-keys")?,
                "installT3" => crate::app::open_url("https://t3.codes")?,
                "recheck" => ui.emit_state(),
                other => anyhow::bail!("unknown setup action {other}"),
            }
            Ok(Value::Null)
        }
        "check_for_updates" => Ok(serde_json::to_value(ui.refresh_update().await?)?),
        "install_update" => {
            let info = ui.update.lock().unwrap().clone();
            let url = info.release_url.unwrap_or_else(updater::releases_page_url);
            crate::app::open_url(&url)?;
            Ok(Value::Null)
        }
        "open_release_notes" => {
            let info = ui.update.lock().unwrap().clone();
            let url = info.release_url.unwrap_or_else(updater::releases_page_url);
            crate::app::open_url(&url)?;
            Ok(Value::Null)
        }
        "quit_app" => {
            anyhow::bail!("t3agent37-daemon is managed in the background; stop t3agent37-supervisor to quit");
        }
        other => anyhow::bail!("unknown command {other}"),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string argument {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_subpath_blocks_traversal() {
        let root = Path::new("/srv/assets");
        assert_eq!(safe_subpath(root, "index.js"), Some(PathBuf::from("/srv/assets/index.js")));
        assert_eq!(safe_subpath(root, "../secret"), None);
        assert_eq!(safe_subpath(root, "/etc/passwd"), None);
    }

    #[test]
    fn origin_guard_allows_same_localhost_and_blocks_foreign_sites() {
        let mut ok = HeaderMap::new();
        ok.insert(HOST, "127.0.0.1:47730".parse().unwrap());
        ok.insert(ORIGIN, "http://127.0.0.1:47730".parse().unwrap());
        assert!(same_local_origin(&ok));

        let mut evil = HeaderMap::new();
        evil.insert(HOST, "127.0.0.1:47730".parse().unwrap());
        evil.insert(ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!same_local_origin(&evil));

        let mut wrong_port = HeaderMap::new();
        wrong_port.insert(HOST, "localhost:47730".parse().unwrap());
        wrong_port.insert(ORIGIN, "http://localhost:3000".parse().unwrap());
        assert!(!same_local_origin(&wrong_port));

        let mut rebind = HeaderMap::new();
        rebind.insert(HOST, "evil.example:47730".parse().unwrap());
        assert!(!same_local_origin(&rebind));
    }
}
