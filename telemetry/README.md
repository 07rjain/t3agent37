# Desktop telemetry collector

The native desktop build sends opt-in events to `POST /v1/events`. The Cloudflare Worker validates each payload and forwards it to a dedicated PostHog project. The daemon and bundled web console do not send these events.

The t3agent37 collector runs at `https://t3agent37-telemetry.bothra434.workers.dev/v1/events` and sends events to the [t3agent37 PostHog project](https://us.posthog.com/project/763458). The [desktop telemetry dashboard](https://us.posthog.com/project/763458/dashboard/2404276) excludes smoke tests tagged with the app version `0.0.0-telemetry-smoke`.

## Connect a release

1. Create a dedicated PostHog Cloud project. Use the Free plan's one-year event retention or set a one-year project override. Set **IP data capture** to **Discard** and disable **GeoIP enrichment**. Set `POSTHOG_REGION` in [wrangler.toml](wrangler.toml) to `eu` or `us` for the project's region.
2. In this directory, deploy with `npx wrangler deploy` and set the project's **Project token** with `npx wrangler secret put POSTHOG_PROJECT_TOKEN`. Use the `phc_` token from Project Settings. Leave Worker request logging disabled for the collector.
3. Set the GitHub Actions repository variable `T3AGENT37_TELEMETRY_URL` to the full HTTPS Worker URL ending in `/v1/events`. The release workflow embeds only this public URL. For a local Tauri build, set the same environment variable before building. A build without an HTTPS collector URL has telemetry disabled and hides its controls.
4. Test against a staging project first. Confirm that a clean install sends zero events while undecided or opted out. Opt in and check that `first_launch`, `first_machine_provisioned`, and `first_t3_paired` arrive in that order under one installation ID. Verify that opting out stops new events, re-enabling generates a fresh ID, and an offline collector leaves machine operations and updates usable.

## Dashboard

- Create a PostHog funnel using `first_launch`, `first_machine_provisioned`, and `first_t3_paired`. Label it **Opted-in desktop installs**.
- Plot `machine_sleep` and `machine_wake` by `outcome` and filter by `app_version`. Plot `update_failed` by `stage` alongside `update_succeeded`. Some failed installs never relaunch, so these update counts do not give a complete install success rate.

Delivery is best-effort. Events can be dropped when the app exits, the collector is unreachable, or the queue is full. A first-run milestone is marked when queued for delivery.

Run the collector contract tests with `node --test telemetry/worker.test.mjs` from the repository root.
