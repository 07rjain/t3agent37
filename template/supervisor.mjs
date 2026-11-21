// t3agent37 lightweight zero-dependency process supervisor.
// Works identically on Agent37 `default` containers (no systemd) and `vm` / `performance` instances.
//
// Manages:
//   - t3 serve --port 3773 --host 0.0.0.0          (child process, when $HOME/.local/bin/t3 exists)
//   - $HOME/.t3agent37/watchdog.mjs                (in-process module when watchdog.mjs & config.json exist)
//
// Signals:
//   - SIGHUP:  restart t3 and reload watchdog config
//   - SIGUSR1: reload watchdog config in-process (and start t3 if installed and not running)
//   - SIGUSR2: restart t3 only (and start watchdog if ready)
//   - SIGTERM / SIGINT: forward SIGTERM to children, wait for clean exit, remove pidfile

import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const HOME = homedir();
const STATE_DIR = join(HOME, ".t3agent37");
const PID_PATH = join(STATE_DIR, "supervisor.pid");

const T3_BIN = join(HOME, ".local", "bin", "t3");
const T3_PORT = process.env.T3_PORT || "3773";
const T3_HOST = process.env.T3_HOST || process.env.T3AGENT37_BIND_HOST || "0.0.0.0";
const T3_LOG = join(STATE_DIR, "t3.log");
const T3_PID_PATH = join(STATE_DIR, "t3.pid");

const WATCHDOG_SCRIPT = join(STATE_DIR, "watchdog.mjs");
const WATCHDOG_CONFIG = join(STATE_DIR, "config.json");
const WATCHDOG_PID_PATH = join(STATE_DIR, "watchdog.pid");

const POLL_MS = 2_000;
const MIN_BACKOFF_MS = 1_000;
const MAX_BACKOFF_MS = 30_000;
const STABLE_UPTIME_MS = 30_000;
const STOP_GRACE_MS = 5_000;

mkdirSync(STATE_DIR, { recursive: true, mode: 0o700 });
writeFileSync(PID_PATH, `${process.pid}\n`, { mode: 0o600 });

const log = (...args) => console.log(new Date().toISOString(), "[supervisor]", ...args);

let shuttingDown = false;

function removePidIfOwned(path, expectedPid) {
  try {
    if (readFileSync(path, "utf8").trim() === String(expectedPid)) {
      rmSync(path, { force: true });
    }
  } catch {}
}

class ManagedService {
  constructor({ name, isAvailable, command, args, logPath, pidPath, env }) {
    this.name = name;
    this.isAvailable = isAvailable;
    this.command = command;
    this.args = args;
    this.logPath = logPath;
    this.pidPath = pidPath;
    this.env = env;

    this.child = null;
    this.startedAt = 0;
    this.backoffMs = MIN_BACKOFF_MS;
    this.restartTimer = null;
    this.killTimer = null;
    this.stopping = false;
    this.restartAfterExit = false;
  }

  ensureRunning() {
    if (shuttingDown || this.child || this.restartTimer) return;
    if (!this.isAvailable()) return;
    this.start();
  }

  start() {
    if (shuttingDown || this.child) return;
    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }
    if (!this.isAvailable()) return;

    let logFd;
    try {
      mkdirSync(STATE_DIR, { recursive: true, mode: 0o700 });
      logFd = openSync(this.logPath, "a", 0o600);
    } catch (e) {
      log(`${this.name}: failed to open log ${this.logPath}:`, e.message);
      this.scheduleRestart();
      return;
    }

    let child;
    try {
      child = spawn(this.command, this.args, {
        cwd: HOME,
        env: this.env(),
        stdio: ["ignore", logFd, logFd],
      });
    } catch (e) {
      try {
        closeSync(logFd);
      } catch {}
      log(`${this.name}: spawn failed:`, e.message);
      this.scheduleRestart();
      return;
    }

    try {
      closeSync(logFd);
    } catch {}

    this.child = child;
    this.startedAt = Date.now();
    this.stopping = false;
    this.restartAfterExit = false;
    const childPid = child.pid;
    if (childPid) {
      try {
        writeFileSync(this.pidPath, `${childPid}\n`, { mode: 0o600 });
      } catch {}
    }
    log(`${this.name}: started (pid ${childPid ?? "unknown"})`);

    child.on("error", (err) => {
      log(`${this.name}: process error:`, err.message);
      if (this.child === child) {
        if (this.killTimer) {
          clearTimeout(this.killTimer);
          this.killTimer = null;
        }
        if (childPid) removePidIfOwned(this.pidPath, childPid);
        this.child = null;
        this.stopping = false;
        this.restartAfterExit = false;
        if (!shuttingDown) this.scheduleRestart();
      }
    });

    child.on("exit", (code, signal) => {
      if (this.killTimer) {
        clearTimeout(this.killTimer);
        this.killTimer = null;
      }
      if (childPid) removePidIfOwned(this.pidPath, childPid);

      const uptime = Date.now() - this.startedAt;
      const shouldRestartNow = this.restartAfterExit;
      this.child = null;
      this.stopping = false;
      this.restartAfterExit = false;

      log(`${this.name}: exited (code=${code}, signal=${signal}, uptime=${Math.round(uptime / 1000)}s)`);

      if (shuttingDown) return;
      if (shouldRestartNow) {
        this.backoffMs = MIN_BACKOFF_MS;
        this.start();
        return;
      }
      if (uptime >= STABLE_UPTIME_MS) {
        this.backoffMs = MIN_BACKOFF_MS;
      }
      this.scheduleRestart();
    });
  }

  scheduleRestart() {
    if (shuttingDown || this.restartTimer || this.child) return;
    if (!this.isAvailable()) {
      this.backoffMs = MIN_BACKOFF_MS;
      return;
    }
    const delay = this.backoffMs;
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
    log(`${this.name}: restarting in ${delay}ms`);
    this.restartTimer = setTimeout(() => {
      this.restartTimer = null;
      this.start();
    }, delay);
  }

  restart() {
    if (shuttingDown) return;
    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }
    this.backoffMs = MIN_BACKOFF_MS;

    if (!this.child) {
      this.start();
      return;
    }

    this.restartAfterExit = true;
    this.stopChild();
  }

  stopChild() {
    if (!this.child || this.stopping) return;
    this.stopping = true;
    const child = this.child;
    log(`${this.name}: sending SIGTERM (pid ${child.pid ?? "unknown"})`);
    try {
      child.kill("SIGTERM");
    } catch {}
    this.killTimer = setTimeout(() => {
      this.killTimer = null;
      if (this.child === child) {
        log(`${this.name}: did not exit in ${STOP_GRACE_MS}ms, sending SIGKILL`);
        try {
          child.kill("SIGKILL");
        } catch {}
      }
    }, STOP_GRACE_MS);
  }

  stopForShutdown() {
    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }
    this.restartAfterExit = false;
    if (!this.child) return Promise.resolve();
    const child = this.child;
    return new Promise((resolve) => {
      child.once("exit", () => resolve());
      this.stopChild();
    });
  }
}

const t3Service = new ManagedService({
  name: "t3",
  isAvailable: () => existsSync(T3_BIN),
  command: T3_BIN,
  args: ["serve", "--port", T3_PORT, "--host", T3_HOST],
  logPath: T3_LOG,
  pidPath: T3_PID_PATH,
  env: () => ({
    ...process.env,
    HOME,
    PATH: `${join(HOME, ".local", "bin")}:${process.env.PATH || "/usr/local/bin:/usr/bin:/bin"}`,
  }),
});

// Run watchdog.mjs IN-PROCESS inside this single Node runtime (saves ~130 MB RSS vs spawning a
// second V8 process per sandbox).
let watchdogMod = null;
let watchdogLoading = false;

async function ensureWatchdog() {
  if (shuttingDown || watchdogMod || watchdogLoading) return;
  if (!existsSync(WATCHDOG_SCRIPT) || !existsSync(WATCHDOG_CONFIG)) return;
  watchdogLoading = true;
  try {
    const mod = await import(pathToFileURL(WATCHDOG_SCRIPT).href);
    mod.start();
    watchdogMod = mod;
    writeFileSync(WATCHDOG_PID_PATH, `${process.pid}\n`, { mode: 0o600 });
    log("watchdog started in-process");
  } catch (e) {
    log("failed to load watchdog in-process:", e?.message ?? String(e));
  } finally {
    watchdogLoading = false;
  }
}

process.on("SIGHUP", () => {
  log("received SIGHUP: restarting t3 and reloading watchdog config");
  t3Service.restart();
  if (watchdogMod) watchdogMod.reloadConfig();
  else void ensureWatchdog();
});

process.on("SIGUSR1", () => {
  log("received SIGUSR1: reloading watchdog config in-process");
  t3Service.ensureRunning();
  if (watchdogMod) watchdogMod.reloadConfig();
  else void ensureWatchdog();
});

process.on("SIGUSR2", () => {
  log("received SIGUSR2: restarting t3");
  t3Service.restart();
  void ensureWatchdog();
});

async function shutdown(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  clearInterval(pollTimer);
  log(`received ${signal}: shutting down`);
  await Promise.all([
    t3Service.stopForShutdown(),
    watchdogMod ? watchdogMod.stop() : Promise.resolve(),
  ]);
  removePidIfOwned(WATCHDOG_PID_PATH, process.pid);
  removePidIfOwned(PID_PATH, process.pid);
  process.exit(0);
}

process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));
process.on("uncaughtException", (err) => {
  log("uncaughtException (keeping supervisor alive):", err?.stack ?? String(err));
});
process.on("unhandledRejection", (reason) => {
  log("unhandledRejection (keeping supervisor alive):", reason?.stack ?? String(reason));
});
process.on("exit", () => {
  removePidIfOwned(WATCHDOG_PID_PATH, process.pid);
  removePidIfOwned(PID_PATH, process.pid);
});

log(`started (pid ${process.pid}), home=${HOME}`);
t3Service.ensureRunning();
void ensureWatchdog();

const pollTimer = setInterval(() => {
  t3Service.ensureRunning();
  void ensureWatchdog();
}, POLL_MS);
