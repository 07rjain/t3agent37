//! GitHub authentication on an Agent37 instance. Unlike Boat, Agent37 does not pre-sign
//! sandboxes into GitHub, so t3agent37 supports either:
//! - Interactive browser device login (`gh auth login --web`, opening the verification URL locally
//!   and showing the one-time code in the UI & notification), or
//! - A user-supplied fine-grained PAT piped over stdin (`gh auth login --with-token`).

use super::MachineRuntime;
use anyhow::{bail, Result};
use base64::Engine;
use std::time::{Duration, Instant};

const CODE_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

impl MachineRuntime {
    /// Checks whether `gh` is authenticated on the running instance.
    pub async fn refresh_github(&self) {
        let id = self.sandbox_id();
        match self.shared.agent37.exec(&id, "gh auth status >/dev/null 2>&1", None, 20).await {
            Ok(res) => {
                let signed_in = res.exit_code == Some(0);
                if self.record().github_signed_in != signed_in {
                    self.update_record(|r| r.github_signed_in = signed_in);
                }
            }
            Err(e) => tracing::warn!("checking GitHub auth on {}: {e:#}", self.id()),
        }
    }

    /// Authenticates `gh` and git's credential helper on the instance.
    /// If `token` is `Some`, pipes it via base64 into `gh auth login --with-token`;
    /// otherwise launches `gh auth login --web` device flow and opens the browser locally.
    pub async fn sign_in_github(&self, token: Option<&str>) -> Result<()> {
        self.ensure_awake().await?;
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        if let Some(tok) = token.map(str::trim).filter(|t| !t.is_empty()) {
            let encoded = base64::engine::general_purpose::STANDARD.encode(tok.as_bytes());
            let script = format!(
                "set -e; printf '%s' '{encoded}' | base64 -d | gh auth login --hostname github.com --git-protocol https --with-token && gh auth setup-git"
            );
            a37.exec_ok(&id, &script, 60).await?;
            self.update_record(|r| r.github_signed_in = true);
            return Ok(());
        }

        // Device-code browser flow: start in background, read one-time code & URL, open locally.
        let dir = a37
            .exec_ok(
                &id,
                "d=$(mktemp -d); \
                 nohup sh -c 'printf \"\\n\" | gh auth login --hostname github.com --git-protocol https --web > \"$1/log\" 2>&1; echo $? > \"$1/exit\"' _ \"$d\" \
                 </dev/null >/dev/null 2>&1 & echo \"$d\"",
                30,
            )
            .await?;
        let dir = validate_tmp_dir(dir.lines().last().unwrap_or_default().trim())?;
        let result = self.kept_awake(self.finish_github_device_login(&dir)).await;
        self.shared.set_sign_in_code(None);
        let _ = a37.exec(&id, &format!("rm -rf '{dir}'; pkill -f '[g]h auth login' 2>/dev/null || true"), None, 30).await;
        result?;
        a37.exec_ok(&id, "gh auth setup-git", 30).await?;
        self.update_record(|r| r.github_signed_in = true);
        Ok(())
    }

    async fn finish_github_device_login(&self, dir: &str) -> Result<()> {
        let id = self.sandbox_id();
        let a37 = &self.shared.agent37;
        let started = Instant::now();
        let mut opened = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let out = a37
                .exec(&id, &format!("cat '{dir}/exit' 2>/dev/null; echo ---; cat '{dir}/log' 2>/dev/null"), None, 30)
                .await?;
            let (exit, log) = out.stdout.split_once("---").unwrap_or_default();
            if !opened {
                if let Some((code, url)) = parse_device_login(log) {
                    self.shared.set_sign_in_code(Some(code.clone()));
                    self.shared.notify(
                        format!("GitHub sign-in for {}", self.record().name),
                        format!("Enter one-time code {code} at {url}"),
                    );
                    let _ = crate::app::open_url(&url);
                    opened = true;
                } else if started.elapsed() > CODE_TIMEOUT {
                    bail!("gh auth login did not print a device code: {}", log.trim());
                }
            }
            match exit.trim() {
                "" => {}
                "0" => return Ok(()),
                _ => bail!("GitHub sign-in failed: {}", log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error")),
            }
            if started.elapsed() > LOGIN_TIMEOUT {
                bail!("GitHub sign-in was not finished in the browser");
            }
        }
    }
}

/// Validates a sandbox temp directory returned by `mktemp -d`: must be a single direct child
/// name directly beneath `/tmp/` (never `/tmp`, `/tmp//`, `/tmp/.`, or nested/traversal paths).
pub(crate) fn validate_tmp_dir(dir: &str) -> Result<String> {
    let Some(name) = dir.strip_prefix("/tmp/") else {
        bail!("unexpected temporary directory from sandbox: {dir}");
    };
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains("..")
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("unexpected temporary directory from sandbox: {dir}");
    }
    Ok(dir.to_owned())
}

/// Extracts `(one_time_code, verification_url)` from `gh auth login --web` output.
pub(crate) fn parse_device_login(log: &str) -> Option<(String, String)> {
    let url = log
        .split_whitespace()
        .find(|w| w.starts_with("https://github.com/login/device"))
        .map(|u| u.trim_end_matches(|c: char| matches!(c, '.' | ',' | ')')).to_owned())?;
    let code = log.split_whitespace().find(|w| {
        let b = w.as_bytes();
        b.len() == 9
            && b[4] == b'-'
            && b[..4].iter().all(|c| c.is_ascii_alphanumeric())
            && b[5..].iter().all(|c| c.is_ascii_alphanumeric())
    })?;
    Some((code.to_owned(), url))
}

#[cfg(test)]
mod tests {
    use super::{parse_device_login, validate_tmp_dir};

    #[test]
    fn parses_gh_device_code_and_url() {
        let sample = "! First copy your one-time code: A1B2-C3D4\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
        assert_eq!(
            parse_device_login(sample),
            Some(("A1B2-C3D4".into(), "https://github.com/login/device".into()))
        );
    }

    #[test]
    fn validates_tmp_dir_strictly() {
        assert!(validate_tmp_dir("/tmp/tmp.AbC123_x-y").is_ok());
        assert!(validate_tmp_dir("/tmp//").is_err());
        assert!(validate_tmp_dir("/tmp/.").is_err());
        assert!(validate_tmp_dir("/tmp/a/b").is_err());
        assert!(validate_tmp_dir("/tmp/a'; rm -rf /").is_err());
        assert!(validate_tmp_dir("/tmp/../etc").is_err());
        assert!(validate_tmp_dir("/home/node/x").is_err());
    }
}
