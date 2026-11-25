//! T3 Connect: T3's own way for other devices (a phone, a browser) to reach a machine with the
//! user's T3 account, through a tunnel the machine dials out.

use super::{provision, MachineRuntime, Power};
use crate::proxy::upstream;
use anyhow::{bail, Context, Result};
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const URL_TIMEOUT: Duration = Duration::from_secs(120);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffStatus {
    pub address: Option<String>,
    pub reachable: Option<bool>,
    pub setting_up: bool,
    // T3 may resume with a cached probe from before the machine slept.
    #[serde(skip)]
    pub(super) check_after: Option<chrono::DateTime<chrono::Utc>>,
}

impl MachineRuntime {
    pub async fn turn_on_connect(self: &Arc<Self>) -> Result<()> {
        let machine = self.clone();
        // Browser approval can outlive the UI request that started it.
        tokio::spawn(async move { machine.setup_connect().await }).await?
    }

    async fn setup_connect(&self) -> Result<()> {
        let _operation = self.connect_operation.lock().await;
        self.handoff.lock().unwrap().setting_up = true;
        self.shared.changed();
        let result = self.enable_connect().await;
        self.handoff.lock().unwrap().setting_up = false;
        self.shared.changed();
        result
    }

    pub fn handoff_status(&self) -> HandoffStatus {
        self.handoff.lock().unwrap().clone()
    }

    async fn enable_connect(&self) -> Result<()> {
        self.ensure_awake().await?;
        self.kept_awake(async {
            let state = self.connect_api(Method::GET, "link-state", None).await?;
            let already_configured = self.apply_connect_state(&state);
            if already_configured {
                if self.handoff_status().reachable != Some(true)
                    && let Err(error) = self.connect_api(Method::POST, "reachability", None).await
                {
                    tracing::debug!("requesting a T3 Connect probe for {}: {error:#}", self.id());
                }
            } else {
                self.ensure_idle_for_connect_restart().await?;
                self.install_connect_relay_client().await?;
                self.run_connect_setup().await?;
                {
                    let _transition = self.waking.lock().await;
                    // Approval can take minutes. A task that started meanwhile must finish first.
                    self.ensure_idle_for_connect_restart().await?;
                    provision::restart_supervised_t3(&self.shared.agent37, &self.sandbox_id()).await?;
                }
            }
            let state = self.wait_for_connect().await?;
            if !already_configured {
                let preferences = json!({
                    "publishAgentActivity": state["publishAgentActivity"].as_bool().unwrap_or(false),
                    "holdWebhooksWhileOffline": true,
                });
                if let Err(error) = self.connect_api(Method::POST, "preferences", Some(preferences)).await {
                    tracing::warn!("setting T3 Connect preferences for {}: {error:#}", self.id());
                }
            }
            Ok(())
        }).await
    }

    async fn ensure_idle_for_connect_restart(&self) -> Result<()> {
        if self.power() != Power::Running || self.is_stopping() {
            bail!("The machine is changing power state. Wait for it to be awake before setting up handoffs.");
        }
        let s = self.watchdog_get("/t3boat-status?fresh").await?;
        if s.busy {
            bail!("an agent is working; turn T3 Connect on once it finishes");
        }
        if s.waiting_threads > 0 {
            bail!("an agent is waiting on you there; answer it, then turn T3 Connect on");
        }
        Ok(())
    }

    async fn install_connect_relay_client(&self) -> Result<()> {
        // The CLI's `connect link` prompts if the connector is missing. Use T3's own
        // installer RPC with the existing VM session before starting headless approval.
        let headers = self.edge_headers();
        let url = self.upstream_url();
        let token = self.upstream_token();
        let status = upstream::call_with_headers(&url, &token, &headers, "cloud.getRelayClientStatus", json!({}))
            .await
            .context("checking the machine's T3 Connect relay client")?;
        if status["status"] == "available" {
            return Ok(());
        }
        let (installed, _) = upstream::subscribe_until_with_headers(
            &url,
            &token,
            &headers,
            "cloud.installRelayClient",
            json!({}),
            TUNNEL_TIMEOUT,
            |event| (event["type"] == "complete").then(|| event["status"]["status"] == "available"),
        )
        .await
        .context("installing the machine's T3 Connect relay client")?;
        if installed != Some(true) {
            bail!("T3 Code could not install the relay client on this machine.");
        }
        Ok(())
    }

    async fn wait_for_connect(&self) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + TUNNEL_TIMEOUT;
        let mut reason = None;
        loop {
            if self.power() != Power::Running || self.is_stopping() {
                bail!("The machine went offline during T3 Connect setup. Wake it and try again.");
            }
            // The server can answer link-state before registration and DNS are ready.
            match tokio::time::timeout_at(deadline, self.connect_api(Method::GET, "link-state", None)).await {
                Ok(Ok(state)) => {
                    self.apply_connect_state(&state);
                    let status = self.handoff_status();
                    if status.address.is_some() && status.reachable == Some(true) {
                        return Ok(state);
                    }
                    reason = state["reachability"]["reason"].as_str().map(str::to_owned);
                }
                _ if tokio::time::Instant::now() >= deadline => break,
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_secs(2))).await;
        }
        if self.record().connect {
            if let Some(reason) = reason {
                bail!("T3 Connect could not reach the machine. {reason}");
            }
            bail!("T3 Connect is configured, but the tunnel did not become reachable. Check T3 Connect for this machine in T3 Code → Settings → Sharing & access.");
        }
        bail!("T3 Code restarted but did not activate a T3 Connect tunnel. Try setting up handoffs again.");
    }

    pub(super) async fn refresh_connect(&self) {
        let Ok(_operation) = self.connect_operation.try_lock() else { return };
        match self.connect_api(Method::GET, "link-state", None).await {
            Ok(state) => {
                self.apply_connect_state(&state);
            }
            Err(e) => {
                if self.handoff.lock().unwrap().reachable.take().is_some() {
                    self.shared.changed();
                }
                tracing::warn!("reading T3 Connect state of {}: {e:#}", self.id());
            }
        }
    }

    fn apply_connect_state(&self, state: &Value) -> bool {
        let on = state["linked"] == true
            && state["managedTunnelActive"] == true
            && state["linkRevoked"] != true;
        let changed = {
            let mut handoff = self.handoff.lock().unwrap();
            let address = if on { state["tunnelHostname"].as_str().and_then(handoff_address) } else { None };
            let reachable = if on && self.power() == Power::Running {
                checked_reachability(&state["reachability"], handoff.check_after)
            } else {
                None
            };
            let changed = handoff.address != address || handoff.reachable != reachable;
            handoff.address = address;
            handoff.reachable = reachable;
            changed
        };
        if self.record().connect != on {
            self.update_record(|r| r.connect = on);
        } else if changed {
            self.shared.changed();
        }
        on
    }

    async fn run_connect_setup(&self) -> Result<()> {
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        let dir = a37
            .exec_ok(
                &id,
                r#"set -e
                    d=$(mktemp -d)
                    (
                        "$HOME/.local/bin/t3" connect link --headless &
                        child=$!
                        printf '%s\n' "$child" >"$d/pid"
                        status=0
                        wait "$child" || status=$?
                        printf '%s\n' "$status" >"$d/exit"
                    ) </dev/null >"$d/log" 2>&1 &
                    printf '%s\n' "$d"
                "#,
                30,
            )
            .await?;
        let dir = super::github::validate_tmp_dir(dir.lines().last().unwrap_or_default().trim())?;
        let result = self.finish_connect_setup(&dir).await;
        let cleanup = format!(r#"
            d='{dir}'
            if [ ! -f "$d/exit" ] && read -r pid <"$d/pid"; then
                case "$pid" in
                    ''|*[!0-9]*) ;;
                    *) case "$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null)" in
                        *" connect link --headless "*) kill -TERM "$pid" 2>/dev/null || true ;;
                    esac ;;
                esac
            fi
            rm -rf "$d"
        "#);
        let _ = a37.exec(&id, &cleanup, None, 30).await;
        result
    }

    async fn finish_connect_setup(&self, dir: &str) -> Result<()> {
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        let started = Instant::now();
        let mut opened = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let out = a37
                .exec(&id, &format!("cat '{dir}/exit' 2>/dev/null; echo ---; cat '{dir}/log'"), None, 30)
                .await?;
            let (exit, log) = out.stdout.split_once("---").unwrap_or_default();
            if !opened
                && let Some(raw) = log.split_whitespace().find(|w| w.starts_with("https://"))
                && let Ok(parsed) = url::Url::parse(raw)
                && parsed.scheme() == "https"
                && parsed.host_str().is_some_and(is_trusted_connect_host)
            {
                crate::app::open_url(parsed.as_str())?;
                opened = true;
            }
            match exit.trim() {
                "" => {}
                "0" => return Ok(()),
                _ => bail!("T3 Connect setup failed: {}", last_line(log)),
            }
            if !opened && started.elapsed() > URL_TIMEOUT {
                bail!("T3 Connect setup did not reach its sign-in: {}", last_line(log));
            }
            if started.elapsed() > SETUP_TIMEOUT {
                bail!("the T3 Connect sign-in was not finished in the browser");
            }
        }
    }

    async fn connect_api(&self, method: Method, route: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}/api/connect/{route}", self.upstream_url());
        let mut req = self
            .shared
            .http
            .request(method, url)
            .headers(self.edge_headers())
            .bearer_auth(self.upstream_token())
            .timeout(Duration::from_secs(15));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            bail!("T3 Connect {route}: HTTP {}", res.status());
        }
        Ok(res.json().await?)
    }
}

fn checked_reachability(probe: &Value, check_after: Option<chrono::DateTime<chrono::Utc>>) -> Option<bool> {
    if let Some(after) = check_after {
        let checked = chrono::DateTime::parse_from_rfc3339(probe["checkedAt"].as_str()?).ok()?;
        if checked <= after {
            return None;
        }
    }
    match probe["status"].as_str()? {
        "reachable" => Some(true),
        "unreachable" => Some(false),
        _ => None,
    }
}

fn handoff_address(hostname: &str) -> Option<String> {
    if !hostname.contains('.') || hostname.len() > 253 || !hostname.split('.').all(|label| {
        !label.is_empty() && label.len() <= 63
            && !label.starts_with('-') && !label.ends_with('-')
            && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return None;
    }
    match url::Host::parse(hostname).ok()? {
        url::Host::Domain(hostname) => Some(format!("https://{hostname}")),
        _ => None,
    }
}

fn is_trusted_connect_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    matches!(h.as_str(), "t3.codes" | "t3.chat" | "t3.gg")
        || h.ends_with(".t3.codes")
        || h.ends_with(".t3.chat")
        || h.ends_with(".t3.gg")
}

fn last_line(log: &str) -> &str {
    log.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_https_origin_only_from_a_hostname() {
        assert_eq!(handoff_address("vm.connect.example.com").as_deref(), Some("https://vm.connect.example.com"));
        for invalid in ["localhost", "127.0.0.1", "host.example:1234", "host.example/path", "user@host.example"] {
            assert_eq!(handoff_address(invalid), None);
        }
    }

    #[test]
    fn waits_for_a_new_tunnel_probe_after_sleep() {
        let probe = json!({ "status": "reachable", "checkedAt": "2026-11-24T10:00:00Z" });
        let after = "2026-11-24T10:01:00Z".parse().unwrap();
        assert_eq!(checked_reachability(&probe, None), Some(true));
        assert_eq!(checked_reachability(&probe, Some(after)), None);
        let probe = json!({ "status": "unreachable", "checkedAt": "2026-11-24T10:02:00Z" });
        assert_eq!(checked_reachability(&probe, Some(after)), Some(false));
        assert_eq!(checked_reachability(&Value::Null, None), None);
    }
}
