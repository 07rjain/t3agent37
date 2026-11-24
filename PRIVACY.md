# Desktop telemetry

Desktop telemetry is off until you choose **Share telemetry**. You can turn it on or off in **Settings → Privacy** or from the setup screen. Turning it off stops future sends, discards queued events, and removes the installation ID from the app. Events already received follow the retention period below. Turning telemetry back on generates a new random ID.

When enabled, t3agent37 records the first launch after consent, first machine provisioned, and first successful T3 Code pairing after provisioning. It also records sleep and wake outcomes with approximate duration ranges and app update results with predefined failure stages. Each event includes a random installation ID, the app version, the operating system and processor architecture, and the macOS major version when available.

The event payload excludes prompts and replies, source code, repo names and paths, machine names and IDs, email addresses, credentials, and logs. The app uses Cloudflare to receive and rate-limit requests. Cloudflare uses the connection IP for rate limiting. The collector forwards the event to a dedicated PostHog project without forwarding client headers. PostHog person profiles, IP capture, and location enrichment are disabled for this data.

The PostHog project uses a one-year event retention period. PostHog deletes expired events in batches. The queue on your computer is held in memory and is discarded when the app quits.

For privacy questions, [open a GitHub issue](https://github.com/07rjain/t3agent37/issues). Keep credentials and private project details out of public posts.
