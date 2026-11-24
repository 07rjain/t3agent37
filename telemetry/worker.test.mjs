import assert from "node:assert/strict";
import test from "node:test";
import worker from "./worker.mjs";

const endpoint = "https://metrics.example/v1/events";
const installId = "39ecad95ff244dd1a1a04dc338f765f2";
const base = { install_id: installId, app_version: "0.2.0", os: "macos", os_major: 26, arch: "aarch64" };
const environment = {
  POSTHOG_PROJECT_TOKEN: "phc_test",
  POSTHOG_REGION: "eu",
  INGEST_RATE_LIMIT: { limit: async () => ({ success: true }) },
};

function post(event) {
  return new Request(endpoint, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "cf-connecting-ip": "192.0.2.1",
      "user-agent": "a custom user agent",
    },
    body: JSON.stringify(event),
  });
}

test("forwards only approved event properties and opts out of person and location processing", async (t) => {
  const upstream = t.mock.method(globalThis, "fetch", async () => Response.json({ status: "Ok" }));
  const limit = t.mock.fn(async () => ({ success: true }));
  const props = { outcome: "capture_failed", duration: "5_to_29s" };
  const response = await worker.fetch(
    post({ ...base, event: "machine_sleep", ...props }),
    { ...environment, INGEST_RATE_LIMIT: { limit } },
  );

  assert.equal(response.status, 204);
  assert.deepEqual(limit.mock.calls[0].arguments, [{ key: "192.0.2.1" }]);
  const [url, request] = upstream.mock.calls[0].arguments;
  assert.equal(url, "https://eu.i.posthog.com/i/v0/e/");
  assert.deepEqual(request.headers, { "Content-Type": "application/json" });
  assert.equal(request.redirect, "manual");
  const { install_id, ...metadata } = base;
  assert.deepEqual(JSON.parse(request.body), {
    api_key: "phc_test",
    event: "machine_sleep",
    distinct_id: install_id,
    properties: { ...metadata, ...props, $process_person_profile: false, $geoip_disable: true },
  });
});

test("rejects malformed, oversized, and rate-limited requests before forwarding", async (t) => {
  const upstream = t.mock.method(globalThis, "fetch");
  for (const data of [
    { ...base, event: "first_launch", prompt: "private text" },
    { ...base, event: "first_launch", install_id: "bad ID" },
    { ...base, event: "first_launch", os_major: "26.2" },
    { ...base, event: "first_launch", app_version: "0.2.0\n" },
    { ...base, event: "machine_wake", outcome: "capture_failed", duration: "under_5s" },
    { ...base, event: "machine_wake", outcome: "failed", duration: 1234 },
    { ...base, event: "update_failed", stage: "private error details" },
    { ...base, event: "$identify" },
    { ...base, event: "first_launch", app_version: "x".repeat(1024) },
  ]) {
    assert.equal((await worker.fetch(post(data), environment)).status, 400);
  }
  const reject = { ...environment, INGEST_RATE_LIMIT: { limit: async () => ({ success: false }) } };
  assert.equal((await worker.fetch(post({ ...base, event: "first_launch" }), reject)).status, 429);
  assert.equal((await worker.fetch(new Request(endpoint), environment)).status, 405);
  assert.equal(upstream.mock.callCount(), 0);
});

test("reports ingestion failure without returning upstream response contents", async (t) => {
  t.mock.method(globalThis, "fetch", async () => Response.json({ status: 0, error: "upstream details" }));
  const response = await worker.fetch(post({ ...base, event: "update_succeeded" }), environment);
  assert.equal(response.status, 502);
  assert.equal(await response.text(), "");
});

test("rejects an upstream redirect and keeps the ingestion token on the original request", async (t) => {
  const upstream = t.mock.method(globalThis, "fetch", async () => new Response(null, {
    status: 307,
    headers: { Location: "https://other.example/events" },
  }));
  const response = await worker.fetch(post({ ...base, event: "update_succeeded" }), environment);
  assert.equal(response.status, 502);
  assert.equal(upstream.mock.callCount(), 1);
  assert.equal(upstream.mock.calls[0].arguments[1].redirect, "manual");
});
