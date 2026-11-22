// Browser-only stand-in for the Tauri backend so `bun run dev` works without Tauri.
// URL params shape the starting state: ?setup=apiKey|installT3|starting
// ?machines=0|many|<count>  ?busy=1  ?mcp=mixed (Claude Code added, Codex not installed)
// ?projects=0|error (T3 Code has no projects with a remote / listing them fails)
// ?ready=1 (a new machine comes up in T3 Code with its agents signed in and acme on it, as the website shows it)
// ?disk=full (devbox's disk is nearly full)  ?wallet=low
import type { Backend } from "./api";
import { timeLapse } from "./pixel/clock";
import { computeHourly, diskMonthly, monthly } from "./machine";
import type {
  CommandArgs,
  CommandName,
  DiskUsage,
  InstanceType,
  LocalProject,
  MachineDetails,
  MachineProject,
  MachineView,
  Setup,
  Size,
  UiState,
} from "./types";

const params = new URLSearchParams(location.search);

/** Agent37's shapes: $0.80 per vCPU and $0.70 per GB of RAM a month, by the awake hour. */
const SIZES: Size[] = [
  { id: "small", spec: "2 vCPU · 4 GB", cpu: 2, memory: 4, minDisk: 2, maxDisk: 12, defaultDisk: 12, vmOnly: false },
  { id: "medium", spec: "4 vCPU · 8 GB", cpu: 4, memory: 8, minDisk: 2, maxDisk: 20, defaultDisk: 12, vmOnly: false },
  { id: "large", spec: "4 vCPU · 16 GB", cpu: 4, memory: 16, minDisk: 2, maxDisk: 40, defaultDisk: 20, vmOnly: false },
  { id: "xlarge", spec: "4 vCPU · 32 GB", cpu: 4, memory: 32, minDisk: 2, maxDisk: 80, defaultDisk: 32, vmOnly: false },
  { id: "max", spec: "8 vCPU · 64 GB", cpu: 8, memory: 64, minDisk: 2, maxDisk: 80, defaultDisk: 40, vmOnly: true },
].map((s) => ({ ...s, hourly: (s.cpu * 0.8 + s.memory * 0.7) / 730 }));

const shapeOf = (id: string) => {
  const s = SIZES.find((x) => x.id === id);
  if (!s) throw `unknown size ${id} (valid: small, medium, large, xlarge, max)`;
  return s;
};

/** Prices and disk that follow from a shape, instance type and disk size. */
const priced = (size: string, instanceType: InstanceType, diskGb: number) => ({
  size,
  instanceType,
  diskGb,
  idleMonthly: diskMonthly(diskGb),
  hourlyRate: computeHourly(shapeOf(size), instanceType) + diskMonthly(diskGb) / 730,
});

const usage = (usedGb: number, diskGb: number): DiskUsage => ({
  usedBytes: usedGb * 1e9,
  totalBytes: diskGb * 1e9,
  freeBytes: Math.max(0, diskGb - usedGb) * 1e9,
  usedPercent: (usedGb / diskGb) * 100,
});

const asleep = (m: { idleMonthly: number }) => `Asleep · disk only, ${monthly(m.idleMonthly)}`;

type Seed = Pick<MachineView, "id" | "name" | "state" | "detail" | "paired"> &
  Partial<Omit<MachineView, "id" | "name" | "state" | "detail" | "paired">>;
const machine = ({ size = "medium", instanceType = "default", diskGb, ...m }: Seed): MachineView => ({
  ...priced(size, instanceType, diskGb ?? shapeOf(size).defaultDisk),
  region: "us",
  diskUsage: null,
  diskWarning: false,
  githubSignedIn: true,
  countdown: null,
  signedOut: [],
  projects: [],
  keepAwakeUntil: null,
  keepAwakeForever: false,
  waiting: false,
  ...m,
});

const onMachine = (title: string, repoKey: string): MachineProject => ({ id: title, title, repoKey, status: null });

/** T3 Code's projects with a git remote: pinned first, then most recent; most have no favicon. */
const LOCAL_PROJECTS: LocalProject[] = [
  { id: "p1", title: "t3code", path: "/Users/you/Documents/Dev/t3code", repoKey: "github.com/pingdotgg/t3code", icon: null },
  { id: "p2", title: "website", path: "/Users/you/Documents/Dev/website", repoKey: "github.com/you/website", icon: null },
  { id: "p3", title: "acme", path: "/Users/you/Documents/Dev/acme", repoKey: "github.com/you/acme", icon: null },
  { id: "p4", title: "dotfiles", path: "/Users/you/dotfiles", repoKey: "github.com/you/dotfiles", icon: null },
];

/** An idle machine's detail: sleeps 3 minutes from now. */
const idle = () => ({ detail: "Idle · sleeps in 3m", countdown: { template: "Idle · sleeps in {left}", endsAt: Date.now() + 3 * 60_000 } });

const WAKING = { state: "waking", detail: "Waking up…", countdown: null } as const;

/** Kept awake until `endsAt` (unix ms). */
const keptAwake = (endsAt: number) => ({
  state: "idle" as const,
  keepAwakeForever: false,
  keepAwakeUntil: endsAt,
  detail: "Kept awake",
  countdown: { template: "Kept awake · {left} left", endsAt },
});

const seedMachines: MachineView[] = [
  // first: asleep until its T3 Code sends a turn
  machine({
    id: "sb_e4d1",
    name: "devbox",
    size: "small",
    state: "asleep",
    detail: asleep({ idleMonthly: diskMonthly(12) }),
    paired: true,
    projects: [onMachine("acme", "github.com/you/acme")],
    ...(params.get("disk") === "full" ? { diskUsage: usage(11.2, 12), diskWarning: true } : { diskUsage: usage(4.1, 12) }),
  }),
  machine({
    id: "sb_7f2a",
    name: "t3-main",
    size: "large",
    instanceType: "vm",
    diskUsage: usage(13.7, 20),
    state: "working",
    detail: "Working · 2 threads · 3 subagents",
    paired: true,
    projects: [onMachine("t3code", "github.com/pingdotgg/t3code"), onMachine("website", "github.com/you/website")],
  }),
  machine({
    id: "sb_19c0",
    name: "side-quest",
    size: "small",
    region: "eu",
    githubSignedIn: false,
    diskUsage: usage(10.9, 12),
    diskWarning: true,
    state: "idle",
    ...idle(),
    paired: true,
    signedOut: ["Claude", "Codex"],
  }),
  machine({ id: "sb_02bb", name: "scratch", size: "small", diskGb: 4, state: "idle", detail: "Idle · not in T3 Code yet", paired: false, githubSignedIn: false }),
];

const manyExtra: MachineView[] = [
  machine({ id: "sb_aa01", name: "docs-site", size: "small", state: "waking", detail: "Waking up…", paired: true }),
  machine({ id: "sb_aa02", name: "infra", size: "xlarge", instanceType: "performance", region: "ap-south", state: "error", detail: "Couldn't reach machine", paired: true }),
  machine({ id: "sb_aa03", name: "old-spike", size: "small", state: "missing", detail: "Deleted on Agent37", paired: true }),
  machine({ id: "", name: "new-api", size: "medium", state: "creating", detail: "Installing T3 server…", paired: false }),
];

/** Machines with T3 Connect on. */
const connectOn = new Set(["sb_7f2a"]);

/** Per-machine extras get_machine_details adds on top of MachineView. */
const seedDetails: Record<string, { awake: number; month: number; days: number }> = {
  sb_7f2a: { awake: 8040, month: 6.4, days: 13 },
  sb_19c0: { awake: 1260, month: 1.1, days: 4 },
  sb_e4d1: { awake: 0, month: 3.9, days: 30 },
};

function seed(): UiState {
  const m = params.get("machines");
  const all = [...seedMachines, ...manyExtra];
  const count = Number(m);
  const machines =
    m === "many"
      ? all
      : count >= 0 && m
        ? Array.from({ length: count }, (_, i) => {
            const base = all[i % all.length];
            const lap = Math.floor(i / all.length);
            return lap ? { ...base, id: base.id && `${base.id}-${lap}`, name: `${base.name}-${lap + 1}` } : base;
          })
        : seedMachines;
  return {
    setup: (params.get("setup") as Setup | null) ?? "ready",
    setupBusy: params.get("busy") === "1",
    signInCode: null,
    signInUrl: null,
    account: { identifier: "you@example.com", plan: "Pro" },
    machines: structuredClone(machines),
    costs: tally(
      { today: 0.42, month: 12.8, idleMonthly: 0, walletBalance: params.get("wallet") === "low" ? 3.1 : 42.5, awakeRamGb: 0, memoryLimitGb: 32 },
      machines,
    ),
    settings: { startAtLogin: true, template: "agent37-base", autoSleepFuseMinutes: 120 },
    mcp: params.get("mcp") === "mixed" ? { claudeCode: true, codex: null } : { claudeCode: false, codex: false },
    sizes: SIZES,
    update:
      params.get("update") === "1"
        ? {
            currentVersion: "0.2.0",
            latestVersion: "0.2.1",
            updateAvailable: true,
            releaseUrl: "https://github.com/07rjain/t3agent37/releases/tag/v0.2.1",
            changelog: "### Fixed\n- Faster Agent37 provisioning and automatic update checks.",
            publishedAt: new Date().toISOString(),
            assetName: "t3agent37-macos-aarch64.app.tar.gz",
            assetUrl: "https://github.com/07rjain/t3agent37/releases/download/v0.2.1/t3agent37-macos-aarch64.app.tar.gz",
            checksumsUrl: "https://github.com/07rjain/t3agent37/releases/download/v0.2.1/SHA256SUMS.txt",
            canSelfUpdate: true,
            status: "available",
            error: null,
            checkedAt: Math.floor(Date.now() / 1000),
          }
        : {
            currentVersion: "0.2.0",
            latestVersion: "0.2.0",
            updateAvailable: false,
            releaseUrl: "https://github.com/07rjain/t3agent37/releases",
            changelog: null,
            publishedAt: null,
            assetName: null,
            assetUrl: null,
            checksumsUrl: null,
            canSelfUpdate: false,
            status: "upToDate",
            error: null,
            checkedAt: Math.floor(Date.now() / 1000),
          },
  };
}

const AWAKE = new Set<MachineView["state"]>(["working", "idle", "waking", "sleeping"]);

/** The footer's machine-wide numbers, from the machines as they are now. */
function tally(costs: UiState["costs"], machines: MachineView[]): UiState["costs"] {
  const live = machines.filter((m) => m.state !== "missing" && m.state !== "creating");
  return {
    ...costs,
    idleMonthly: live.reduce((n, m) => n + m.idleMonthly, 0),
    awakeRamGb: live.filter((m) => AWAKE.has(m.state)).reduce((n, m) => n + shapeOf(m.size).memory, 0),
  };
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The mock, with T3 Code's side of a machine: what the real backend sees through its proxy. */
export interface MockBackend extends Backend {
  /**
   * A turn sent from T3 Code, a stage at a time for a demo that steps through it: the
   * machine wakes for the message, an agent works on it, and once that is done it idles.
   */
  turn(id: string, stage: "wake" | "work" | "done"): void;
  /**
   * Time flies for an idle machine: its countdown runs out within `ms`, the sea a time lapse
   * meanwhile, then it goes to sleep.
   */
  later(id: string, ms: number): Promise<void>;
}

let shared: MockBackend | null = null;
/** One per page: the UI and whoever plays T3 Code next to it share its state. */
export const mockBackend = (): MockBackend => (shared ??= create());

function create(): MockBackend {
  let state = seed();
  const listeners = new Set<(s: UiState) => void>();

  const emit = () => {
    state.costs = tally(state.costs, state.machines);
    state = structuredClone(state);
    listeners.forEach((l) => l(state));
  };
  const patch = (id: string, p: Partial<MachineView>) => {
    state.machines = state.machines.map((m) => (m.id === id ? { ...m, ...p } : m));
    emit();
  };
  const find = (id: string) => {
    const m = state.machines.find((x) => x.id === id);
    if (!m) throw "machine not found";
    return m;
  };

  const setupOrder: Setup[] = ["apiKey", "installT3", "ready"];
  const nextStep = () => {
    state.setup = setupOrder[setupOrder.indexOf(state.setup) + 1] ?? "ready";
  };

  const handlers: { [C in CommandName]: (a: CommandArgs<C>) => Promise<unknown> } = {
    get_state: async () => structuredClone(state),

    async save_api_key({ apiKey }) {
      await wait(900);
      if (!apiKey.startsWith("sk_")) throw "That isn't an Agent37 API key: they start with sk_live_";
      nextStep();
      emit();
    },

    async create_machine({ name, size, instanceType = "default", region = "us", diskGb, projects }) {
      if (state.machines.some((m) => m.name === name)) throw `a machine called "${name}" already exists`;
      const shape = shapeOf(size);
      if (shape.vmOnly && instanceType === "default") throw `size ${size} needs instance type vm or performance`;
      const disk = diskGb ?? shape.defaultDisk;
      if (disk < shape.minDisk || disk > shape.maxDisk) throw `disk for ${size} must be ${shape.minDisk}–${shape.maxDisk} GB`;
      const row = machine({
        id: "",
        name,
        size,
        instanceType,
        region,
        diskGb: disk,
        githubSignedIn: false,
        state: "creating",
        detail: "Creating sandbox…",
        paired: false,
      });
      state.machines = [...state.machines, row];
      emit();
      const steps = ["Creating sandbox…", "Installing T3 server…", "Starting T3 server…", "Asking T3 Code to add it…"];
      for (const step of steps) {
        state.machines = state.machines.map((m) => (m.name === name ? { ...m, detail: step } : m));
        emit();
        await wait(1800);
      }
      const id = `sb_${Math.random().toString(16).slice(2, 6)}`;
      state.machines = state.machines.map((m) =>
        m.name === name
          ? {
              ...m,
              id,
              state: "idle",
              diskUsage: usage(1.4, disk),
              ...idle(),
              ...(params.get("ready") === "1"
                ? { paired: true, projects: projects?.length ? [] : [onMachine("acme", "github.com/you/acme")] }
                : { signedOut: ["Claude", "Codex"] }),
            }
          : m,
      );
      emit();
      for (const p of projects ?? []) await handlers.add_project({ id, projectId: p });
    },

    async local_projects() {
      await wait(400);
      if (params.get("projects") === "error") throw "T3 Code is not running";
      return params.get("projects") === "0" ? [] : LOCAL_PROJECTS;
    },

    async add_project({ id, projectId }) {
      const p = LOCAL_PROJECTS.find((x) => x.id === projectId);
      if (!p) throw "no such project";
      const set = (status: string | null) =>
        patch(id, { projects: [...find(id).projects.filter((x) => x.repoKey !== p.repoKey), { id: p.id, title: p.title, repoKey: p.repoKey, status }] });
      set("Waking the machine");
      await wait(900);
      for (const pct of [8, 35, 71, 100]) {
        set(`Receiving objects · ${pct}% · 3.1 MiB/s`);
        await wait(700);
      }
      set("Checking out files");
      await wait(600);
      set(null);
    },

    async wake_machine({ id }) {
      find(id);
      patch(id, WAKING);
      await wait(2600);
      patch(id, { state: "idle", ...idle() });
    },

    async sleep_machine({ id }) {
      if (find(id).state === "working") throw "an agent is working";
      patch(id, { state: "sleeping", detail: "Going to sleep…", countdown: null });
      await wait(1800);
      // A stop ends any keep-awake, as on a real machine.
      patch(id, { state: "asleep", detail: asleep(find(id)), keepAwakeUntil: null, keepAwakeForever: false });
    },

    async keep_awake({ id, minutes }) {
      await wait(600);
      patch(
        id,
        minutes === null
          ? { keepAwakeForever: true, keepAwakeUntil: null, state: "idle", detail: "Awake until you stop it", countdown: null }
          : minutes
            ? keptAwake(Date.now() + minutes * 60_000)
            : { keepAwakeForever: false, keepAwakeUntil: null, ...idle() },
      );
    },

    async resize_machine({ id, size, diskGb }) {
      const m = find(id);
      const to = size ?? m.size;
      const shape = shapeOf(to);
      if (shape.vmOnly && m.instanceType === "default") throw `size ${to} needs instance type vm or performance`;
      const disk = diskGb ?? m.diskGb;
      if (disk < m.diskGb) throw "a disk can grow, never shrink";
      if (disk > shape.maxDisk) throw `${to} tops out at ${shape.maxDisk} GB of disk`;
      if (m.state === "working") throw "an agent is working: it resizes once the turn ends";
      // Agent37 recreates the container: an asleep one wakes first, the disk comes along
      if (m.state === "asleep") {
        patch(id, WAKING);
        await wait(2000);
      }
      patch(id, { state: "waking", detail: `Recreating on ${to}${disk !== m.diskGb ? `, ${disk} GB` : ""}…`, countdown: null });
      await wait(2200);
      const used = m.diskUsage?.usedBytes ?? 0;
      const grown = m.diskUsage ? usage(used / 1e9, disk) : null;
      patch(id, {
        ...priced(to, m.instanceType, disk),
        diskUsage: grown,
        diskWarning: !!grown && grown.usedPercent >= 80,
        state: "idle",
        ...idle(),
      });
    },

    async sign_in_github({ id }) {
      find(id);
      state.signInCode = "A1B2-C3D4";
      emit();
      await wait(3500);
      state.signInCode = null;
      patch(id, { githubSignedIn: true });
    },

    async turn_on_connect({ id }) {
      await wait(4000);
      find(id);
      connectOn.add(id);
      emit(); // the details view refetches on every state event
    },

    async sign_in_agent({ id, agent }) {
      await wait(3000);
      patch(id, { signedOut: find(id).signedOut.filter((a) => a !== agent) });
    },

    async open_machine({ id }) {
      const m = find(id);
      if (!m.paired) {
        await wait(1200);
        patch(id, { paired: true, ...idle() });
      }
      // pretend the user finishes the agent sign-ins in T3 Code
      if (m.signedOut.length) setTimeout(() => patch(id, { signedOut: [] }), 4000);
    },

    async pair_machine({ id }) {
      find(id);
      await wait(1200);
      patch(id, { paired: true });
    },

    async delete_machine({ id, discard }) {
      if (find(id).state === "working" && !discard) {
        await wait(400);
        throw "work is still running on it (an agent or a build); wait for it to finish, or delete and discard";
      }
      await wait(500);
      state.machines = state.machines.filter((x) => x.id !== id);
      emit();
    },

    async latest_thread({ id }) {
      await wait(150);
      return id === "sb_7f2a" ? "th_1" : null;
    },

    async get_machine_details({ id }): Promise<MachineDetails> {
      const m = find(id);
      await wait(350);
      const extra = seedDetails[id] ?? { awake: 0, month: 0, days: 0 };
      const awake = extra.awake + (m.state === "working" || m.state === "idle" ? 300 : 0);
      const costToday = (awake / 3600) * m.hourlyRate;
      return {
        id,
        costToday,
        costMonth: extra.month + costToday,
        awakeSecsToday: awake,
        instanceType: m.instanceType,
        region: m.region,
        diskGb: m.diskGb,
        idleMonthly: m.idleMonthly,
        hourlyRate: m.hourlyRate,
        diskUsage: m.diskUsage,
        githubSignedIn: m.githubSignedIn,
        agents: ["Claude", "Codex"].map((name) => ({ name, signedIn: !m.signedOut.includes(name) })),
        t3Version: "0.0.21",
        createdAt: Math.floor(Date.now() / 1000) - extra.days * 86400,
        connect: connectOn.has(id),
      };
    },

    async add_mcp({ client }) {
      if (state.mcp[client] === null) throw "not installed";
      await wait(900);
      state.mcp = { ...state.mcp, [client]: true };
      emit();
    },

    async update_settings({ settings }) {
      state.settings = settings;
      emit();
    },

    async setup_action({ action }) {
      if (action === "openApiKeys") {
        console.info("[mock] open the Agent37 dashboard's API keys page");
        return;
      }
      state.setupBusy = true;
      emit();
      await wait(1600);
      state.setupBusy = false;
      if (action === "recheck" || action === state.setup) nextStep();
      emit();
    },

    async check_for_updates() {
      const current = state.update?.currentVersion ?? "0.2.0";
      state.update = {
        ...(state.update ?? {
          currentVersion: current,
          latestVersion: current,
          updateAvailable: false,
          releaseUrl: "https://github.com/07rjain/t3agent37/releases",
          changelog: null,
          publishedAt: null,
          assetName: null,
          assetUrl: null,
          checksumsUrl: null,
          canSelfUpdate: false,
          status: "idle",
          error: null,
          checkedAt: null,
        }),
        status: "checking",
        error: null,
      };
      emit();
      await wait(450);
      const next = {
        ...state.update,
        status: state.update.updateAvailable ? ("available" as const) : ("upToDate" as const),
        checkedAt: Math.floor(Date.now() / 1000),
      };
      state.update = next;
      emit();
      return next;
    },

    async install_update() {
      if (!state.update) return;
      state.update = { ...state.update, status: "downloading", error: null };
      emit();
      await wait(700);
      state.update = { ...state.update, status: "installing" };
      emit();
      await wait(500);
      const latest = state.update.latestVersion ?? state.update.currentVersion;
      state.update = {
        ...state.update,
        currentVersion: latest,
        updateAvailable: false,
        status: "upToDate",
      };
      emit();
    },

    async open_release_notes() {
      console.info("[mock] open_release_notes", state.update?.releaseUrl);
    },

    async quit_app() {
      console.info("[mock] quit_app");
    },
  };

  // The watchdog: a timed keep-awake that ran out falls back to the idle countdown, and an
  // idle machine sleeps when its countdown runs out.
  setInterval(() => {
    const now = Date.now();
    state.machines
      .filter((m) => m.state === "idle" && m.keepAwakeUntil !== null && m.keepAwakeUntil <= now)
      .forEach((m) => patch(m.id, { keepAwakeUntil: null, ...idle() }));
    state.machines
      .filter((m) => m.state === "idle" && !m.keepAwakeUntil && m.countdown && m.countdown.endsAt <= now)
      .forEach((m) => void handlers.sleep_machine({ id: m.id }));
  }, 500);

  if (state.setup === "starting") {
    setTimeout(() => {
      state.setup = "apiKey";
      emit();
    }, 1500);
  }

  return {
    async call(cmd, args) {
      await wait(120);
      return (handlers[cmd] as (a: typeof args) => Promise<never>)(args);
    },
    subscribe(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    turn(id, stage) {
      patch(
        id,
        stage === "wake"
          ? WAKING
          : stage === "work"
            ? { state: "working", detail: "Working · 1 thread", countdown: null }
            : { state: "idle", ...idle() },
      );
    },
    async later(id, ms) {
      const { countdown } = find(id);
      const left = countdown ? countdown.endsAt - Date.now() : 0;
      const ticks = Math.round(ms / 100);
      timeLapse(left / ms);
      for (let i = 1; countdown && i <= ticks; i++) {
        await wait(ms / ticks);
        // woken, slept or deleted meanwhile: no more time to skip
        if (state.machines.find((m) => m.id === id)?.state !== "idle") break;
        patch(id, { countdown: { ...countdown, endsAt: Date.now() + (left * (ticks - i)) / ticks } });
      }
      timeLapse(1);
      if (state.machines.find((m) => m.id === id)?.state === "idle") await handlers.sleep_machine({ id });
    },
  };
}
