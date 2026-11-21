//! T3 Connect: T3's own way for other devices (a phone, a browser) to reach a machine with the
//! user's T3 account, through a tunnel the machine dials out.

use super::MachineRuntime;
use anyhow::{bail, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const URL_TIMEOUT: Duration = Duration::from_secs(120);

impl MachineRuntime {
    pub async fn turn_on_connect(&self) -> Result<()> {
        self.ensure_awake().await?;
        let s = self.watchdog_get("/t3boat-status?fresh").await?;
        if s.busy {
            bail!("an agent is working; turn T3 Connect on once it finishes");
        }
        if s.waiting_threads > 0 {
            bail!("an agent is waiting on you there; answer it, then turn T3 Connect on");
        }
        self.kept_awake(self.run_connect_setup()).await?;
        let state = self.connect_api(Method::GET, "link-state", None).await?;
        let preferences = json!({
            "publishAgentActivity": state["publishAgentActivity"].as_bool().unwrap_or(false),
            "holdWebhooksWhileOffline": true,
        });
        self.connect_api(Method::POST, "preferences", Some(preferences)).await?;
        self.update_record(|r| r.connect = true);
        Ok(())
    }

    pub(super) async fn refresh_connect(&self) {
        match self.connect_api(Method::GET, "link-state", None).await {
            Ok(state) => {
                let on = state["linked"] == true && state["managedTunnelActive"] == true;
                if self.record().connect != on {
                    self.update_record(|r| r.connect = on);
                }
            }
            Err(e) => tracing::warn!("reading T3 Connect state of {}: {e:#}", self.id()),
        }
    }

    async fn run_connect_setup(&self) -> Result<()> {
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        let dir = a37
            .exec_ok(
                &id,
                "d=$(mktemp -d); \
                 (\"$HOME/.local/bin/t3\" up --connect --headless --yes > \"$d/log\" 2>&1; echo $? > \"$d/exit\") \
                 </dev/null >/dev/null 2>&1 & echo \"$d\"",
                30,
            )
            .await?;
        let dir = super::github::validate_tmp_dir(dir.lines().last().unwrap_or_default().trim())?;
        let result = self.finish_connect_setup(&dir).await;
        let _ = a37.exec(&id, &format!("rm -rf '{dir}'; pkill -f '[t]3 up --connect' 2>/dev/null || true"), None, 30).await;
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
