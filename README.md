# t3agent37

<p>
  <a href="https://t3agent37.subdomain.wtf/">Website &amp; Interactive Demo</a> ·
  <a href="https://github.com/07rjain/t3agent37/releases/latest">Download for macOS</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="apps/desktop/docs/architecture.md">How it works</a>
</p>

Auto-sleeping [T3 Code](https://t3.codes) remote machines on [Agent37](https://www.agent37.com), forked from [paukraft/t3boat](https://github.com/paukraft/t3boat).

Create isolated cloud containers or VMs on your Agent37 wallet, pair them into T3 Code through an edge-authenticated localhost proxy, and let them CRIU-checkpoint to sleep when idle so compute billing drops to `$0/hr` (retaining persistent disk at `$0.09/GB/mo` until deleted).

## What you get

- **Edge-authenticated localhost proxy** (`127.0.0.1:47731+`): injects `X-Agent37-Key` on upstream HTTPS/WebSocket calls while passing T3 Code's `Authorization: Bearer` untouched. Zero `public_ports` needed, and your `sk_live_...` API key never enters any sandbox.
- **CRIU checkpoint sleep & wake-on-demand**: read-only in-box watchdog (`:3774`) monitors T3 runs, `statev2.sqlite`, CPU, and disk usage; the local proxy captures state and calls `POST /v1/instances/{id}/sleep`, waking via `POST /start` with automatic `409 try_again` backoff.
- **Containers, VMs, and Performance tiers**: defaults to `default` containers (`2 vCPU / 4 GB / 12 GB` — ~$4.76/mo compute + `$1.08/mo` disk), with 1-click `vm` (2.5×, full kernel/Docker) and `performance` (4×) options across `us`, `eu`, and `ap-south`.
- **Headless VPS daemon + Desktop menu-bar app**: run `t3agent37-daemon` on an always-on Linux VPS (zero GTK/WebKit dependencies) so machines sleep reliably even when your laptop is closed, or build the full Tauri desktop tray app.
- **Projects, GitHub, Agent OAuth & T3 Connect**: clone local T3 projects with `.env` files, sign in to GitHub (`gh auth login --web` or scoped PAT), sign in Claude/Codex via Agent37's WebSocket SSH tunnel, and auto-link into T3 Code's Linked Environments.
- **Local MCP server** (`http://127.0.0.1:47730/mcp`): lets local Claude Code or Codex agents create, wake, sleep, resize, and manage Agent37 machines.

## Quick start

On macOS, download the `aarch64` DMG for Apple Silicon or the `x86_64` DMG for Intel from [GitHub Releases](https://github.com/07rjain/t3agent37/releases/latest). Open the DMG, drag t3agent37 to Applications, and follow the menu-bar app's setup prompts. Existing installations can check for updates in **Settings → Updates**. See the [changelog](CHANGELOG.md) for release notes.

### 1. Build & run the headless daemon (Linux VPS / CLI — no GTK needed)

```sh
cd apps/desktop/src-tauri
cargo build --no-default-features --release --bin t3agent37-daemon

export AGENT37_API_KEY="sk_live_..."
./target/release/t3agent37-daemon
```

Interactive daemon commands: `list`, `create <name> [size] [type] [region] [disk_gb]`, `pair <id>`, `wake <id>`, `sleep <id>`, `keep <id> [min]`, `resize <id> <size> [disk_gb]`, `grow-disk <id> <gb>`, `github <id> [pat]`, `signin <id> <Claude|Codex>`, `connect <id>`, `delete <id> [discard]`, `costs`.

### 2. Optional: Build the custom Agent37 template (faster boots)

```sh
npx agent37 templates build template --name t3agent37 --default-port 3773
```

Then set Template to `t3agent37` in Settings (works out of the box on stock `agent37-base` too).

### 3. Build the desktop Tauri tray app

Requires Rust, Bun, and [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
bun install
bun run build:desktop
```

## Desktop telemetry

Telemetry is off until you choose **Share telemetry**. You can change your choice in **Settings → Privacy**. The native desktop app can share setup milestones, sleep and wake outcomes, and app update results. Turning telemetry off clears queued events and removes the local installation ID. See the [privacy notice](PRIVACY.md) for the data fields and retention period, and the [collector guide](telemetry/README.md) for deployment and verification.

## Development & Tests

```sh
# Rust unit + end-to-end mock Agent37 integration tests (no GTK required)
cargo test --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml

# Frontend typecheck & production bundle
bun --filter @t3boat/desktop build

# Telemetry collector contract and release version checks
node --test telemetry/worker.test.mjs
bun run release:verify
```

See the [architecture overview](apps/desktop/docs/architecture.md) and [agent guide](AGENTS.md).

## License

[MIT](LICENSE). Based on [t3boat](https://github.com/paukraft/t3boat) by Pau Kraft. Not affiliated with or endorsed by T3 Code or Agent37.
