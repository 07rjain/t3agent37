// Mirrors docs/ui-contract.md exactly.

export type Setup = "starting" | "ready" | "apiKey" | "installT3";

export type SetupAction = "installT3" | "recheck" | "openApiKeys";

/** Agent37 instance type: `default` container, `vm` full VM with Docker, `performance` dedicated VM. */
export type InstanceType = "default" | "vm" | "performance";

export type Region = "us" | "eu" | "ap-south";

export type MachineState =
  | "working"
  | "idle"
  | "sleeping"
  | "asleep"
  | "waking"
  | "creating"
  | "error"
  | "missing"
  | "unknown";

export interface DiskUsage {
  usedBytes: number;
  totalBytes: number;
  freeBytes: number;
  usedPercent: number;
}

export interface MachineView {
  id: string;
  name: string;
  size: string;
  instanceType: InstanceType;
  region: Region;
  diskGb: number;
  /** disk cost per month: billed awake or asleep, until deleted */
  idleMonthly: number;
  /** what an awake hour costs, compute and disk */
  hourlyRate: number;
  /** as last seen; null until measured */
  diskUsage: DiskUsage | null;
  /** the disk is nearly full: growing it is the fix */
  diskWarning: boolean;
  /** GitHub is signed in there, so private repos clone */
  githubSignedIn: boolean;
  state: MachineState;
  detail: string;
  countdown: Countdown | null; // set while `detail` counts down
  paired: boolean;
  signedOut: string[];
  projects: MachineProject[]; // its T3 projects as last seen (never wakes it), then those being added
  keepAwakeUntil: number | null; // when a "keep awake" runs out (unix ms); null when off or until stopped
  keepAwakeForever: boolean; // kept awake until someone stops it: no auto sleep
  waiting: boolean; // an agent there waits on the user: a resize waits for the next sleep
}

export interface MachineProject {
  id: string; // T3's project id on the machine; while t3agent37 adds it, the local project's id
  title: string;
  repoKey: string | null; // T3's repository key: the same repo has it on every machine
  status: string | null; // set while t3agent37 adds it: clone progress as T3 Code words it
}

/** A T3 Code project on this computer that can go to a machine (it has a git remote). */
export interface LocalProject {
  id: string;
  title: string;
  path: string;
  repoKey: string;
  icon: string | null; // data: URL of the project's favicon in T3 Code
}

/** `detail` as `template` with `{left}` filled in: the time left until `endsAt` (unix ms). */
export interface Countdown {
  template: string;
  endsAt: number;
}

export interface Settings {
  startAtLogin: boolean;
  /** Agent37 template new machines start from */
  template: string;
  /** sleeps every machine after this long with no word from this computer; null = off */
  autoSleepFuseMinutes: number | null;
}

/** An Agent37 compute shape. */
export interface Size {
  id: string;
  spec: string;
  cpu: number;
  memory: number; // GB
  minDisk: number; // GB
  maxDisk: number; // GB
  defaultDisk: number; // GB
  /** compute per awake hour on a `default` container; other instance types multiply it */
  hourly: number;
  /** needs instance type `vm` or `performance` */
  vmOnly: boolean;
}

export type McpClient = "claudeCode" | "codex";

export type UpdateStatus = "idle" | "checking" | "available" | "upToDate" | "downloading" | "installing" | "error";

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  releaseUrl: string | null;
  changelog: string | null;
  publishedAt: string | null;
  assetName: string | null;
  assetUrl: string | null;
  checksumsUrl: string | null;
  canSelfUpdate: boolean;
  status: UpdateStatus;
  error: string | null;
  checkedAt: number | null;
}

export interface Costs {
  today: number;
  month: number;
  /** every machine's disk per month: the floor while all sleep */
  idleMonthly: number;
  walletBalance: number;
  /** memory of the machines awake now, against the workspace's limit */
  awakeRamGb: number;
  memoryLimitGb: number;
}

export interface UiState {
  setup: Setup;
  setupBusy: boolean;
  /** while a GitHub device sign-in waits: the code its page asks for */
  signInCode: string | null;
  /** while an agent OAuth sign-in waits: browser URL fallback */
  signInUrl?: string | null;
  account: { identifier: string | null; plan: string | null } | null;
  machines: MachineView[];
  costs: Costs;
  settings: Settings;
  sizes: Size[];
  /** null = that CLI isn't installed */
  mcp: Record<McpClient, boolean | null>;
  /** Latest GitHub Release check / install status */
  update?: UpdateInfo;
}

export interface MachineDetails {
  id: string;
  costToday: number;
  costMonth: number;
  awakeSecsToday: number;
  agents: { name: string; signedIn: boolean }[];
  t3Version: string;
  createdAt: number; // unix secs
  /** T3 Connect is on: the user's other devices reach it with their T3 account */
  connect: boolean;
  instanceType: InstanceType;
  region: Region;
  diskGb: number;
  idleMonthly: number;
  hourlyRate: number;
  diskUsage: DiskUsage | null;
  githubSignedIn: boolean;
}

export interface Commands {
  get_state: [undefined, UiState];
  save_api_key: [{ apiKey: string }, void]; // stays on this computer, never on a machine
  create_machine: [
    { name: string; size: string; instanceType?: InstanceType; region?: Region; diskGb?: number; projects?: string[] },
    void,
  ]; // projects: LocalProject ids to add once up
  local_projects: [undefined, LocalProject[]]; // pinned first, then most recent
  add_project: [{ id: string; projectId: string }, void]; // resolves once cloned
  wake_machine: [{ id: string }, void];
  sleep_machine: [{ id: string }, void];
  open_machine: [{ id: string; thread?: string }, void];
  pair_machine: [{ id: string }, void];
  delete_machine: [{ id: string; discard?: boolean }, void];
  resize_machine: [{ id: string; size?: string; diskGb?: number }, void]; // disk only grows
  keep_awake: [{ id: string; minutes: number | null }, void]; // null: until stopped; 0: stop
  sign_in_agent: [{ id: string; agent: string }, void];
  sign_in_github: [{ id: string; token?: string }, void]; // no token: browser device login
  turn_on_connect: [{ id: string }, void]; // resolves once reachable, after the browser sign-in
  latest_thread: [{ id: string }, string | null]; // thread id
  get_machine_details: [{ id: string }, MachineDetails];
  add_mcp: [{ client: McpClient }, void];
  update_settings: [{ settings: Settings }, void];
  setup_action: [{ action: SetupAction }, void];
  check_for_updates: [undefined, UpdateInfo];
  install_update: [undefined, void];
  open_release_notes: [undefined, void];
  quit_app: [undefined, void];
}

export type CommandName = keyof Commands;
export type CommandArgs<C extends CommandName> = Commands[C][0];
export type CommandResult<C extends CommandName> = Commands[C][1];
