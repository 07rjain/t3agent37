const MAX_BODY_BYTES = 1024;
const EVENTS_PATH = "/v1/events";
const POSTHOG_ENDPOINTS = {
  us: "https://us.i.posthog.com/i/v0/e/",
  eu: "https://eu.i.posthog.com/i/v0/e/",
};

const EVENTS = new Map([
  ["first_launch", []],
  ["first_machine_provisioned", []],
  ["first_t3_paired", []],
  ["machine_wake", ["outcome", "duration"]],
  ["machine_sleep", ["outcome", "duration"]],
  ["update_failed", ["stage"]],
  ["update_succeeded", []],
]);
const BASE_FIELDS = ["event", "install_id", "app_version", "os", "os_major", "arch"];
const DURATIONS = ["under_5s", "5_to_29s", "30_to_119s", "at_least_120s"];
const UPDATE_STAGES = ["check", "download", "verify", "install", "open_release_page", "relaunch"];

function validEvent(event) {
  if (!event || typeof event !== "object" || Array.isArray(event)) return false;
  const extra = EVENTS.get(event.event);
  if (!extra) return false;

  const allowed = [...BASE_FIELDS, ...extra];
  const keys = Object.keys(event);
  if (keys.length !== allowed.length || !keys.every((key) => allowed.includes(key))) return false;
  if (typeof event.install_id !== "string" || event.install_id.length !== 32
    || !/^[0-9a-f]{32}$/.test(event.install_id)) return false;
  if (typeof event.app_version !== "string" || event.app_version.length > 64
    || event.app_version !== event.app_version.trim()
    || !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(event.app_version)) return false;
  if (!["macos", "linux", "windows"].includes(event.os)
    || !["aarch64", "x86_64", "x86", "arm", "riscv64"].includes(event.arch)) return false;
  if (event.os_major !== null
    && (!Number.isInteger(event.os_major) || event.os_major < 1 || event.os_major > 999)) return false;

  switch (event.event) {
    case "machine_wake":
      return ["succeeded", "failed"].includes(event.outcome) && DURATIONS.includes(event.duration);
    case "machine_sleep":
      return ["succeeded", "failed", "capture_failed", "watchdog_failed"].includes(event.outcome)
        && DURATIONS.includes(event.duration);
    case "update_failed":
      return UPDATE_STAGES.includes(event.stage);
    default:
      return true;
  }
}

async function readEvent(request) {
  const reader = request.body?.getReader();
  if (!reader) return null;
  const bytes = new Uint8Array(MAX_BODY_BYTES);
  let size = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (size + value.byteLength > MAX_BODY_BYTES) {
        void reader.cancel().catch(() => {});
        return null;
      }
      bytes.set(value, size);
      size += value.byteLength;
    }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, size)));
  } catch {
    return null;
  } finally {
    reader.releaseLock();
  }
}

export default {
  async fetch(request, env) {
    if (new URL(request.url).pathname !== EVENTS_PATH) return new Response(null, { status: 404 });
    if (request.method !== "POST") return new Response(null, { status: 405, headers: { Allow: "POST" } });
    if (!/^application\/json(?:\s*;|$)/i.test(request.headers.get("Content-Type") ?? "")) {
      return new Response(null, { status: 415 });
    }
    if (Number(request.headers.get("Content-Length")) > MAX_BODY_BYTES) return new Response(null, { status: 413 });

    const endpoint = POSTHOG_ENDPOINTS[env.POSTHOG_REGION];
    if (typeof endpoint !== "string" || !env.POSTHOG_PROJECT_TOKEN?.startsWith("phc_")) {
      return new Response(null, { status: 503 });
    }
    // Cloudflare provides this header at the edge. Use it only as the rate limit key.
    const key = request.headers.get("CF-Connecting-IP") ?? "unavailable";
    const { success } = await env.INGEST_RATE_LIMIT.limit({ key });
    if (!success) return new Response(null, { status: 429 });

    const payload = await readEvent(request);
    if (!validEvent(payload)) return new Response(null, { status: 400 });

    const { event, install_id, ...properties } = payload;
    try {
      const response = await fetch(endpoint, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          api_key: env.POSTHOG_PROJECT_TOKEN,
          event,
          distinct_id: install_id,
          properties: { ...properties, $process_person_profile: false, $geoip_disable: true },
        }),
        redirect: "manual",
        signal: AbortSignal.timeout(3000),
      });
      if (!response.ok) return new Response(null, { status: 502 });
      const { status } = await response.json();
      return new Response(null, { status: status === "Ok" || status === 1 ? 204 : 502 });
    } catch {
      return new Response(null, { status: 502 });
    }
  },
};
