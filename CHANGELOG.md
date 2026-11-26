# Changelog

All notable changes to **t3agent37** are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.2] - 2026-11-26

### Fixed
- Deleting an Agent37 machine removes its T3 handoff link and attempts to deregister it from T3 Connect before destroying the VM. Cleanup resumes after a restart and shows manual removal instructions if it cannot confirm success.

## [0.4.1] - 2026-11-25

### Fixed
- After browser approval, handoff setup restarts the T3 server managed by t3agent37 once the machine is idle and waits for its T3 Connect address to become reachable.
- Use **Check connection** in machine details to retry reachability for a configured tunnel without restarting T3.

## [0.4.0] - 2026-11-25

### Added
- **Cross-machine handoffs.** Opt in when launching a machine or choose **Set up handoffs** in machine details. After you approve T3 Connect in the browser, T3 Code can discover the machine by name.
- Machine details shows T3 Connect reachability and lets you copy the handoff address when available. Wake a sleeping machine before handing off from another device.

### Changed
- The landing page loads the interactive demo in walkthrough order so the first step can appear sooner.

## [0.3.1] - 2026-11-24

### Fixed
- The telemetry consent screen uses the menu-bar header and controls. The Bond ASCII artwork fills the header width.

## [0.3.0] - 2026-11-24

### Added
- **Opt-in desktop telemetry** for first machine setup and T3 Code pairing, sleep and wake outcomes, and app update results. Choose during setup or under **Settings → Privacy**. Telemetry starts off for both new and existing installs.
- **Telemetry collector and privacy notice.** The Cloudflare Worker validates and rate-limits events before forwarding them to a dedicated PostHog project. Events use a random installation ID and a one-year retention period. See [what the app shares](https://github.com/07rjain/t3agent37/blob/main/PRIVACY.md).

## [0.2.0] - 2026-11-22

### Added
- **In-app GitHub auto-updater & changelog**: Automatic background checks against GitHub Releases (`07rjain/t3agent37`), an **Update** button and banner in the menubar popover, native tray menu update item, SHA-256 verified `.app` bundle replacement + relaunch on macOS, and release notes viewer in Settings.
- **GitHub Releases CI workflow**: Automatically builds macOS Apple Silicon (`aarch64`) and Intel (`x86_64`) `.app.tar.gz` and `.dmg` bundles, generates `SHA256SUMS.txt`, and publishes versioned releases with notes extracted from `CHANGELOG.md`.
- **007 Gun-Barrel Iris icons & noir theme**: Replaced sailboat icons with the 007 Gun-Barrel Iris macOS menu bar template icon and app bundle icons, plus the Agent37 007 gun-barrel ASCII strip and aperture icons across desktop and Web UI.
- **Native HTTPS OAuth forwarder**: Sign in to Codex and Claude Code directly over Agent37's authenticated HTTPS preview proxy without requiring the `agent37` CLI or SSH.
- **Live provisioning progress**: Streams real-time setup phases (`Installing T3 Code`, `Installing agent CLIs`, `Starting T3 server`, `Starting watchdog`) into the menubar UI.

### Fixed
- **Machine creation hang & auto-delete**: Fixed an unbounded `curl` probe in `provision.sh` that hung indefinitely when hitting `t3 serve` 0.1.12's early TCP listen window, causing creation to time out and the orphan reaper to auto-delete the new instance.
- **Cloudflare 100s exec timeout resilience**: Provisioning now runs detached on the sandbox and polls every 1.5s via short `exec` calls, eliminating single-request gateway timeouts on slow installs.
- **Transient `503 try_again` retries**: Agent37 API and exec calls now retry `503`/`502` `try_again` responses (not only `409`), preventing premature failures during boot and host transitions.
- **Visible creation failure alerts**: Failed machine creations now log the full error and post a native OS notification instead of silently disappearing when the menubar popover closes.
- **macOS Cargo default binary**: Set `default-run = "t3boat"` in `Cargo.toml` so `cargo tauri dev` / `build` selects the desktop binary unambiguously.

## [0.1.0] - 2026-11-21

### Added
- Initial **t3agent37** release powered by Agent37 cloud sandboxes (`default` containers, `vm`, and `performance` shapes across `us`, `eu`, and `ap-south`).
- Localhost authenticating proxy with CRIU auto-sleep after 3 minutes of agent inactivity and transparent wake-on-demand.
- T3 Code 0.1.12 Bearer pairing through the localhost proxy.
- Headless VPS daemon (`t3agent37-daemon`) with live Web Console and MCP server integration for Claude Code and Codex.
