//! MCP server so coding agents on this computer (Claude Code, Codex) can drive t3agent37:
//! streamable HTTP on loopback, one JSON response per request. Local processes are trusted;
//! Host and Origin checks keep web pages out.

use crate::agent37::{InstanceType, Region};
use crate::app::App;
use crate::machine::signin::Agent;
use crate::process::command;
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::State;
use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;

pub const MCP_PORT: u16 = 47730;
const NAME: &str = "t3agent37";
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const WAIT: Duration = Duration::from_secs(40);

const INSTRUCTIONS: &str = "t3agent37 runs remote T3 Code machines on Agent37 for the user. \
Machines sleep on their own when no agent works (compute drops to $0; disk still bills $0.09/GB/mo \
until deleted) and wake when used. Address a machine by its name or id from list_machines. \
add_project puts one of the user's projects on a machine, as T3 Code's own \"Add to another machine\" \
does (run sign_in_github first for private repos). Each machine is also a linked environment of the \
user's T3 Code (t3_environment_links lists it): work there with T3's own tools and that environmentId, \
e.g. delegate_task or t3_thread_launch.";

pub fn url() -> String {
    format!("http://127.0.0.1:{MCP_PORT}/mcp")
}

pub fn app_router(app: Arc<App>) -> Router {
    router(app)
}

pub async fn serve(app: Arc<App>) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", MCP_PORT))
        .await
        .with_context(|| format!("listening on port {MCP_PORT}"))?;
    axum::serve(listener, router(app)).await?;
    Ok(())
}

trait Tools: Send + Sync + 'static {
    fn call(self: Arc<Self>, call: Call) -> impl Future<Output = Result<String>> + Send;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
enum Call {
    ListMachines {},
    CreateMachine {
        name: String,
        size: Option<String>,
        instance_type: Option<String>,
        region: Option<String>,
        disk_gb: Option<u32>,
        #[serde(default)]
        projects: Vec<String>,
    },
    ListProjects {},
    AddProject { machine: String, project: String },
    WakeMachine { machine: String },
    SleepMachine { machine: String },
    ResizeMachine {
        machine: String,
        size: Option<String>,
        disk_gb: Option<u32>,
    },
    KeepAwake { machine: String, minutes: u32 },
    SignInAgent { machine: String, agent: String },
    SignInGithub { machine: String, token: Option<String> },
    TurnOnConnect { machine: String },
    MachineDetails { machine: String },
    SetAutoSleep { machine: String, on: bool },
    OpenInT3 { machine: String },
    AddToT3 { machine: String },
    DeleteMachine {
        machine: String,
        #[serde(default)]
        discard: bool,
    },
}

fn router<T: Tools>(tools: Arc<T>) -> Router {
    Router::new().route("/mcp", post(handle::<T>)).with_state(tools)
}

async fn handle<T: Tools>(State(tools): State<Arc<T>>, headers: HeaderMap, body: String) -> Response {
    if !from_local_process(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return Json(reply(Value::Null, Err((-32700, "parse error".into())))).into_response();
    };
    match dispatch(tools, request).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn from_local_process(headers: &HeaderMap) -> bool {
    !headers.contains_key(ORIGIN)
        && headers.get(HOST).and_then(|h| h.to_str().ok()).is_some_and(|host| {
            url::Url::parse(&format!("http://{host}"))
                .is_ok_and(|u| matches!(u.host_str(), Some("localhost" | "127.0.0.1")))
        })
}

async fn dispatch<T: Tools>(tools: Arc<T>, request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => match parse_call(params) {
            Ok(call) => Ok(match tools.call(call).await {
                Ok(text) => content(text, false),
                Err(e) => content(format!("{e:#}"), true),
            }),
            Err(e) => Err((-32602, format!("invalid tool call: {e}"))),
        },
        method => Err((-32601, format!("unknown method {method}"))),
    };
    Some(reply(id, result))
}

fn reply(id: Value, result: Result<Value, (i64, String)>) -> Value {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn parse_call(mut params: Value) -> serde_json::Result<Call> {
    if let Some(p) = params.as_object_mut() {
        p.entry("arguments").or_insert_with(|| json!({}));
    }
    serde_json::from_value(params)
}

fn content(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn tool_list() -> Value {
    let machine = json!({ "type": "string", "description": "Machine name or id" });
    let sizes = crate::machine::provision::SIZES;
    let size = |description: &str| {
        let ids: Vec<&str> = sizes.iter().map(|s| s.id).collect();
        let specs: Vec<String> = sizes
            .iter()
            .map(|s| format!("{}: {}, ~${:.4}/h default compute, disk {}-{}GB", s.id, s.spec, s.hourly, s.min_disk, s.max_disk))
            .collect();
        json!({ "type": "string", "enum": ids, "description": format!("{description} ({})", specs.join("; ")) })
    };
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required },
        })
    };
    json!([
        tool(
            "list_machines",
            "Lists the user's t3agent37 machines with id, size, instanceType, region, diskGb, \
             idleMonthly ($/mo retained disk cost), diskUsage/diskWarning, state, signedOut agents, \
             githubSignedIn, T3 projects, plus Agent37 wallet balance, awake RAM vs memory limit, \
             and estimated spend.",
            json!({}),
            &[],
        ),
        tool(
            "create_machine",
            "Creates a new Agent37 remote machine with T3 Code on it. Defaults to `small` (2 vCPU / 4 GB), \
             type `default` (container, $4.40/mo compute + $0.09/GB/mo disk), region `us`, 12 GB disk. \
             Disk bills forever until deleted and can only grow. When ready, T3 Code asks the user to confirm.",
            json!({
                "name": { "type": "string", "description": "Unique machine name (lowercase letters, digits, dashes)" },
                "size": size("Defaults to small"),
                "instance_type": { "type": "string", "enum": ["default", "vm", "performance"], "description": "default (container 1x), vm (full kernel/Docker 2.5x), performance (dedicated cores 4x)" },
                "region": { "type": "string", "enum": ["us", "eu", "ap-south"], "description": "Defaults to us" },
                "disk_gb": { "type": "integer", "minimum": 2, "maximum": 80, "description": "Disk size in GB ($0.09/GB/mo even while asleep; grow-only)" },
                "projects": { "type": "array", "items": { "type": "string" }, "description": "Project ids from list_projects" },
            }),
            &["name"],
        ),
        tool(
            "list_projects",
            "The user's T3 Code projects on this computer that can be added to a machine (those with a git remote).",
            json!({}),
            &[],
        ),
        tool(
            "add_project",
            "Clones one of the user's T3 Code projects onto a machine and copies its env files. \
             For private repos, call sign_in_github on the machine first.",
            json!({
                "machine": machine,
                "project": { "type": "string", "description": "Project id, title or path from list_projects" },
            }),
            &["machine", "project"],
        ),
        tool(
            "wake_machine",
            "Wakes a sleeping Agent37 machine (with 409 checkpoint retry) and waits until T3 Code answers.",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "sleep_machine",
            "Captures state and puts a machine to sleep via Agent37 memory checkpointing (compute drops to $0; disk still bills). Refused while an agent works.",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "keep_awake",
            "Keeps a machine up for `minutes` (waking if asleep); 0 ends it now.",
            json!({
                "machine": machine,
                "minutes": { "type": "integer", "minimum": 0, "maximum": 1440, "description": "0 turns it off" },
            }),
            &["machine", "minutes"],
        ),
        tool(
            "resize_machine",
            "Resizes CPU/RAM (`size`) and/or grows `disk_gb` on Agent37. Wakes first if asleep and recreates the container (losing in-memory state while preserving disk). Disk can only grow, never shrink.",
            json!({
                "machine": machine,
                "size": size("Optional new CPU/RAM shape"),
                "disk_gb": { "type": "integer", "minimum": 2, "maximum": 80, "description": "Optional new disk size in GB (must be >= current diskGb)" },
            }),
            &["machine"],
        ),
        tool(
            "sign_in_agent",
            "Signs Claude or Codex in on a machine via Agent37 SSH port-forwarding and local browser OAuth.",
            json!({
                "machine": machine,
                "agent": { "type": "string", "enum": ["Claude", "Codex"] },
            }),
            &["machine", "agent"],
        ),
        tool(
            "sign_in_github",
            "Authenticates GitHub CLI (`gh`) and git on an Agent37 machine so private repos can be cloned/pushed. Omit `token` to open browser device login, or pass a fine-grained PAT.",
            json!({
                "machine": machine,
                "token": { "type": "string", "description": "Optional fine-grained GitHub PAT; omit for interactive browser device login" },
            }),
            &["machine"],
        ),
        tool(
            "turn_on_connect",
            "Turns on T3 Connect on a machine so phones/browsers reach it with the user's T3 account.",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "machine_details",
            "Detailed stats for one machine: compute/disk rates, disk usage, GitHub & agent login status, T3 version, and T3 Connect status. Never wakes it.",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "set_auto_sleep",
            "Auto sleep on (sleeps 3 minutes after agents finish) or off (stays up and bills compute until turned back on).",
            json!({ "machine": machine, "on": { "type": "boolean" } }),
            &["machine", "on"],
        ),
        tool(
            "open_in_t3",
            "Shows the machine in the T3 Code desktop app on its most recent thread (or opens pairing if unpaired).",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "add_to_t3",
            "Opens T3 Code's confirmation dialog to add this machine again.",
            json!({ "machine": machine }),
            &["machine"],
        ),
        tool(
            "delete_machine",
            "Permanently deletes an Agent37 machine and stops its recurring disk billing. Ask the user first.",
            json!({ "machine": machine, "discard": { "type": "boolean", "description": "Delete even while work runs" } }),
            &["machine"],
        ),
    ])
}

impl Call {
    fn pending(&self) -> String {
        match self {
            Self::CreateMachine { name, .. } => format!(
                "Creating {name} on Agent37. list_machines shows its progress. When ready, T3 Code asks the user to confirm."
            ),
            Self::SignInAgent { machine, agent } => format!(
                "The {agent} sign-in for {machine} is open in the browser; the user must click Authorize."
            ),
            Self::SignInGithub { machine, .. } => format!(
                "GitHub device sign-in for {machine} is open in the browser; check the notification for the one-time code."
            ),
            Self::AddProject { machine, project } => format!(
                "Still cloning {project} onto {machine}; list_machines shows progress."
            ),
            Self::TurnOnConnect { machine } => format!(
                "T3 Connect sign-in for {machine} is open in the browser; approve it there."
            ),
            _ => "Still running in the background; list_machines shows the machine's state.".into(),
        }
    }
}

impl Tools for App {
    async fn call(self: Arc<Self>, call: Call) -> Result<String> {
        let pending = call.pending();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Err(Err(e)) = tx.send(run(&self, call).await) {
                self.shared.notify(NAME, format!("{e:#}"));
            }
        });
        match tokio::time::timeout(WAIT, rx).await {
            Ok(result) => result?,
            Err(_) => Ok(pending),
        }
    }
}

async fn run(app: &Arc<App>, call: Call) -> Result<String> {
    Ok(match call {
        Call::ListMachines {} => json!({ "machines": app.views(), "costs": app.costs() }).to_string(),
        Call::CreateMachine { name, size, instance_type, region, disk_gb, projects } => {
            let ty = instance_type.as_deref().map(InstanceType::from_str).transpose()?.unwrap_or_default();
            let reg = region.as_deref().map(Region::from_str).transpose()?.unwrap_or_default();
            let options = crate::app::CreateOptions { projects, ..Default::default() };
            let m = app.create_machine(&name, size.as_deref().unwrap_or("small"), ty, reg, disk_gb, options).await?;
            format!(
                "Created {} ({}) on Agent37. T3 Code now asks the user to add it as a backend; tell them to confirm.",
                m.record().name,
                m.id()
            )
        }
        Call::ListProjects {} => serde_json::to_string(&app.local_projects().await?)?,
        Call::AddProject { machine, project } => {
            let m = app.machine(&machine)?;
            let p = app.add_project(&m, &project).await?;
            format!("{} is on {} now, in T3 Code.", p.title, m.record().name)
        }
        Call::WakeMachine { machine } => {
            let m = app.machine(&machine)?;
            m.ensure_awake().await?;
            format!("{} is awake.", m.record().name)
        }
        Call::SleepMachine { machine } => {
            let m = app.machine(&machine)?;
            m.sleep_now().await?;
            format!("{} is going to sleep.", m.record().name)
        }
        Call::KeepAwake { machine, minutes } => {
            let m = app.machine(&machine)?;
            let name = m.record().name;
            m.keep_awake(Some(minutes)).await?;
            match minutes {
                0 => format!("{name} sleeps as usual again."),
                n => format!("{name} stays awake for {n} minutes."),
            }
        }
        Call::ResizeMachine { machine, size, disk_gb } => {
            let m = app.machine(&machine)?;
            m.resize(size.as_deref(), disk_gb).await?;
            let r = m.record();
            format!("{} resized to {} ({} GB disk).", r.name, r.size, r.disk_gb)
        }
        Call::SignInAgent { machine, agent } => {
            let a = Agent::from_name(&agent).ok_or_else(|| anyhow!("agent must be Claude or Codex"))?;
            let m = app.machine(&machine)?;
            m.sign_in(a).await?;
            format!("{agent} is signed in on {}.", m.record().name)
        }
        Call::SignInGithub { machine, token } => {
            let m = app.machine(&machine)?;
            m.sign_in_github(token.as_deref()).await?;
            format!("GitHub is signed in on {}.", m.record().name)
        }
        Call::TurnOnConnect { machine } => {
            let m = app.machine(&machine)?;
            m.turn_on_connect().await?;
            format!("T3 Connect is on for {}.", m.record().name)
        }
        Call::MachineDetails { machine } => serde_json::to_string(&app.details(&machine)?)?,
        Call::SetAutoSleep { machine, on } => {
            let m = app.machine(&machine)?;
            m.keep_awake(if on { Some(0) } else { None }).await?;
            match on {
                true => format!("{} sleeps 3 minutes after its agents finish again.", m.record().name),
                false => format!("{} stays up until auto sleep is turned back on.", m.record().name),
            }
        }
        Call::OpenInT3 { machine } => {
            let m = app.machine(&machine)?;
            app.open_in_t3(&m, m.latest_thread().await.as_deref())?;
            format!("Showed {} in T3 Code.", m.record().name)
        }
        Call::AddToT3 { machine } => {
            let m = app.machine(&machine)?;
            app.pair(&m)?;
            format!("T3 Code now asks the user to add {}; tell them to confirm.", m.record().name)
        }
        Call::DeleteMachine { machine, discard } => {
            let m = app.machine(&machine)?;
            let record = m.record();
            app.delete_machine(&m, discard).await?;
            let name = record.name;
            if record.client_tokens.is_empty() {
                format!("Deleted {name}.")
            } else {
                format!("Deleted {name}. T3 Code still lists it until the user removes it there, under Settings → Environments.")
            }
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Client {
    ClaudeCode,
    Codex,
}

impl Client {
    fn cli(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    fn add_args(self) -> Vec<String> {
        let args: &[&str] = match self {
            Self::ClaudeCode => &["mcp", "add", "--transport", "http", "--scope", "user", NAME],
            Self::Codex => &["mcp", "add", NAME, "--url"],
        };
        args.iter().map(|a| a.to_string()).chain([url()]).collect()
    }
}

pub async fn installed(client: Client) -> bool {
    locate(client.cli()).await.is_some()
}

pub async fn registered(client: Client) -> bool {
    run_cli(client, &["mcp".into(), "get".into(), NAME.into()]).await.is_ok()
}

pub async fn register(client: Client) -> Result<()> {
    if registered(client).await {
        return Ok(());
    }
    run_cli(client, &client.add_args()).await
}

async fn run_cli(client: Client, args: &[String]) -> Result<()> {
    let cli = client.cli();
    let bin = locate(cli).await.ok_or_else(|| anyhow!("the {cli} CLI is not installed"))?;
    let dirs = bin.parent().map(PathBuf::from).into_iter();
    let search = login_path().await.cloned().or_else(|| std::env::var_os("PATH")).unwrap_or_default();
    let path = std::env::join_paths(dirs.chain(std::env::split_paths(&search)))?;
    let out = command(&bin).args(args).env("PATH", path).stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        bail!("{cli} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

async fn locate(cli: &str) -> Option<PathBuf> {
    if let Ok(bin) = which::which(cli) {
        return Some(bin);
    }
    which::which_in(cli, login_path().await, std::env::current_dir().ok()?).ok()
}

async fn login_path() -> Option<&'static OsString> {
    static PATH: OnceCell<Option<OsString>> = OnceCell::const_new();
    PATH.get_or_init(|| async {
        let shell = std::env::var("SHELL").ok()?;
        let out = command(shell)
            .args(["-lic", "echo \"$PATH\""])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(5), out).await.ok()?.ok()?;
        let path = String::from_utf8_lossy(&out.stdout).lines().last()?.trim().to_owned();
        (!path.is_empty()).then(|| path.into())
    })
    .await
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Tools for Echo {
        async fn call(self: Arc<Self>, call: Call) -> Result<String> {
            match call {
                Call::SleepMachine { .. } => bail!("an agent is working"),
                Call::ResizeMachine { size: Some(size), .. } => Ok(crate::machine::provision::size(&size)?.id.into()),
                call => Ok(format!("{call:?}")),
            }
        }
    }

    async fn rpc(method: &str, params: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        dispatch(Arc::new(Echo), request).await.unwrap()
    }

    #[tokio::test]
    async fn initialize_negotiates_the_version() {
        let r = rpc("initialize", json!({ "protocolVersion": "2025-03-26" })).await;
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "t3agent37");
        assert_eq!(r["result"]["capabilities"], json!({ "tools": {} }));
        let r = rpc("initialize", json!({ "protocolVersion": "1999-01-01" })).await;
        assert_eq!(r["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn lists_tools_and_rejects_unknown_methods() {
        let tools = rpc("tools/list", json!({})).await["result"]["tools"].clone();
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), 16);
        assert!(names.contains(&"sign_in_agent") && names.contains(&"sign_in_github") && names.contains(&"delete_machine"));
        assert_eq!(rpc("ping", json!({})).await["result"], json!({}));
        assert_eq!(rpc("resources/list", json!({})).await["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn calls_tools() {
        let r = rpc("tools/call", json!({ "name": "list_machines" })).await;
        assert_eq!(r["result"], content("ListMachines".into(), false));
        let r = rpc("tools/call", json!({ "name": "create_machine", "arguments": { "name": "dev" } })).await;
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("CreateMachine"));
        let r = rpc("tools/call", json!({ "name": "sleep_machine", "arguments": { "machine": "dev" } })).await;
        assert_eq!(r["result"], content("an agent is working".into(), true));
        let r = rpc("tools/call", json!({ "name": "resize_machine", "arguments": { "machine": "dev", "size": "huge" } })).await;
        assert!(r["result"]["isError"] == true);
        for bad in [
            json!({ "name": "format_disk", "arguments": { "machine": "dev" } }),
            json!({ "name": "wake_machine", "arguments": {} }),
        ] {
            assert_eq!(rpc("tools/call", bad).await["error"]["code"], -32602);
        }
    }

    async fn serve_echo() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(Arc::new(Echo))).await });
        format!("http://{addr}/mcp")
    }

    async fn post(headers: &[(&str, &str)], body: Value) -> (StatusCode, String) {
        let mut req = reqwest::Client::new().post(serve_echo().await).json(&body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = req.send().await.unwrap();
        (res.status(), res.text().await.unwrap())
    }

    #[tokio::test]
    async fn http_answers_requests_and_notifications() {
        let (status, body) = post(&[], json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["result"], json!({}));
        let (status, body) = post(&[], json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
        assert_eq!((status, body.as_str()), (StatusCode::ACCEPTED, ""));
        let ok = &[("host", "localhost:47730")];
        assert_eq!(post(ok, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn http_rejects_web_pages_and_get() {
        let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        for headers in [
            &[("host", "evil.example:47730")][..],
            &[("host", "localhost.evil.example")],
            &[("origin", "https://evil.example")],
            &[("origin", "http://localhost:3000"), ("host", "localhost:47730")],
            &[("origin", "null")],
        ] {
            assert_eq!(post(headers, ping.clone()).await.0, StatusCode::FORBIDDEN, "{headers:?}");
        }
        let res = reqwest::get(serve_echo().await).await.unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
