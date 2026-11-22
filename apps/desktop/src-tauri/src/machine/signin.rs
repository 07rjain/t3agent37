//! Signing an agent in on a machine with the vendor's own CLI login. The login runs on the
//! machine and its localhost callback port is forwarded to this computer over Agent37's
//! authenticated HTTPS preview proxy (`Agent37::forward`), so the browser's redirect after
//! "Authorize" lands on the machine: no code to paste, no external CLI required, and the
//! credential stays on the machine. This computer's own logins are never touched.

use super::MachineRuntime;
use crate::proxy::upstream;
use anyhow::{anyhow, bail, Context, Result};
use std::time::{Duration, Instant};

const URL_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// By the provider's display name, as `signedOut` lists it.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Claude" => Some(Self::Claude),
            "Codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn login(self) -> &'static str {
        match self {
            Self::Claude => "claude auth login",
            Self::Codex => "codex login",
        }
    }

    /// Exits 0 once signed in.
    fn status(self) -> &'static str {
        match self {
            Self::Claude => "PATH=\"$HOME/.local/bin:$PATH\" claude auth status >/dev/null 2>&1",
            Self::Codex => "PATH=\"$HOME/.local/bin:$PATH\" codex login status >/dev/null 2>&1",
        }
    }
}

impl MachineRuntime {
    pub async fn sign_in(&self, agent: Agent) -> Result<()> {
        // Cancel any prior in-flight OAuth sign-in so its local callback port (e.g. 1455)
        // is immediately released before starting a new attempt.
        let prev = self.shared.sign_in_cancel.lock().unwrap().take();
        if let Some((cancel_tx, done_rx)) = prev {
            let _ = cancel_tx.send(());
            let _ = tokio::time::timeout(Duration::from_secs(5), done_rx).await;
        }
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        *self.shared.sign_in_cancel.lock().unwrap() = Some((cancel_tx, done_rx));

        struct SignInGuard {
            shared: std::sync::Arc<super::Shared>,
            done_tx: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl Drop for SignInGuard {
            fn drop(&mut self) {
                self.shared.set_sign_in_url(None);
                if let Some(tx) = self.done_tx.take() {
                    let _ = tx.send(());
                }
            }
        }
        let _guard = SignInGuard { shared: self.shared.clone(), done_tx: Some(done_tx) };

        self.ensure_awake().await?;
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        // The CLI hands the sign-in URL to $BROWSER; this shim records it instead.
        let dir = a37
            .exec_ok(
                &id,
                &format!(
                    "d=$(mktemp -d); printf '#!/bin/sh\\necho \"$1\" > %s/url\\n' \"$d\" > \"$d/browser\"; \
                     chmod +x \"$d/browser\"; \
                     nohup sh -c 'PATH=\"$HOME/.local/bin:$PATH\" BROWSER=\"$1/browser\" script -qfec \"$2\" \"$1/log\" >/dev/null 2>&1; echo $? > \"$1/done\"' _ \"$d\" '{}' </dev/null >/dev/null 2>&1 & echo \"$d\"",
                    agent.login()
                ),
                30,
            )
            .await?;
        let dir = super::github::validate_tmp_dir(dir.lines().last().unwrap_or_default().trim())?;
        let result = self.kept_awake(self.finish_sign_in(agent, &dir, &mut cancel_rx)).await;
        let pattern = format!("[{}]{}", &agent.login()[..1], &agent.login()[1..]);
        let _ = a37
            .exec(&id, &format!("rm -rf '{dir}'; pkill -f '{pattern}' 2>/dev/null || true"), None, 30)
            .await;
        drop(_guard);
        result?;
        let refreshed = upstream::call_with_headers(
            &self.upstream_url(),
            &self.upstream_token(),
            &self.edge_headers(),
            "server.refreshProviders",
            serde_json::json!({}),
        )
        .await;
        if let Ok(result) = refreshed {
            self.cache.lock().unwrap().fold_providers(&result["providers"]);
            self.save_cache();
            self.shared.changed();
        }
        Ok(())
    }

    async fn finish_sign_in(
        &self,
        agent: Agent,
        dir: &str,
        cancel_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        let started = Instant::now();
        let url = loop {
            let out = a37.exec(&id, &format!("cat '{dir}/url' 2>/dev/null"), None, 30).await?;
            if let Some(url) = out.stdout.split_whitespace().find(|w| w.starts_with("https://")) {
                break url.to_owned();
            }
            if started.elapsed() > URL_TIMEOUT {
                bail!("the {agent:?} login did not start on the machine");
            }
            tokio::select! {
                _ = &mut *cancel_rx => bail!("the {agent:?} sign-in was replaced by a newer attempt"),
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
            }
        };
        let port = validated_oauth_callback(agent, &url)
            .ok_or_else(|| anyhow!("unexpected or invalid {agent:?} login URL"))?;
        let forward = a37
            .forward(&id, port)
            .await
            .with_context(|| format!("forwarding port {port} for the {agent:?} login"))?;
        self.shared.set_sign_in_url(Some(url.clone()));
        self.shared.notify(
            format!("{agent:?} sign-in for {}", self.record().name),
            "Complete authentication in the opened browser tab.".to_owned(),
        );
        if let Err(e) = crate::app::open_url(&url) {
            tracing::warn!("could not auto-open browser for {agent:?} sign-in: {e:#}");
        }
        let outcome = loop {
            tokio::select! {
                _ = &mut *cancel_rx => break Err(anyhow!("the {agent:?} sign-in was replaced by a newer attempt")),
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
            if a37.exec(&id, agent.status(), None, 30).await?.exit_code == Some(0) {
                break Ok(());
            }
            if a37.exec(&id, &format!("test -f '{dir}/done'"), None, 20).await?.exit_code == Some(0) {
                if a37.exec(&id, agent.status(), None, 30).await?.exit_code == Some(0) {
                    break Ok(());
                }
                break Err(anyhow!("the {agent:?} sign-in ended before completing"));
            }
            if started.elapsed() > LOGIN_TIMEOUT {
                break Err(anyhow!("the {agent:?} sign-in was not finished in the browser"));
            }
        };
        forward.stop().await;
        outcome
    }
}

/// Validates that `url` uses HTTPS on a known vendor OAuth host for `agent`, and returns the
/// localhost callback port from `redirect_uri`.
fn validated_oauth_callback(agent: Agent, url: &str) -> Option<u16> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let allowed = match agent {
        Agent::Claude => matches!(
            host.as_str(),
            "claude.ai" | "claude.com" | "console.anthropic.com" | "anthropic.com"
        ) || host.ends_with(".claude.ai")
            || host.ends_with(".claude.com")
            || host.ends_with(".anthropic.com"),
        Agent::Codex => matches!(host.as_str(), "auth.openai.com" | "chatgpt.com" | "openai.com")
            || host.ends_with(".openai.com")
            || host.ends_with(".chatgpt.com"),
    };
    if !allowed {
        return None;
    }
    callback_port(url)
}

/// The port of a `redirect_uri=http://localhost:<port>/...` (or 127.0.0.1) parameter.
fn callback_port(url: &str) -> Option<u16> {
    let url = url::Url::parse(url).ok()?;
    let redirect = url.query_pairs().find(|(k, _)| k == "redirect_uri")?.1;
    let redirect = url::Url::parse(&redirect).ok()?;
    matches!(redirect.host_str()?, "localhost" | "127.0.0.1").then_some(())?;
    redirect.port()
}

#[cfg(test)]
mod tests {
    use super::callback_port;

    #[test]
    fn finds_the_local_callback_port() {
        let claude = "https://claude.com/cai/oauth/authorize?code=true&redirect_uri=http%3A%2F%2Flocalhost%3A43327%2Fcallback&state=x";
        let codex = "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback";
        let manual = "https://claude.com/cai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback";
        assert_eq!(callback_port(claude), Some(43327));
        assert_eq!(callback_port(codex), Some(1455));
        assert_eq!(callback_port(manual), None);
    }
}
