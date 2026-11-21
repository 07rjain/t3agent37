// t3agent37 watchdog. Runs on the Agent37 sandbox next to the T3 server and tracks when no agent
// works and nobody used it for `sleepAfterSecs`.
//
// Strictly read-only: this sandbox NEVER holds an Agent37 API key and NEVER stops itself.
// To sleep it "drains": it publishes a manifest of the final orchestration state so the local
// t3agent37 proxy can capture it and keep answering T3 Code truthfully while the machine sleeps.
// Once the proxy captures that state and calls POST /sleep-ack, the watchdog transitions to
// `phase = "readyToSleep"` (`readyToSleep: true`), and the proxy itself calls Agent37's sleep API.
// If any activity, busy work, or new orchestration events occur while draining or readyToSleep,
// it immediately reverts to `phase = "active"` (`readyToSleep: false`, `manifest: null`) so the
// proxy knows NOT to sleep the instance.

import { createServer } from "node:http";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, statfsSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { availableParallelism, homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const HOME = homedir();
const STATE_DIR = join(HOME, ".t3agent37");
const CONFIG_PATH = join(STATE_DIR, "config.json");
// On disk, so it survives watchdog restarts (upgrades); a sleep/resume ends it (see BOOT_ID and tick).
const KEEP_AWAKE_PATH = join(STATE_DIR, "keep-awake-until");
const MAX_KEEP_AWAKE_MINUTES = 24 * 60;
const DB_PATH = join(HOME, ".t3", "userdata", "statev2.sqlite");
const T3_ORIGIN = process.env.T3_ORIGIN || "http://127.0.0.1:3773";
const HOST = process.env.WATCHDOG_HOST || process.env.T3AGENT37_BIND_HOST || "0.0.0.0";
const PORT = Number(process.env.WATCHDOG_PORT) || 3774;
const TICK_MS = 5_000;
// A tick that arrives this late means the instance was frozen: a warm resume after a sleep.
const RESUMED_GAP_MS = 30_000;

function loadConfig() {
  return JSON.parse(readFileSync(CONFIG_PATH, "utf8"));
}

let config = { secret: "", t3Token: "", sleepAfterSecs: 180 };
const log = (...args) => console.log(new Date().toISOString(), "[watchdog]", ...args);

export function reloadConfig() {
  try {
    config = loadConfig();
    log(`config reloaded, sleep after ${config.sleepAfterSecs}s`);
    if (!started) start();
  } catch (e) {
    log("config reload failed", e.message);
  }
}

/**
 * draining: the manifest is published, waiting for the proxy to capture state and ack.
 * readyToSleep: the proxy acked the manifest and everything is still idle; the proxy may sleep it.
 * @type {"active" | "draining" | "readyToSleep"}
 */
let phase = "active";
/** Last time anything held the machine up, CPU load included: the idle countdown starts here. */
let lastActivityAt = Date.now();
/** Last time an agent, the user or a keep-awake held it up: CPU load holds it only so long after. */
let lastUsedAt = Date.now();
/** Agent runs in flight (top-level threads and subagents), and runs waiting on a person. */
let working = { threads: 0, subagents: 0, waiting: 0 };
// T3 not answering: an agent might be mid-turn, so count it as working, but only this long, so a
// broken server cannot keep the machine billing.
const UNKNOWN_BUSY_MS = 30 * 60_000;
let unknownSince = 0;
// T3 boots alongside this watchdog: until it first answers it is starting, not failing, and no
// agent can be mid-turn on it.
let t3Answered = false;
const unknownBusy = () => unknownSince > 0 && Date.now() - unknownSince < UNKNOWN_BUSY_MS;
const agentsWorking = () => working.threads + working.subagents > 0;
const agentsBusy = () => agentsWorking() || unknownBusy();

/**
 * One busy check for the tick, "Sleep now", sleep-ack and the delete guard. A failure starts
 * (or continues) "can't tell".
 */
async function refreshWorking() {
  try {
    working = await checkWorking();
    unknownSince = 0;
    t3Answered = true;
  } catch (e) {
    working = { threads: 0, subagents: 0, waiting: 0 };
    if (t3Answered) unknownSince ||= Date.now();
    log(e.message);
  }
}

// A build or job started outside an agent turn (in the background, in a terminal) shows as CPU
// use; an idle dev server does not. Counted in cores, so a single-threaded build counts on any
// size, and smoothed over ~30s so a short spike doesn't count. It holds the machine up for at most
// CPU_HOLD_MS after the last use, so a busy dev server or a stray loop cannot keep it billing;
// longer jobs take a keep-awake.
const CPU_BUSY_CORES = 0.5;
const CPU_HOLD_MS = 30 * 60_000;
const CPU_COUNT = availableParallelism();
/** Cores in use. */
let cpuLoad = 0;
/** "Sleep now" was asked for: CPU load no longer holds the machine up, agents still do. */
let sleepRequested = false;
let cpuPrev = readCpu();
const cpuBusy = () => !sleepRequested && cpuLoad > CPU_BUSY_CORES && Date.now() - lastUsedAt < CPU_HOLD_MS;
const isBusy = () => agentsBusy() || cpuBusy();

/** Busy and total jiffies across all cores since boot, from /proc/stat. */
function readCpu() {
  try {
    const fields = readFileSync("/proc/stat", "utf8").split("\n")[0].trim().split(/\s+/).slice(1).map(Number);
    const idle = fields[3] + (fields[4] ?? 0);
    const total = fields.reduce((a, b) => a + b, 0);
    return { busy: total - idle, total };
  } catch {
    return { busy: 0, total: 0 };
  }
}

function sampleCpu() {
  const now = readCpu();
  const total = now.total - cpuPrev.total;
  const usage = total > 0 ? ((now.busy - cpuPrev.busy) / total) * CPU_COUNT : 0;
  cpuPrev = now;
  cpuLoad = cpuLoad * 0.8 + usage * 0.2;
}

/** Disk usage telemetry for HOME (cached for 30s to keep idle CPU low). */
const DISK_POLL_MS = 30_000;
let cachedDisk = null;
let cachedDiskAt = 0;

function readDisk() {
  const now = Date.now();
  if (cachedDisk !== null && now - cachedDiskAt < DISK_POLL_MS) {
    return cachedDisk;
  }
  let next = null;
  try {
    const stat = statfsSync(HOME);
    const blockSize = stat.frsize || stat.bsize;
    const totalBytes = stat.blocks * blockSize;
    const freeBytes = stat.bavail * blockSize;
    const usedBytes = Math.max(0, (stat.blocks - stat.bfree) * blockSize);
    const usableBytes = usedBytes + freeBytes;
    const usedPercent = usableBytes > 0 ? Math.round((usedBytes / usableBytes) * 1000) / 10 : 0;
    next = { usedBytes, totalBytes, freeBytes, usedPercent };
  } catch {
    try {
      const out = execFileSync("df", ["-kP", HOME], { encoding: "utf8", timeout: 3_000 });
      const lines = out.trim().split("\n");
      const cols = lines[lines.length - 1]?.trim().split(/\s+/) ?? [];
      const totalBytes = Number(cols[1]) * 1024;
      const usedBytes = Number(cols[2]) * 1024;
      const freeBytes = Number(cols[3]) * 1024;
      if (Number.isFinite(totalBytes) && Number.isFinite(usedBytes) && Number.isFinite(freeBytes)) {
        const usableBytes = usedBytes + freeBytes;
        const usedPercent = usableBytes > 0 ? Math.round((usedBytes / usableBytes) * 1000) / 10 : 0;
        next = { usedBytes, totalBytes, freeBytes, usedPercent };
      }
    } catch {
      next = null;
    }
  }
  cachedDisk = next;
  cachedDiskAt = now;
  return next;
}

let manifest = null;
let lastTickAt = Date.now();
/** "Keep awake": no sleeping before this (ms since epoch), working or not; Infinity = until stopped. */
// A stop ends any hold: after a cold resume (new boot) the stored one is void. A warm resume
// keeps the boot, so the resume check in tick() and POST /activity { resumed: true } end it there.
const BOOT_ID = readFileSafe("/proc/sys/kernel/random/boot_id").trim();
const [storedUntil, storedBoot] = readFileSafe(KEEP_AWAKE_PATH).split(" ");
let keepAwakeUntil = storedBoot === BOOT_ID ? Number(storedUntil) || 0 : 0;
const keptAwake = () => Date.now() < keepAwakeUntil;

function readFileSafe(path) {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return "";
  }
}

/** null: until stopped; 0: off. */
function setKeepAwake(minutes) {
  keepAwakeUntil = minutes === null ? Infinity : minutes > 0 ? Date.now() + minutes * 60_000 : 0;
  mkdirSync(STATE_DIR, { recursive: true, mode: 0o700 });
  writeFileSync(KEEP_AWAKE_PATH, `${keepAwakeUntil} ${BOOT_ID}`, { mode: 0o600 });
}

// Work T3 starts on its own at a set time: an automation, a webhook run it held back, an agent
// resuming after a usage limit. Due this soon, the machine waits for it instead of sleeping and
// waking again. Overdue this long, it is not coming (T3 runs due work within seconds), or the
// time stays behind in T3's rows after it ran.
const DUE_SOON_MS = 5 * 60_000;
const OVERDUE_MS = 2 * 60_000;
/** When T3 next starts work on its own, ms since epoch; null for never. The proxy wakes the machine for it. */
let nextDueAt = null;
const dueSoon = () => nextDueAt !== null && nextDueAt - Date.now() < DUE_SOON_MS;

// Events only a person causes, from any client: T3 Code through the proxy, but also a phone or
// another computer through T3 Connect, which the proxy never sees.
const PERSON_EVENTS = [
  "thread.created",
  "thread.visit-recorded",
  "thread.visited",
  "thread.marked-unread",
  "thread.archived",
  "thread.unarchived",
  "thread.deleted",
  "thread.pinned",
  "thread.unpinned",
  "thread.pin-reordered",
  "thread.snoozed",
  "thread.unsnoozed",
  "thread.tags-updated",
  "thread.runtime-mode-updated",
  "thread.interaction-mode-updated",
  "thread.model-selection-updated",
];
/** Last orchestration event looked at; null until the first look. */
let eventCursor = null;

/**
 * Reads T3's database for the tick: sets `nextDueAt`, and says whether a person acted since the
 * last tick. A table this T3 version lacks has nothing due.
 */
function readT3State() {
  const db = new DatabaseSync(DB_PATH, { readOnly: true });
  try {
    const times = (sql) => {
      try {
        return db.prepare(sql).all().map((r) => (typeof r.t === "string" ? Date.parse(r.t) : r.t));
      } catch {
        return [];
      }
    };
    const recovering = `FROM orchestration_v2_projection_threads
      WHERE deleted_at IS NULL AND json_extract(payload_json, '$.limitRecovery.autoResume') = 1`;
    const due = [
      // Parsed here, not compared as text: T3 notes expanded years sort first.
      ...times(`SELECT next_run_at AS t FROM scheduled_tasks
        WHERE enabled = 1 AND next_run_at IS NOT NULL AND last_run_status <> 'running'`),
      ...times(`SELECT MIN(due_at) AS t FROM scheduled_task_webhook_pending WHERE state = 'pending'`),
      ...times(`SELECT json_extract(payload_json, '$.limitRecovery.resetAt') AS t ${recovering}`),
      ...times(`SELECT json_extract(payload_json, '$.snoozedUntil') AS t ${recovering}`),
    ].filter((t) => Number.isFinite(t) && Date.now() - t < OVERDUE_MS);
    nextDueAt = due.length > 0 ? Math.min(...due) : null;

    if (eventCursor === null) {
      eventCursor = db.prepare("SELECT MAX(sequence) AS s FROM orchestration_events").get()?.s ?? 0;
      return false;
    }
    const events = db
      .prepare(
        `SELECT MAX(sequence) AS last, SUM(event_type IN (${PERSON_EVENTS.map(() => "?").join(",")})) AS person
         FROM orchestration_events WHERE sequence > ?`,
      )
      .get(...PERSON_EVENTS, eventCursor);
    eventCursor = events?.last ?? eventCursor;
    return (events?.person ?? 0) > 0;
  } finally {
    db.close();
  }
}

const LIVE_STATUSES = new Set(["queued", "preparing", "starting", "running", "waiting"]);
/** Request kinds T3 settles itself: nobody is waited on. */
const SELF_SETTLED = ["auth_refresh", "dynamic_tool_call"];

/**
 * The run stopped at a prompt only a person answers: an approval, or a question it can't go on
 * past. T3 marks the other questions `blocking: false`: the run works past them, or (left after a
 * restart) T3 takes the answer as a message. It does not hold the machine: nobody should pay while
 * it waits. A question survives the sleep; an approval survives a warm resume, after a cold one T3
 * expires it and the user sends a message to go on.
 */
function waitsOnPerson(thread) {
  const request = thread.pendingRuntimeRequest;
  if (request == null || request.blocking === false) return false;
  if (SELF_SETTLED.includes(request.kind)) return false;
  // A secret or a link card. The shell can't tell them apart, and a link card's agent works on.
  if (request.userOnly) return false;
  // A delegated child's question goes to the agent that delegated it, here or on another machine.
  const delegated = thread.lineage?.relationshipToParent === "subagent" || thread.delegatedFrom != null;
  return !(delegated && request.kind === "user_input");
}

/**
 * The threads among `waiting` whose run works on beside the prompt: anything T3 counts as
 * background work, less the tools a pending prompt holds up. That is a command, a tool or a
 * subagent still running in any run but a rolled-back one, less a persistent tool (it never
 * ends), and the tasks in the provider thread's roster (Claude's background commands). T3 lists
 * none of it while a run is active, so this reads its database. A prompt holds up the tool its
 * approval hangs under (Claude, Codex) or, in ACP, the one in its provider turn it shares a
 * native id with. OpenCode and Pi tie theirs to no tool, so it works on. A subagent always
 * counts: the prompt may be its own, while its parent works on. T3 shows a thread's prompt from
 * any run, and Pi asks outside one: a prompt not in the active run stops nothing, so that run
 * works on. Unreadable: all of them work, so none is cut off.
 */
function workingBesidePrompt(waiting, ownWork) {
  if (waiting.size === 0) return new Set();
  try {
    const db = new DatabaseSync(DB_PATH, { readOnly: true });
    try {
      const rows = db
        .prepare(
          `WITH waiting(thread_id, provider_thread_id, run_id, request_id) AS (
             SELECT json_extract(value, '$.id'), json_extract(value, '$.activeProviderThreadId'),
               json_extract(value, '$.activeRunId'), json_extract(value, '$.requestId')
             FROM json_each(?)
           )
           SELECT waiting.thread_id AS threadId, item.type AS kind,
             json_extract(item.payload_json, '$.childThreadId') AS childThreadId
           FROM waiting
           JOIN orchestration_v2_projection_turn_items AS item ON item.thread_id = waiting.thread_id
           LEFT JOIN orchestration_v2_projection_runs AS run ON run.run_id = item.run_id
           WHERE item.type IN ('command_execution', 'dynamic_tool', 'subagent')
             AND item.status IN ('pending', 'running', 'waiting')
             AND run.status IS NOT 'rolled_back'
             AND json_extract(item.payload_json, '$.input.persistent') IS NOT 1
             AND (item.type = 'subagent' OR NOT EXISTS (
               SELECT 1 FROM orchestration_v2_projection_runtime_requests AS request
               JOIN orchestration_v2_projection_nodes AS gate ON gate.node_id = request.node_id
               WHERE request.thread_id = waiting.thread_id AND request.status = 'pending'
                 AND request.kind NOT IN (${SELF_SETTLED.map((k) => `'${k}'`).join(", ")})
                 AND (gate.parent_node_id = item.node_id
                   OR request.provider_turn_id = item.provider_turn_id
                     AND json_extract(request.payload_json, '$.nativeRequestRef.nativeId')
                       = json_extract(item.payload_json, '$.nativeItemRef.nativeId'))
             ))
           UNION ALL
           SELECT waiting.thread_id, json_extract(task.value, '$.kind'),
             json_extract(task.value, '$.childThreadId')
           FROM waiting
           JOIN orchestration_v2_projection_provider_threads AS roster
             ON roster.thread_id = waiting.thread_id
             AND roster.provider_thread_id = COALESCE(waiting.provider_thread_id, roster.provider_thread_id)
           JOIN json_each(roster.payload_json, '$.pendingBackgroundTasks') AS task
           UNION ALL
           SELECT waiting.thread_id, 'run', NULL
           FROM waiting
           LEFT JOIN orchestration_v2_projection_runtime_requests AS request
             ON request.runtime_request_id = waiting.request_id
           LEFT JOIN orchestration_v2_projection_nodes AS gate ON gate.node_id = request.node_id
           WHERE gate.run_id IS NOT waiting.run_id`,
        )
        .all(
          JSON.stringify(
            Array.from(waiting, (t) => ({
              id: t.id,
              activeProviderThreadId: t.activeProviderThreadId,
              activeRunId: t.activeRunId,
              requestId: t.pendingRuntimeRequest.id,
            })),
          ),
        );
      return new Set(rows.filter(ownWork).map((r) => r.threadId));
    } finally {
      db.close();
    }
  } catch (e) {
    log("background work unavailable", e.message);
    return new Set(Array.from(waiting, (t) => t.id));
  }
}

/**
 * Agent runs in flight and background work, but not runs waiting on a person with nothing else
 * running: those count apart.
 */
async function checkWorking() {
  const res = await fetch(`${T3_ORIGIN}/api/orchestration/shell`, {
    headers: { authorization: `Bearer ${config.t3Token}`, "x-t3-orchestration-protocol": "2" },
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) throw new Error(`shell snapshot: HTTP ${res.status}`);
  const threads = (await res.json()).threads ?? [];
  // A subagent T3 runs as its own thread on this machine counts by its own state: waiting for it
  // holds nothing more, so a child waiting on a person doesn't keep its parent's machine up. A
  // provider's own background agent shows no runs of its own, and one elsewhere isn't seen here:
  // both hold it through the parent.
  const here = new Set(threads.filter((t) => t.creationSource !== "provider").map((t) => t.id));
  const ownWork = (task) => !(task.kind === "subagent" && here.has(task.childThreadId));
  // T3 lists a watched pull request as a task, awaited only while its checks run: after that a
  // person moves it next, so it holds nothing. T3 adds these to the shell's list alone, never to
  // the provider roster read beside a prompt.
  const holds = (task) => ownWork(task) && !(task.taskId.startsWith("pull-request-watch:") && task.awaited !== true);
  // A pending request alone is no run: one left after a restart has none.
  const running = (t) => t.activeRunId != null || t.activityRunStatus != null || LIVE_STATUSES.has(t.status);
  const waiting = new Set(threads.filter((t) => running(t) && waitsOnPerson(t)));
  const besidePrompt = workingBesidePrompt(waiting, ownWork);
  const live = threads.filter(
    (t) =>
      (t.pendingBackgroundTasks ?? []).some(holds) ||
      (running(t) && (!waiting.has(t) || besidePrompt.has(t.id))),
  );
  const subagents = live.filter((t) => t.lineage?.relationshipToParent === "subagent").length;
  return { threads: live.length - subagents, subagents, waiting: waiting.size };
}

/** The orchestration high-water marks T3 itself compares resume cursors against. */
function readManifest() {
  const db = new DatabaseSync(DB_PATH, { readOnly: true });
  try {
    const global = db
      .prepare(
        `SELECT MAX(sequence) AS s FROM orchestration_events
         WHERE aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread')`,
      )
      .get();
    const rows = db
      .prepare(
        `SELECT stream_id AS id, MAX(sequence) AS s FROM orchestration_events
         WHERE application_event_version = 2 AND aggregate_kind = 'thread' GROUP BY stream_id`,
      )
      .all();
    return {
      sequence: global?.s ?? 0,
      threads: Object.fromEntries(rows.map((r) => [r.id, r.s])),
    };
  } finally {
    db.close();
  }
}

/** Publishes the manifest. Only right after a fresh check found no agent working. */
function drain() {
  try {
    manifest = readManifest();
  } catch (e) {
    log("manifest unavailable", e.message);
    manifest = null;
  }
  phase = "draining";
  log("draining", manifest?.sequence ?? null);
}

/** Someone uses the machine: it stays up, and CPU load may hold it up again. */
function touch(reason) {
  lastUsedAt = Date.now();
  hold(reason);
}

/** Keeps the machine up and restarts the idle countdown, calling off a sleep in progress. */
function hold(reason) {
  sleepRequested = false;
  lastActivityAt = Date.now();
  if (phase !== "active") log(`back to active: ${reason}`);
  phase = "active";
  manifest = null;
}

/** Anything happened since the manifest: someone uses the machine after all. */
function newEvents() {
  let current = null;
  try {
    current = readManifest();
  } catch {}
  if (current === null) return false;
  if (manifest === null) return current.sequence > 0;
  return current.sequence !== manifest.sequence;
}

async function tick() {
  const now = Date.now();
  if (now - lastTickAt > RESUMED_GAP_MS) {
    setKeepAwake(0);
    touch("resumed");
  }
  lastTickAt = now;

  sampleCpu();
  await refreshWorking();
  let personActed = false;
  try {
    personActed = readT3State();
  } catch (e) {
    log("T3 state unavailable", e.message);
  }
  if (agentsWorking()) return touch("agent working");
  if (personActed) return touch("used in T3");
  if (keptAwake()) return touch("kept awake");
  if (cpuBusy()) return hold("cpu busy");
  if (dueSoon()) return hold("work due");
  // Can't tell: don't sleep (revert draining/readyToSleep), but do not reset lastActivityAt so
  // flaky checks cannot hold the machine up past UNKNOWN_BUSY_MS.
  if (unknownBusy()) {
    if (phase !== "active") {
      log("back to active: agents unknown");
      phase = "active";
      manifest = null;
    }
    return;
  }
  if (phase !== "active" && newEvents()) return touch("new events");

  const idleMs = now - lastActivityAt;
  if (phase === "active" && idleMs >= config.sleepAfterSecs * 1000) drain();
}

function status() {
  return {
    phase,
    readyToSleep: phase === "readyToSleep",
    busy: isBusy(),
    cpuBusy: !agentsBusy() && cpuBusy(),
    agentsUnknown: unknownBusy(),
    workingThreads: working.threads,
    workingSubagents: working.subagents,
    // Not busy, but a T3 restart would expire what they wait on.
    waitingThreads: working.waiting,
    keepAwakeForever: keepAwakeUntil === Infinity,
    keepAwakeSecs: Number.isFinite(keepAwakeUntil) ? Math.max(0, Math.ceil((keepAwakeUntil - Date.now()) / 1000)) : 0,
    idleSecs: Math.max(0, Math.floor((Date.now() - lastActivityAt) / 1000)),
    sleepAfterSecs: config.sleepAfterSecs,
    nextDueAt,
    workDue: dueSoon(),
    // While draining or readyToSleep, it names the sequence the machine sleeps at: the proxy
    // trusts its capture only when they match.
    manifest: phase === "active" ? null : manifest,
    disk: readDisk(),
  };
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = "";
    req.on("data", (c) => {
      body += c;
      if (body.length > 64_000) req.destroy();
    });
    req.on("end", () => resolve(body));
    req.on("error", reject);
  });
}

const server = createServer(async (req, res) => {
  const reply = (code, value) => {
    res.writeHead(code, { "content-type": "application/json" });
    res.end(value === undefined ? "" : JSON.stringify(value));
  };
  if (req.headers.authorization !== `Bearer ${config.secret}`) return reply(401, { error: "unauthorized" });
  try {
    const url = new URL(req.url ?? "/", "http://127.0.0.1");
    const path = url.pathname;
    const fresh = url.searchParams.has("fresh");

    if (req.method === "GET" && (path === "/status" || path === "/t3boat-status")) {
      if (fresh) {
        // The delete guard and wake check: agents as of now, not as of the last tick.
        await refreshWorking();
      }
      return reply(200, status());
    }
    if (req.method === "POST" && path === "/activity") {
      // { resumed }: the proxy just resumed the sandbox, so end any prior keep-awake and return to active.
      const { resumed } = JSON.parse((await readBody(req)) || "{}");
      if (resumed) {
        setKeepAwake(0);
        touch("resumed");
        return reply(200, status());
      }
      touch("user activity");
      return reply(200, status());
    }
    if (req.method === "POST" && path === "/drain") {
      // "Sleep now" from the tray. Never mid-turn; it does end a keep-awake.
      await refreshWorking();
      // A busy CPU alone doesn't refuse: "Sleep now" is the user's explicit call.
      if (agentsBusy()) {
        const error = agentsWorking() ? "an agent is working" : "could not check agents";
        return reply(409, { error, ...status() });
      }
      try {
        readT3State();
      } catch {}
      if (dueSoon()) {
        return reply(409, { error: "scheduled work is due soon", ...status() });
      }
      setKeepAwake(0);
      if (phase === "active") drain();
      sleepRequested = true;
      return reply(200, status());
    }
    if (req.method === "POST" && path === "/keep-awake") {
      // { minutes }: null = until stopped, 0 ends it. Counted on this machine's clock, never the laptop's.
      const { minutes } = JSON.parse((await readBody(req)) || "{}");
      const valid = minutes === null || (Number.isInteger(minutes) && minutes >= 0 && minutes <= MAX_KEEP_AWAKE_MINUTES);
      if (!valid) return reply(400, { error: "minutes must be null (until stopped) or 0 to " + MAX_KEEP_AWAKE_MINUTES });
      setKeepAwake(minutes);
      if (minutes !== 0) touch("keep awake");
      return reply(200, status());
    }
    if (req.method === "POST" && path === "/sleep-ack") {
      // The proxy captured this state and wants confirmation to sleep the instance via Agent37 API.
      // Re-verify nothing started working or changed since the manifest before marking readyToSleep.
      const { sequence } = JSON.parse((await readBody(req)) || "{}");
      await refreshWorking();
      let personActed = false;
      try {
        personActed = readT3State();
      } catch (e) {
        log("T3 state unavailable", e.message);
      }
      if (agentsWorking()) {
        touch("agent working");
        return reply(200, status());
      }
      if (personActed) {
        touch("used in T3");
        return reply(200, status());
      }
      if (keptAwake()) {
        touch("kept awake");
        return reply(200, status());
      }
      if (cpuBusy()) {
        hold("cpu busy");
        return reply(200, status());
      }
      if (dueSoon()) {
        hold("work due");
        return reply(200, status());
      }
      if (unknownBusy()) {
        if (phase !== "active") {
          log("back to active: agents unknown");
          phase = "active";
          manifest = null;
        }
        return reply(200, status());
      }
      if (phase !== "active" && newEvents()) {
        touch("new events");
        return reply(200, status());
      }
      if (phase === "draining" || phase === "readyToSleep") {
        const matches = manifest !== null ? sequence === manifest.sequence : sequence == null;
        if (matches) {
          if (phase !== "readyToSleep") log("ready to sleep", manifest?.sequence ?? null);
          phase = "readyToSleep";
        }
      }
      return reply(200, status());
    }
    reply(404, { error: "not found" });
  } catch (e) {
    reply(400, { error: e.message });
  }
});

let loopTimer = null;
let listenRetryTimer = null;
let stopped = false;
let started = false;

server.on("error", (err) => {
  log(`watchdog HTTP error (${err.code || err.message}); retrying listen in 2s`);
  if (stopped || listenRetryTimer) return;
  listenRetryTimer = setTimeout(() => {
    listenRetryTimer = null;
    if (!stopped && !server.listening) {
      server.listen(PORT, HOST);
    }
  }, 2_000);
});

// One tick at a time: a slow T3 check must never overlap a newer tick.
async function loop() {
  if (stopped) return;
  await tick().catch((e) => log("tick failed", e.message));
  if (!stopped) loopTimer = setTimeout(loop, TICK_MS);
}

export function start() {
  if (started) return;
  config = loadConfig();
  started = true;
  stopped = false;
  server.listen(PORT, HOST, () => log(`watchdog on ${HOST}:${PORT}, sleep after ${config.sleepAfterSecs}s`));
  loop();
}

export function stop() {
  if (stopped) return Promise.resolve();
  stopped = true;
  if (loopTimer) {
    clearTimeout(loopTimer);
    loopTimer = null;
  }
  if (listenRetryTimer) {
    clearTimeout(listenRetryTimer);
    listenRetryTimer = null;
  }
  return new Promise((resolve) => {
    if (!started || !server.listening) return resolve();
    server.close(() => resolve());
    server.closeAllConnections?.();
  });
}

const isMain = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isMain) {
  start();
  const exit = (signal) => {
    log(`received ${signal}: exiting`);
    stop().then(() => process.exit(0));
    setTimeout(() => process.exit(0), 2_000).unref();
  };
  process.on("SIGTERM", () => exit("SIGTERM"));
  process.on("SIGINT", () => exit("SIGINT"));
}
