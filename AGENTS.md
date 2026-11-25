# Working on t3agent37

- `apps/desktop/src` is the React UI. `apps/desktop/src-tauri` is the Rust backend for the native tray app and headless daemon. `apps/web` is the landing page and demo. The [architecture overview](apps/desktop/docs/architecture.md) describes the Agent37 and T3 Code boundaries.
- Keep machine lifecycle behavior in the Rust backend so the tray, daemon, and MCP entry points share it. Use the browser mock when changing preview behavior.
- Install frontend dependencies with `bun install --frozen-lockfile`. Run `bun --filter @t3boat/desktop build` for desktop UI changes and `bun --filter @t3boat/web build` when shared UI or site code changes.
- Run `cargo test --locked --no-default-features --manifest-path apps/desktop/src-tauri/Cargo.toml` for backend changes. This runs without GTK or live Agent37 credentials. The desktop Tauri build has additional system prerequisites.

## Cross-machine handoffs

- Local pairing and wake-on-demand run through the loopback proxy. Cross-machine setup is opt-in and uses T3 Connect's browser approval. Keep provisioning independent of the outcome of that approval.
- Use T3's relay installer and `t3 connect link --headless` for enrollment. Apply the link by restarting T3 through the Agent37 supervisor after checking for active work. `t3 up` manages its own service.
- Take the copyable address and reachability from T3 Connect's link state. A sleeping machine needs to wake before it can receive a handoff through its T3 Connect tunnel. Keep handoff addresses out of telemetry. See the [UI contract](apps/desktop/docs/ui-contract.md).

## Telemetry

- The native app starts with telemetry off until the user opts in. Keep consent and event delivery in Rust. Send only fields allowed by the [collector schema](telemetry/worker.mjs).
- Keep prompt text, credentials, raw errors, account identifiers, and project and machine details out of telemetry. PostHog's project token belongs in the Worker's secret binding. Desktop builds receive the public collector URL through `T3AGENT37_TELEMETRY_URL`.
- Run `node --test telemetry/worker.test.mjs` when changing collector behavior. Keep the [privacy notice](PRIVACY.md) and [collector guide](telemetry/README.md) aligned with the data collected and deployed settings.

## Releases

- Keep the release version in sync across the root and desktop `package.json` files, the desktop entry in `bun.lock`, and the desktop Cargo manifest, lockfile, and Tauri config.
- Add a matching release section to [CHANGELOG.md](CHANGELOG.md) and run `bun run release:verify`. Pushing a new version to `main` starts the [Release workflow](.github/workflows/release.yml). It runs the headless backend tests and builds macOS installers before publishing them with checksums and changelog notes.
