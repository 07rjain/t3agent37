#!/usr/bin/env bash
# Container entrypoint under tini: keeps the container alive when provision.sh restarts
# supervisor.mjs during an in-place script upgrade, while waiting for clean child teardown
# on container SIGTERM/SIGINT.
set -u

stopping=0
child=""

on_stop() {
  stopping=1
  if [ -n "$child" ]; then
    kill -TERM "$child" 2>/dev/null || true
  fi
}

trap on_stop TERM INT

while [ "$stopping" = 0 ]; do
  script="/usr/local/lib/t3agent37/supervisor.mjs"
  if [ -f "$HOME/.t3agent37/supervisor.mjs" ]; then
    script="$HOME/.t3agent37/supervisor.mjs"
  fi
  node --no-warnings --max-old-space-size=64 "$script" &
  child=$!
  # If SIGTERM/SIGINT arrived between the loop check and `child=$!`, forward it now.
  if [ "$stopping" = 1 ]; then
    kill -TERM "$child" 2>/dev/null || true
  fi
  # Bash interrupts `wait` when a trapped signal arrives; keep waiting until the child actually exits.
  while kill -0 "$child" 2>/dev/null; do
    wait "$child" 2>/dev/null || true
  done
  wait "$child" 2>/dev/null || true
  child=""
  [ "$stopping" = 0 ] || break
  sleep 0.2
done
exit 0
