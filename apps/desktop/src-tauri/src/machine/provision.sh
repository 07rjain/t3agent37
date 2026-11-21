#!/usr/bin/env bash
# Runs on an Agent37 sandbox (custom t3agent37 template or stock agent37-base).
# Idempotent; also used to upgrade a machine. Requires no systemd and no sudo.
#   T3BOAT_SECRET=<watchdog-secret> provision.sh <t3-version> <sleep-after-secs> <mint-upstream-token:0|1> <name>
# Expects watchdog.mjs (and supervisor.mjs for stock images) next to it.
# Prints one JSON line: {"environmentId", "upstreamToken"?}.
set -euo pipefail

version="$1" sleep_after="$2" mint_upstream="$3" name="$4"
secret="$T3BOAT_SECRET"
here="$(cd "$(dirname "$0")" && pwd)"
t3="$HOME/.local/bin/t3"
state_dir="$HOME/.t3agent37"
mkdir -p "$state_dir" "$HOME/.local/bin" "$HOME/.t3/userdata"
chmod 700 "$state_dir"
export PATH="$HOME/.local/bin:$PATH"

find_supervisor_pid() {
  local pid_file="$state_dir/supervisor.pid"
  [ -f "$pid_file" ] || return 0
  local candidate
  candidate="$(tr -d '[:space:]' <"$pid_file" 2>/dev/null || true)"
  [ -n "$candidate" ] || return 0
  kill -0 "$candidate" 2>/dev/null || return 0
  if [ -r "/proc/$candidate/cmdline" ]; then
    local cmdline
    cmdline="$(tr '\0' ' ' <"/proc/$candidate/cmdline" 2>/dev/null || true)"
    case "$cmdline" in
      *supervisor.mjs*) echo "$candidate" ;;
    esac
  else
    echo "$candidate"
  fi
}

read_pid() {
  [ -f "$1" ] || return 0
  tr -d '[:space:]' <"$1" 2>/dev/null || true
}

wait_pid_exit() {
  local pid="$1"
  [ -n "$pid" ] || return 0
  for _ in $(seq 1 40); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.25
  done
}

ensure_supervisor() {
  local pid
  pid="$(find_supervisor_pid)"
  if [ -n "$pid" ]; then
    echo "$pid"
    return 0
  fi
  local script="$state_dir/supervisor.mjs"
  if [ ! -f "$script" ] && [ -f /usr/local/lib/t3agent37/supervisor.mjs ]; then
    script="/usr/local/lib/t3agent37/supervisor.mjs"
  fi
  if [ ! -f "$script" ]; then
    echo "supervisor.mjs not found" >&2
    exit 1
  fi
  nohup node --no-warnings --max-old-space-size=64 "$script" >>"$state_dir/supervisor.log" 2>&1 </dev/null &
  for _ in $(seq 1 40); do
    pid="$(find_supervisor_pid)"
    if [ -n "$pid" ]; then
      echo "$pid"
      return 0
    fi
    sleep 0.25
  done
  echo "Supervisor did not start" >&2
  tail -n 40 "$state_dir/supervisor.log" >&2 || true
  exit 1
}

# T3 server pinned to the desktop app's version, managed by supervisor.mjs (no systemd needed).
# It is restarted only when something it runs on changed: a restart drops every T3 Code
# connection, and most upgrades only touch the watchdog.
export PATH="$HOME/.local/bin:$PATH"
restart_t3=0
if [ "$("$t3" --version 2>/dev/null || true)" != "t3 v$version" ]; then
  if ! curl -fsSL https://t3.codes/install.sh | T3CODE_VERSION="$version" sh >"$state_dir/install.log" 2>&1; then
    echo "T3 install failed" >&2
    tail -n 40 "$state_dir/install.log" >&2 || true
    exit 1
  fi
  restart_t3=1
fi

# Ensure Claude Code and Codex CLIs are installed on PATH so T3 Code can run & delegate agents
# on stock agent37-base containers as well as custom templates. Installed into $HOME/.local so
# they persist across restarts/updates without requiring root.
missing_pkgs=()
command -v claude >/dev/null 2>&1 || missing_pkgs+=(@anthropic-ai/claude-code)
command -v codex >/dev/null 2>&1 || missing_pkgs+=(@openai/codex)
if [ "${#missing_pkgs[@]}" -gt 0 ] && command -v npm >/dev/null 2>&1; then
  if npm install -g --prefix "$HOME/.local" --no-audit --no-fund "${missing_pkgs[@]}" >"$state_dir/agents-install.log" 2>&1; then
    restart_t3=1
  else
    echo "Warning: agent CLI install failed (see $state_dir/agents-install.log)" >&2
  fi
fi

settings="$HOME/.t3/userdata/settings.json"
before="$(cat "$settings" 2>/dev/null || true)"
node -e '
  const fs = require("fs"), f = process.argv[1];
  const s = fs.existsSync(f) ? JSON.parse(fs.readFileSync(f, "utf8")) : {};
  s.linkedEnvironmentsEnabled = true;
  fs.writeFileSync(f, JSON.stringify(s, null, 2));
' "$settings"
[ "$before" = "$(cat "$settings")" ] || restart_t3=1

# The machine's name, as other devices show it (T3 Connect, linked environments).
# Best-effort only: unprivileged containers may not allow writing /etc/machine-info.
machine_info="PRETTY_HOSTNAME=$name"
if [ "$(cat /etc/machine-info 2>/dev/null || true)" != "$machine_info" ]; then
  if [ -w /etc/machine-info ] || { [ ! -e /etc/machine-info ] && [ -w /etc ]; }; then
    if printf '%s\n' "$machine_info" >/etc/machine-info 2>/dev/null; then
      restart_t3=1
    fi
  elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
    if printf '%s\n' "$machine_info" | sudo -n tee /etc/machine-info >/dev/null 2>&1; then
      restart_t3=1
    fi
  fi
fi

# Install supervisor.mjs (for stock agent37-base containers and local reference).
restart_supervisor=0
if [ -f "$here/supervisor.mjs" ]; then
  if [ -f "$state_dir/supervisor.mjs" ] && ! cmp -s "$here/supervisor.mjs" "$state_dir/supervisor.mjs"; then
    restart_supervisor=1
  fi
  install -m 600 "$here/supervisor.mjs" "$state_dir/supervisor.mjs"
elif [ -f /usr/local/lib/t3agent37/supervisor.mjs ] && [ ! -f "$state_dir/supervisor.mjs" ]; then
  install -m 600 /usr/local/lib/t3agent37/supervisor.mjs "$state_dir/supervisor.mjs"
fi

# Install watchdog.mjs (runs in-process inside supervisor.mjs to save ~130 MB RAM).
if [ -f "$state_dir/watchdog.mjs" ] && ! cmp -s "$here/watchdog.mjs" "$state_dir/watchdog.mjs"; then
  restart_supervisor=1
fi
install -m 600 "$here/watchdog.mjs" "$state_dir/watchdog.mjs"

# If upgraded scripts changed while supervisor was already running, restart supervisor cleanly
# (note: upgrade_if_needed only runs while no agent is working or waiting on the user).
# On custom template images, entrypoint.sh automatically respawns the upgraded supervisor so
# tini never exits; wait briefly for that respawn before launching via nohup (stock agent37-base).
sup_pid="$(find_supervisor_pid)"
if [ "$restart_supervisor" = 1 ] && [ -n "$sup_pid" ]; then
  kill -TERM "$sup_pid" 2>/dev/null || true
  wait_pid_exit "$sup_pid"
  sup_pid=""
  for _ in $(seq 1 12); do
    sup_pid="$(find_supervisor_pid)"
    [ -n "$sup_pid" ] && break
    sleep 0.25
  done
fi

# Ensure supervisor is running; only restart t3 when needed or not yet answering.
if [ -z "$sup_pid" ]; then
  sup_pid="$(ensure_supervisor)"
elif [ "$restart_t3" = 1 ] || ! curl -sf -o /dev/null http://127.0.0.1:3773/.well-known/t3/environment; then
  old_t3_pid="$(read_pid "$state_dir/t3.pid")"
  kill -USR2 "$sup_pid"
  wait_pid_exit "$old_t3_pid"
fi

ready=0
for _ in $(seq 1 120); do
  if curl -sf -o /dev/null http://127.0.0.1:3773/.well-known/t3/environment; then
    ready=1
    break
  fi
  sleep 0.5
done
if [ "$ready" != 1 ]; then
  echo "T3 server did not start" >&2
  tail -n 40 "$state_dir/t3.log" >&2 || true
  exit 1
fi

# Watchdog: its own read-only T3 token, kept across upgrades.
config="$state_dir/config.json"
watchdog_token="$(node -e 'try{process.stdout.write(require(process.argv[1]).t3Token||"")}catch{}' "$config")"
if [ -z "$watchdog_token" ]; then
  watchdog_token="$("$t3" auth session issue --token-only --ttl 3650d --label t3agent37-watchdog --scope orchestration:read)"
fi

SECRET="$secret" TOKEN="$watchdog_token" SLEEP="$sleep_after" node -e '
  const fs = require("fs"), e = process.env, target = process.argv[1], tmp = `${target}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify({
    secret: e.SECRET,
    t3Token: e.TOKEN,
    sleepAfterSecs: Number(e.SLEEP),
  }, null, 2), { mode: 0o600 });
  fs.chmodSync(tmp, 0o600);
  fs.renameSync(tmp, target);
' "$config"

# Load or hot-reload watchdog config in-process inside supervisor (never touches t3).
sup_pid="$(ensure_supervisor)"
kill -USR1 "$sup_pid"

wd_ready=0
for _ in $(seq 1 120); do
  if printf 'header = "Authorization: Bearer %s"\n' "$secret" | curl -sf -o /dev/null -K - http://127.0.0.1:3774/t3boat-status; then
    wd_ready=1
    break
  fi
  sleep 0.5
done
if [ "$wd_ready" != 1 ]; then
  echo "Watchdog did not start" >&2
  tail -n 40 "$state_dir/supervisor.log" >&2 || true
  exit 1
fi

upstream_token=""
if [ "$mint_upstream" = 1 ]; then
  upstream_token="$("$t3" auth session issue --token-only --ttl 3650d --label t3agent37)"
fi
environment_id="$(curl -sf http://127.0.0.1:3773/.well-known/t3/environment | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).environmentId))')"
ENV_ID="$environment_id" TOK="$upstream_token" node -e '
  const o = { environmentId: process.env.ENV_ID };
  if (process.env.TOK) o.upstreamToken = process.env.TOK;
  console.log(JSON.stringify(o));
'
