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
use std::time::{Duration, Instant};

const PROVISION_SH: &str = include_str!("provision.sh");
const WATCHDOG_MJS: &str = include_str!("watchdog.mjs");
const SUPERVISOR_MJS: &str = include_str!("../../../../../template/supervisor.mjs");
pub const T3_PORT: u16 = 3773;
pub const WATCHDOG_PORT: u16 = 3774;
const PROVISION_TIMEOUT: Duration = Duration::from_secs(600);
const PROVISION_POLL_INTERVAL: Duration = Duration::from_millis(1500);
const PROVISION_POLL_ERROR_BUDGET: Duration = Duration::from_secs(60);

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
    run_with_progress(agent37, instance_id, name, t3_version, watchdog_secret, mint_upstream_token, |_| {}).await
}

/// Runs `provision.sh` detached on the sandbox and polls its status via fast `exec` calls so a
/// slow install can never hit Cloudflare's 100s HTTP timeout on `sandbox.agent37.app`, transient
/// routing blips don't abort provisioning, and live step updates reach the UI.
pub async fn run_with_progress(
    agent37: &Agent37,
    instance_id: &str,
    name: &str,
    t3_version: &str,
    watchdog_secret: &str,
    mint_upstream_token: bool,
    progress: impl Fn(&str),
) -> Result<Provisioned> {
    run_with_progress_opts(
        agent37,
        instance_id,
        name,
        t3_version,
        watchdog_secret,
        mint_upstream_token,
        PROVISION_TIMEOUT,
        PROVISION_POLL_INTERVAL,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with_progress_opts(
    agent37: &Agent37,
    instance_id: &str,
    name: &str,
    t3_version: &str,
    watchdog_secret: &str,
    mint_upstream_token: bool,
    timeout: Duration,
    poll_interval: Duration,
    progress: impl Fn(&str),
) -> Result<Provisioned> {
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let token = random_token();
    let job_dir = format!("/tmp/t3agent37-prov-{token}");
    let remote = format!("/tmp/t3agent37-provision-{token}.sh");
    let v = shell_word(t3_version)?;
    let s = shell_word(watchdog_secret)?;
    let n = shell_word(name)?;
    let mint = u8::from(mint_upstream_token);

    let launcher = format!(
        "set -e\n\
         umask 077\n\
         job_dir='{job_dir}'\n\
         mkdir -p \"$job_dir\"\n\
         chmod 700 \"$job_dir\"\n\
         echo {p} | base64 -d > \"$job_dir/provision.sh\"\n\
         echo {w} | base64 -d > \"$job_dir/watchdog.mjs\"\n\
         echo {sup} | base64 -d > \"$job_dir/supervisor.mjs\"\n\
         printf '%s' '{s}' > \"$job_dir/secret\"\n\
         chmod 600 \"$job_dir/secret\"\n\
         cat > \"$job_dir/run.sh\" <<'EOF'\n\
         #!/usr/bin/env bash\n\
         set -u\n\
         job_dir=\"$1\"\n\
         shift\n\
         secret=\"$(cat \"$job_dir/secret\" 2>/dev/null || true)\"\n\
         rm -f \"$job_dir/secret\"\n\
         rc=0\n\
         T3BOAT_SECRET=\"$secret\" T3BOAT_STEP_FILE=\"$job_dir/step\" bash \"$job_dir/provision.sh\" \"$1\" \"$2\" \"$3\" \"$4\" >\"$job_dir/stdout\" 2>\"$job_dir/stderr\" || rc=$?\n\
         printf '%s\\n' \"$rc\" > \"$job_dir/exit_code.tmp\"\n\
         mv \"$job_dir/exit_code.tmp\" \"$job_dir/exit_code\"\n\
         EOF\n\
         chmod 700 \"$job_dir/run.sh\"\n\
         nohup bash \"$job_dir/run.sh\" \"$job_dir\" {v} {sleep} {mint} {n} </dev/null >/dev/null 2>&1 &\n\
         printf '%s\\n' \"$!\" > \"$job_dir/pid\"\n\
         rm -f \"$0\"\n",
        p = b64(PROVISION_SH),
        w = b64(WATCHDOG_MJS),
        sup = b64(SUPERVISOR_MJS),
        sleep = SLEEP_AFTER_SECS,
    );
    agent37.upload_small(instance_id, launcher.as_bytes(), &remote).await?;
    agent37.exec_ok(instance_id, &format!("bash '{remote}'"), 60).await?;

    let poll_script = format!(
        "d='{job_dir}'\n\
         emit_done() {{\n\
           printf 'DONE %s\\n' \"$(cat \"$d/exit_code\")\"\n\
           printf '---STDOUT---\\n'\n\
           cat \"$d/stdout\" 2>/dev/null || true\n\
           printf '\\n---STDERR---\\n'\n\
           tail -c 8192 \"$d/stderr\" 2>/dev/null || true\n\
           rm -rf \"$d\"\n\
         }}\n\
         if [ -f \"$d/exit_code\" ]; then\n\
           emit_done\n\
         else\n\
           pid=\"$(cat \"$d/pid\" 2>/dev/null || true)\"\n\
           step=\"$(cat \"$d/step\" 2>/dev/null || true)\"\n\
           if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then\n\
             printf 'RUNNING %s\\n' \"$step\"\n\
           elif [ -f \"$d/exit_code\" ]; then\n\
             emit_done\n\
           else\n\
             printf 'LOST\\n'\n\
             tail -c 8192 \"$d/stderr\" 2>/dev/null || true\n\
             rm -rf \"$d\"\n\
           fi\n\
         fi\n"
    );

    let started = Instant::now();
    let mut last_step = String::new();
    let mut first_poll_err: Option<Instant> = None;

    loop {
        if started.elapsed() > timeout {
            let _ = agent37.exec(instance_id, &format!("rm -rf '{job_dir}'"), None, 15).await;
            bail!("provisioning timed out after {}s", timeout.as_secs());
        }
        let poll_out = match agent37.exec_ok(instance_id, &poll_script, 30).await {
            Ok(out) => {
                first_poll_err = None;
                out
            }
            Err(e) => {
                let err_since = *first_poll_err.get_or_insert_with(Instant::now);
                if err_since.elapsed() >= PROVISION_POLL_ERROR_BUDGET {
                    return Err(e).context("polling provisioning progress on the machine");
                }
                tracing::warn!("transient poll error while provisioning {instance_id}: {e:#}");
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };

        match parse_poll_output(&poll_out)? {
            PollState::Running(step) => {
                if !step.is_empty() && step != last_step {
                    progress(&step);
                    last_step = step;
                }
                tokio::time::sleep(poll_interval).await;
            }
            PollState::Done { exit_code: 0, stdout, .. } => {
                let line = stdout
                    .lines()
                    .rev()
                    .find(|l| l.starts_with('{'))
                    .ok_or_else(|| anyhow!("provisioning printed no result"))?;
                return Ok(serde_json::from_str(line)?);
            }
            PollState::Done { exit_code, stdout, stderr } => {
                let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
                bail!("provisioning failed (exit {exit_code}): {detail}");
            }
            PollState::Lost(stderr) => {
                let detail = stderr.trim();
                if detail.is_empty() {
                    bail!("provisioning process exited unexpectedly without writing status");
                }
                bail!("provisioning process exited unexpectedly: {detail}");
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PollState {
    Running(String),
    Done { exit_code: i32, stdout: String, stderr: String },
    Lost(String),
}

fn parse_poll_output(out: &str) -> Result<PollState> {
    let trimmed = out.trim_start_matches(['\r', '\n']);
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let first = first.trim_end_matches('\r');
    if let Some(step) = first.strip_prefix("RUNNING") {
        return Ok(PollState::Running(step.trim().to_owned()));
    }
    if let Some(code_str) = first.strip_prefix("DONE ") {
        let exit_code: i32 = code_str
            .trim()
            .parse()
            .with_context(|| format!("invalid provisioning exit code: {code_str:?}"))?;
        let after_stdout = rest
            .strip_prefix("---STDOUT---\n")
            .or_else(|| rest.strip_prefix("---STDOUT---\r\n"))
            .unwrap_or(rest);
        let (stdout, stderr) = after_stdout
            .split_once("\n---STDERR---\n")
            .or_else(|| after_stdout.split_once("\r\n---STDERR---\r\n"))
            .unwrap_or((after_stdout, ""));
        return Ok(PollState::Done {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        });
    }
    if first == "LOST" {
        return Ok(PollState::Lost(rest.to_owned()));
    }
    bail!("unexpected provisioning poll output: {}", first);
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

/// Applies configuration changes to the same T3 server the Agent37 proxy uses.
pub(super) async fn restart_supervised_t3(agent37: &Agent37, instance_id: &str) -> Result<()> {
    let script = r#"
        set -e
        dir="$HOME/.t3agent37"
        read -r supervisor <"$dir/supervisor.pid"
        case "$supervisor" in
            ''|*[!0-9]*) echo "Invalid T3 supervisor PID" >&2; exit 1 ;;
        esac
        if [ "$supervisor" -le 1 ]; then
            echo "Invalid T3 supervisor PID" >&2
            exit 1
        fi
        case "$(tr '\0' ' ' <"/proc/$supervisor/cmdline" 2>/dev/null)" in
            *" $dir/supervisor.mjs "*|*" /usr/local/lib/t3agent37/supervisor.mjs "*) ;;
            *) echo "T3 supervisor is not running" >&2; exit 1 ;;
        esac
        kill -USR2 "$supervisor"
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
    let provisioned = run_with_progress(agent37, &id, name, t3_version, &secret, true, &progress).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn parse_poll_output_handles_running_done_and_lost() {
        assert_eq!(
            parse_poll_output("RUNNING Installing T3 Code\n").unwrap(),
            PollState::Running("Installing T3 Code".into())
        );
        assert_eq!(
            parse_poll_output("RUNNING \n").unwrap(),
            PollState::Running(String::new())
        );
        assert_eq!(
            parse_poll_output("DONE 0\n---STDOUT---\n{\"environmentId\":\"e1\",\"upstreamToken\":\"t1\"}\n---STDERR---\nwarn\n")
                .unwrap(),
            PollState::Done {
                exit_code: 0,
                stdout: "{\"environmentId\":\"e1\",\"upstreamToken\":\"t1\"}".into(),
                stderr: "warn\n".into(),
            }
        );
        assert_eq!(
            parse_poll_output("LOST\nboom\n").unwrap(),
            PollState::Lost("boom\n".into())
        );
    }

    #[tokio::test]
    async fn detached_provision_polls_steps_survives_transient_error_and_parses_result() {
        #[derive(Default)]
        struct MockState {
            calls: AtomicUsize,
        }
        let state = Arc::new(MockState::default());
        let app = Router::new()
            .route(
                "/v1/instances/{id}/exec",
                post(|State(s): State<Arc<MockState>>| async move {
                    match s.calls.fetch_add(1, Ordering::SeqCst) {
                        // 0: upload_small, 1: launch background runner
                        0 | 1 => (StatusCode::OK, Json(json!({"exit_code": 0, "stdout": "", "stderr": ""}))),
                        // 2: first poll -> installing T3
                        2 => (
                            StatusCode::OK,
                            Json(json!({"exit_code": 0, "stdout": "RUNNING Installing T3 Code\n", "stderr": ""})),
                        ),
                        // 3: transient 502 gateway blip during poll
                        3 => (StatusCode::BAD_GATEWAY, Json(json!({"error": "bad_gateway"}))),
                        // 4: next poll -> starting server
                        4 => (
                            StatusCode::OK,
                            Json(json!({"exit_code": 0, "stdout": "RUNNING Starting T3 server\n", "stderr": ""})),
                        ),
                        // 5: completed
                        _ => (
                            StatusCode::OK,
                            Json(json!({
                                "exit_code": 0,
                                "stdout": "DONE 0\n---STDOUT---\n{\"environmentId\":\"env-42\",\"upstreamToken\":\"tok-99\"}\n---STDERR---\n",
                                "stderr": ""
                            })),
                        ),
                    }
                }),
            )
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

        let steps = Mutex::new(Vec::<String>::new());
        let res = run_with_progress_opts(
            &client,
            "inst_1",
            "dev",
            "0.1.12",
            "secret123",
            true,
            Duration::from_secs(10),
            Duration::from_millis(10),
            |s| steps.lock().unwrap().push(s.to_owned()),
        )
        .await
        .unwrap();

        assert_eq!(res.environment_id, "env-42");
        assert_eq!(res.upstream_token.as_deref(), Some("tok-99"));
        assert_eq!(*steps.lock().unwrap(), vec!["Installing T3 Code", "Starting T3 server"]);
    }
}
