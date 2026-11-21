# How t3agent37 works

```text
T3 Code ──http/ws──► localhost proxy (t3agent37) ──https/wss (+ X-Agent37-Key)──► t3 serve (:3773) on Agent37
                         │                                                          ▲
                         ├─► Agent37 REST API (create/start/sleep/resize/delete)    │ polls shell + reads statev2.sqlite
                         ├─► local T3 CLI & server (~/.t3/bin/t3)                   │
                         └─► local MCP server (127.0.0.1:47730/mcp)          watchdog.mjs (:3774, read-only)
```

- **Zero `public_ports`, zero keys in sandboxes**: Every Agent37 port is exposed on `https://{id}-{port}.agent37.app` behind Agent37's edge authentication (`X-Agent37-Key`). The local proxy holds `sk_live_...` privately (mode `0600`) and injects `X-Agent37-Key` on upstream HTTP and WebSocket upgrades while forwarding T3 Code's `Authorization: Bearer` untouched. Agent37's edge strips `X-Agent37-*` before forwarding, so sandboxes never see your workspace key.
- **Single-process container & VM supervisor (`template/supervisor.mjs`)**: Because Agent37 `default` instances are containers without `systemd`, `t3agent37` runs a single lightweight `--max-old-space-size=64` Node supervisor that manages `t3 serve --port 3773 --host 0.0.0.0` as a child process and runs `watchdog.mjs` (`0.0.0.0:3774`) **in-process** (cutting sandbox overhead RAM by ~48% vs two separate Node processes). All state (`~/.local/bin/t3`, `~/.t3/userdata`, `~/.t3agent37`) lives on the persistent home mount.
- **Read-only watchdog & proxy-driven sleep**: `watchdog.mjs` monitors T3's local shell, `statev2.sqlite`, CPU load, scheduled work, and disk usage (`statfsSync`), exposing `/t3boat-status` (avoiding `/health`, which Agent37's edge intercepts). It never calls Agent37 to stop itself. When idle for 3 minutes, it publishes an orchestration manifest; the local proxy captures shell/thread/asset snapshots, posts `/sleep-ack`, and calls `POST /v1/instances/{id}/sleep` (CRIU memory checkpoint, compute billing drops to `$0/hr`).
- **Wake & `409 try_again` resilience**: Waking actions call `POST /v1/instances/{id}/start` with exponential backoff on `409 try_again` (while a checkpoint write is finishing) and `202` polling (on host moves), then wait for `/.well-known/t3/environment` (kicking `supervisor.mjs` via `exec` if a cold-storage rebuild booted fresh).
- **Disk-first billing visibility**: Agent37 bills retained disk (`$0.09/GB/mo`) continuously even while asleep, and disks can only grow. `t3agent37` defaults to small disks (12 GB = `$1.08/mo`), reports disk usage (`>= 80%` warning + 1-click grow via `POST /resize`), shows wallet balance + awake RAM limit from `GET /v1/billing`, and uses an orphan reaper to ensure deleted or failed instances are confirmed gone from `GET /v1/instances`.
- **Headless VPS daemon + Desktop app**: Tauri/GTK dependencies are gated behind the `desktop` Cargo feature, so `t3agent37-daemon` compiles and runs natively on headless Linux servers (solving laptop-closed sleep cleanly) while sharing 100% of the core engine with the Tauri tray app.
