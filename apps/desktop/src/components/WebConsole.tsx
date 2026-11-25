import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { call, errorText } from "../api";
import type { LocalProject, McpClient, MachineView, SetupAction, UiState } from "../types";
import { machineKey, money, stateTag, type Action } from "../machine";
import { MachineRow } from "./MachineRow";
import { MachineDetails } from "./MachineDetails";
import { NewMachine, type NewMachineSpec } from "./NewMachine";
import { SettingsView } from "./SettingsView";
import { SetupView } from "./SetupView";
import { Icon } from "./Icons";

type Panel = "machine" | "new" | "settings";

const FIRE_START = 34;
const FIRE_END = 46;
const LOW_WALLET = 5;

function without<T>(record: Record<string, T>, key: string): Record<string, T> {
  const { [key]: _, ...rest } = record;
  return rest;
}

/**
 * Full-bleed Agent37 007 gun-barrel ASCII art banner, matching the landing page Hero.
 * Dynamically imports the pre-encoded 14fps ASCII frames so Tauri popover bundles stay lean.
 */
function BondBanner({ children }: { children: ReactNode }) {
  const box = useRef<HTMLElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const el = box.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const ctx = cv.getContext("2d");
    if (!ctx) return;

    let cancelled = false;
    let raf = 0;
    let ro: ResizeObserver | undefined;

    import("../../../web/src/bondAscii").then(({ BOND_FPS, BOND_H, BOND_RLE, BOND_W, decodeBondFrame }) => {
      if (cancelled) return;
      const frames = BOND_RLE.map(decodeBondFrame);
      const reduced = matchMedia("(prefers-reduced-motion: reduce)");
      let width = 0;
      let height = 0;
      let dpr = 1;

      const resize = () => {
        dpr = Math.min(window.devicePixelRatio || 1, 2);
        width = el.clientWidth;
        height = el.clientHeight;
        cv.width = Math.max(1, Math.floor(width * dpr));
        cv.height = Math.max(1, Math.floor(height * dpr));
        cv.style.width = `${width}px`;
        cv.style.height = `${height}px`;
      };

      const drawFrame = (frameIdx: number) => {
        if (!width || !height) return;
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.fillStyle = "#07080b";
        ctx.fillRect(0, 0, width, height);

        const charAspect = 0.6;
        const cellByH = height / BOND_H;
        const cellByW = (width * 0.72) / (BOND_W * charAspect);
        const cellH = Math.max(cellByH, cellByW * 0.92, 6.5);
        const cellW = cellH * charAspect;
        const totalW = BOND_W * cellW;
        const totalH = BOND_H * cellH;
        const offsetX = width >= 820 ? width - totalW * 0.88 : (width - totalW) / 2;
        const offsetY = (height - totalH) / 2;

        const fontSize = Math.max(7.5, cellH * 1.02);
        ctx.font = `500 ${fontSize.toFixed(1)}px "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace`;
        ctx.textBaseline = "top";

        const frame = frames[frameIdx % frames.length];
        const firing = frameIdx >= FIRE_START && frameIdx <= FIRE_END;

        for (let y = 0; y < BOND_H; y++) {
          const py = offsetY + y * cellH;
          if (py + cellH < 0 || py > height) continue;
          const row = frame.slice(y * BOND_W, (y + 1) * BOND_W);
          ctx.fillStyle = firing ? "rgba(248, 196, 196, 0.72)" : "rgba(228, 232, 240, 0.56)";
          ctx.fillText(row, offsetX, py);
        }

        if (firing) {
          const cx = offsetX + totalW * 0.62;
          const cy = offsetY + totalH * 0.52;
          const pulse = ctx.createRadialGradient(cx, cy, 4, cx, cy, Math.max(width, height) * 0.42);
          pulse.addColorStop(0, "rgba(220, 38, 38, 0.24)");
          pulse.addColorStop(0.5, "rgba(220, 38, 38, 0.06)");
          pulse.addColorStop(1, "rgba(7, 8, 11, 0)");
          ctx.fillStyle = pulse;
          ctx.fillRect(0, 0, width, height);
        }

        const leftVeil = ctx.createLinearGradient(0, 0, width * 0.76, 0);
        leftVeil.addColorStop(0, "rgba(7, 8, 11, 0.94)");
        leftVeil.addColorStop(0.45, "rgba(7, 8, 11, 0.78)");
        leftVeil.addColorStop(1, "rgba(7, 8, 11, 0.18)");
        ctx.fillStyle = leftVeil;
        ctx.fillRect(0, 0, width, height);

        const vertVeil = ctx.createLinearGradient(0, 0, 0, height);
        vertVeil.addColorStop(0, "rgba(7, 8, 11, 0.65)");
        vertVeil.addColorStop(0.25, "rgba(7, 8, 11, 0)");
        vertVeil.addColorStop(0.75, "rgba(7, 8, 11, 0)");
        vertVeil.addColorStop(1, "rgba(8, 9, 13, 0.98)");
        ctx.fillStyle = vertVeil;
        ctx.fillRect(0, 0, width, height);
      };

      resize();
      ro = new ResizeObserver(() => {
        resize();
        if (reduced.matches) drawFrame(38);
      });
      ro.observe(el);

      let lastIdx = -1;
      const startMs = performance.now();
      const tick = (now: number) => {
        if (reduced.matches) {
          if (lastIdx !== 38) {
            lastIdx = 38;
            drawFrame(38);
          }
          return;
        }
        const idx = Math.floor(((now - startMs) / 1000) * BOND_FPS) % frames.length;
        if (idx !== lastIdx) {
          lastIdx = idx;
          drawFrame(idx);
        }
        raf = requestAnimationFrame(tick);
      };

      if (reduced.matches) drawFrame(38);
      else raf = requestAnimationFrame(tick);
    });

    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      ro?.disconnect();
    };
  }, []);

  return (
    <header ref={box} className="wc-banner">
      <div className="wc-stage-bg" aria-hidden>
        <canvas ref={canvas} />
        <div className="wc-scanlines" />
      </div>
      <div className="wc-banner-inner">{children}</div>
    </header>
  );
}

function Wordmark({ live = true }: { live?: boolean }) {
  return (
    <div className="wc-wordmark-group">
      <span className="wc-wordmark">
        <span className="wc-badge" aria-hidden>
          37
        </span>
        t3agent37
      </span>
      <span
        className={`wc-live-badge${live ? "" : " is-offline"}`}
        title={live ? "Connected to t3agent37-daemon via real-time SSE" : "Daemon unreachable"}
      >
        {live ? "LIVE · SSE" : "OFFLINE"}
      </span>
    </div>
  );
}

interface ConsoleProps {
  state: UiState | null;
  loadError: string | null;
  retry: () => void;
  toast: (t: string, tone?: "error" | "info") => void;
}

export function WebConsole({ state, loadError, retry, toast }: ConsoleProps) {
  if (loadError) {
    return (
      <div className="wc-page">
        <BondBanner>
          <nav className="wc-nav">
            <Wordmark live={false} />
            <span className="wc-endpoint">127.0.0.1:47730</span>
          </nav>
          <p className="wc-kicker">AGENT37 · LOCALHOST CONTROL PLANE</p>
        </BondBanner>
        <main className="wc-center-wrap">
          <div className="wc-card wc-status-card">
            <span className="wc-card-eyebrow">CONNECTION LOST</span>
            <h1 className="display">Couldn't reach t3agent37-daemon</h1>
            <p className="lede">{loadError}</p>
            <button className="btn wc-cta-btn" onClick={retry}>
              Try again
            </button>
          </div>
        </main>
      </div>
    );
  }

  if (!state) {
    return (
      <div className="wc-page">
        <BondBanner>
          <nav className="wc-nav">
            <Wordmark />
            <span className="wc-endpoint">127.0.0.1:47730</span>
          </nav>
          <p className="wc-kicker">AGENT37 · INITIALIZING TELEMETRY…</p>
        </BondBanner>
      </div>
    );
  }

  if (state.setup !== "ready") {
    const act = (action: SetupAction) => call("setup_action", { action }).catch((e) => toast(errorText(e)));
    const saveKey = (apiKey: string) => call("save_api_key", { apiKey }).catch((e) => toast(errorText(e)));
    return (
      <div className="wc-page">
        <BondBanner>
          <nav className="wc-nav">
            <Wordmark />
            <span className="wc-endpoint">127.0.0.1:47730</span>
          </nav>
          <p className="wc-kicker">AGENT37 · FIRST-RUN BRIEFING</p>
        </BondBanner>
        <main className="wc-center-wrap">
          <div className="wc-card wc-setup-card">
            <SetupView setup={state.setup} busy={state.setupBusy} onAction={act} onSaveKey={saveKey} />
          </div>
        </main>
      </div>
    );
  }

  return <ReadyConsole state={state} toast={toast} />;
}

function ReadyConsole({ state, toast }: { state: UiState; toast: (t: string, tone?: "error" | "info") => void }) {
  const [panel, setPanel] = useState<Panel>("machine");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pickProject, setPickProject] = useState(false);
  const [hover, setHover] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [pending, setPending] = useState<Record<string, string>>({});
  const [deleteErrors, setDeleteErrors] = useState<Record<string, string>>({});

  const canOpen = (m: MachineView) => !!m.id && m.state !== "creating";

  // Keep a valid machine selected in desktop master-detail mode whenever machines exist.
  const openable = useMemo(() => state.machines.filter(canOpen), [state.machines]);
  const selected = useMemo(() => {
    if (selectedId) {
      const found = openable.find((m) => m.id === selectedId);
      if (found) return found;
    }
    return openable[0];
  }, [openable, selectedId]);

  useEffect(() => {
    if (selected && selected.id !== selectedId) {
      setSelectedId(selected.id);
    } else if (!selected && selectedId !== null) {
      setSelectedId(null);
    }
  }, [selected, selectedId]);

  const backToMachine = useCallback(() => setPanel("machine"), []);
  const detailsError = useCallback((text: string) => toast(`Couldn't load details: ${text}`), [toast]);

  const openDetails = (m: MachineView, pick = false) => {
    setPickProject(pick);
    setSelectedId(m.id);
    setPanel("machine");
  };

  const addProject = (m: MachineView, p: LocalProject) =>
    call("add_project", { id: m.id, projectId: p.id }).then(
      () => toast(`${p.title} is on ${m.name}`, "info"),
      (e) => toast(`Couldn't add ${p.title} to ${m.name}: ${errorText(e)}`),
    );

  const track = (m: MachineView, busy: string, job: () => Promise<void>) => {
    const key = machineKey(m);
    setPending((p) => ({ ...p, [key]: busy }));
    job()
      .catch((e) => toast(`${m.name}: ${errorText(e)}`))
      .finally(() => setPending((p) => without(p, key)));
  };

  const act = (m: MachineView, a: Action) =>
    track(m, a.busy, async () => {
      const key = machineKey(m);
      const acceptHint = () => toast(`Switch to T3 Code to accept "${m.name}"`, "info");
      switch (a.cmd) {
        case "delete_machine":
          setDeleteErrors((d) => without(d, key));
          try {
            await call("delete_machine", { id: m.id, discard: a.discard });
          } catch (e) {
            if (a.discard) throw e;
            setDeleteErrors((d) => ({ ...d, [key]: errorText(e) }));
          }
          return;
        case "sign_in_agent":
          await call("sign_in_agent", { id: m.id, agent: a.agent });
          toast(`${a.agent} is signed in on ${m.name}`, "info");
          return;
        case "turn_on_connect":
          await call("turn_on_connect", { id: m.id });
          toast(`T3 Connect is set up for ${m.name}`, "info");
          return;
        case "keep_awake":
          await call("keep_awake", { id: m.id, minutes: a.minutes });
          return;
        case "resize_machine":
          await call("resize_machine", { id: m.id, size: a.size, diskGb: a.diskGb });
          return;
        case "sign_in_github":
          await call("sign_in_github", { id: m.id });
          toast(`GitHub is signed in on ${m.name}`, "info");
          return;
        case "open_machine":
          await call("open_machine", { id: m.id, thread: a.thread });
          if (!m.paired) acceptHint();
          return;
        default:
          await call(a.cmd, { id: m.id });
          if (a.cmd === "pair_machine") acceptHint();
      }
    });

  const addMcp = (client: McpClient) => call("add_mcp", { client }).catch((e) => toast(errorText(e)));

  const create = (spec: NewMachineSpec) => {
    setPanel("machine");
    call("create_machine", spec).catch((e) => toast(errorText(e)));
  };

  const { costs } = state;
  const ramRatio = costs.memoryLimitGb > 0 ? Math.min(1, costs.awakeRamGb / costs.memoryLimitGb) : 0;
  const awakeCount = state.machines.filter((m) => m.state === "working" || m.state === "idle").length;

  return (
    <div className="wc-page">
      <BondBanner>
        <nav className="wc-nav" aria-label="Console navigation">
          <Wordmark />
          <div className="wc-nav-actions">
            <span className="wc-endpoint" title="Local daemon web UI & MCP endpoint">
              127.0.0.1:47730
            </span>
            <button
              type="button"
              className={`wc-nav-btn${panel === "settings" ? " is-active" : ""}`}
              onClick={() => setPanel((p) => (p === "settings" ? "machine" : "settings"))}
            >
              <Icon name="sliders" size={14} />
              Settings
            </button>
            <button
              type="button"
              className="wc-cta-btn"
              onClick={() => setPanel("new")}
            >
              <Icon name="plus" size={13} />
              New machine
            </button>
          </div>
        </nav>

        <div className="wc-hero-meta">
          <p className="wc-kicker">AGENT37 · EDGE-AUTHENTICATED CLOUD SANDBOXES</p>
          <div className="wc-dossier" role="list" aria-label="Live fleet dossier">
            {state.machines.length === 0 && panel !== "new" ? (
              <span className="dossier-chip is-asleep">
                <i className="dossier-dot" aria-hidden />
                <span className="dossier-code">00</span>
                <span className="dossier-name">standby</span>
                <span className="dossier-meta">no active sandboxes</span>
              </span>
            ) : (
              state.machines.map((m, idx) => {
                const key = machineKey(m);
                const active = (panel === "machine" && selected?.id === m.id) || hover === key;
                const code = String(idx + 1).padStart(2, "0");
                const tag = stateTag(m.state);
                const meta = `${m.size} · ${m.region} · ${m.diskGb} GB`;
                const cls = `dossier-chip is-${m.state}${active ? " is-active" : ""}`;
                return canOpen(m) ? (
                  <button
                    key={key}
                    type="button"
                    className={cls}
                    onClick={() => openDetails(m)}
                    title={`${m.name} (${tag}) · ${meta}`}
                  >
                    <i className="dossier-dot" aria-hidden />
                    <span className="dossier-code">{code}</span>
                    <span className="dossier-name">{m.name}</span>
                    <span className="dossier-meta">
                      {tag} · {meta}
                    </span>
                  </button>
                ) : (
                  <span key={key} className={cls} title={`${m.name} (${tag}) · ${meta}`}>
                    <i className="dossier-dot" aria-hidden />
                    <span className="dossier-code">{code}</span>
                    <span className="dossier-name">{m.name}</span>
                    <span className="dossier-meta">
                      {tag} · {meta}
                    </span>
                  </span>
                );
              })
            )}
            {panel === "new" ? (
              <span className="dossier-chip is-waking is-active">
                <i className="dossier-dot" aria-hidden />
                <span className="dossier-code">{String(state.machines.length + 1).padStart(2, "0")}</span>
                <span className="dossier-name">{draftName || "new-machine"}</span>
                <span className="dossier-meta">configuring</span>
              </span>
            ) : null}
          </div>
        </div>
      </BondBanner>

      <div className="wc-shell">
        <section className="wc-kpis" aria-label="Account telemetry and costs">
          <div
            className={`wc-kpi${costs.walletBalance < LOW_WALLET ? " is-warn" : ""}`}
            title="Agent37 pay-as-you-go wallet balance"
          >
            <span className="wc-kpi-label">WALLET BALANCE</span>
            <span className="wc-kpi-value">{money(costs.walletBalance)}</span>
            <span className="wc-kpi-sub">{state.account?.plan ?? "pay-as-you-go"}</span>
          </div>
          <div
            className="wc-kpi"
            title="Every machine's disk per month: billed even while all of them sleep"
          >
            <span className="wc-kpi-label">IDLE DISK / MO</span>
            <span className="wc-kpi-value">{money(costs.idleMonthly)}</span>
            <span className="wc-kpi-sub">
              {state.machines.length} {state.machines.length === 1 ? "machine" : "machines"} · billed asleep
            </span>
          </div>
          <div className="wc-kpi" title="Estimated spend accrued this calendar month">
            <span className="wc-kpi-label">THIS MONTH</span>
            <span className="wc-kpi-value">{money(costs.month)}</span>
            <span className="wc-kpi-sub">{awakeCount > 0 ? `${awakeCount} awake now` : "all sleeping"}</span>
          </div>
          <div
            className={`wc-kpi${ramRatio >= 0.8 ? " is-warn" : ""}`}
            title="Memory of the machines awake now / your Agent37 limit"
          >
            <span className="wc-kpi-label">AWAKE RAM</span>
            <span className="wc-kpi-value">
              {costs.awakeRamGb}
              <span className="wc-kpi-of"> / {costs.memoryLimitGb} GB</span>
            </span>
            <div className="wc-kpi-bar" aria-hidden>
              <i style={{ width: `${Math.round(ramRatio * 100)}%` }} />
            </div>
          </div>
        </section>

        <main className="wc-grid">
          <aside className="wc-card wc-roster" aria-label="Machines">
            <div className="list-head wc-card-head">
              <span className="caption">
                MACHINES <span className="count">{state.machines.length}</span>
              </span>
              <button type="button" className="btn btn-sm btn-solid" onClick={() => setPanel("new")}>
                <Icon name="plus" size={12} /> New machine
              </button>
            </div>
            {state.machines.length === 0 ? (
              <div className="empty wc-roster-empty">
                <h2 className="display">No machines yet</h2>
                <p className="lede">
                  Spin up a pay-as-you-go Agent37 container or VM. It sleeps via CRIU checkpoint when idle.
                </p>
                <button type="button" className="btn wc-cta-btn btn-lg" onClick={() => setPanel("new")}>
                  <Icon name="plus" /> New machine
                </button>
              </div>
            ) : (
              <ul className="list wc-roster-list">
                {state.machines.map((m) => (
                  <MachineRow
                    key={machineKey(m)}
                    machine={m}
                    sizes={state.sizes}
                    pending={pending[machineKey(m)] ?? null}
                    deleteError={deleteErrors[machineKey(m)] ?? null}
                    selected={panel === "machine" && selected?.id === m.id}
                    onAction={(a) => act(m, a)}
                    onDismissDeleteError={() => setDeleteErrors((d) => without(d, machineKey(m)))}
                    onHover={setHover}
                    onOpen={canOpen(m) ? (pick) => openDetails(m, pick) : undefined}
                  />
                ))}
              </ul>
            )}
          </aside>

          <section className="wc-card wc-inspector" aria-label="Workspace">
            {panel === "new" ? (
              <NewMachine state={state} onNameChange={setDraftName} onCreate={create} onBack={backToMachine} />
            ) : panel === "settings" ? (
              <SettingsView
                state={state}
                onSave={(settings) => call("update_settings", { settings }).catch((e) => toast(errorText(e)))}
                onAddMcp={addMcp}
                onCheckUpdates={() =>
                  call("check_for_updates", undefined)
                    .then((info) =>
                      toast(
                        info.updateAvailable && info.latestVersion
                          ? `v${info.latestVersion} available on GitHub`
                          : `Up to date (v${info.currentVersion})`,
                        "info",
                      ),
                    )
                    .catch((e) => toast(errorText(e)))
                }
                onInstallUpdate={() => call("install_update", undefined).catch((e) => toast(errorText(e)))}
                onOpenReleaseNotes={() => call("open_release_notes", undefined).catch((e) => toast(errorText(e)))}
                onBack={backToMachine}
              />
            ) : selected ? (
              <MachineDetails
                key={selected.id}
                machine={selected}
                sizes={state.sizes}
                signInCode={state.signInCode}
                signInUrl={state.signInUrl ?? null}
                version={state}
                pending={pending[machineKey(selected)] ?? null}
                deleteError={deleteErrors[machineKey(selected)] ?? null}
                pickProject={pickProject}
                hideBack
                onAction={(a) => act(selected, a)}
                onAddProject={(p) => addProject(selected, p)}
                onDismissDeleteError={() => setDeleteErrors((d) => without(d, machineKey(selected)))}
                onError={detailsError}
                onBack={backToMachine}
              />
            ) : (
              <MissionBriefing
                creating={state.machines.some((m) => m.state === "creating")}
                onNew={() => setPanel("new")}
              />
            )}
          </section>
        </main>

        <footer className="wc-footer">
          <div className="wc-footer-brand">
            <span className="wc-badge is-sm" aria-hidden>
              37
            </span>
            <span>t3agent37</span>
          </div>
          <div className="wc-footer-meta">
            <span>MCP · http://127.0.0.1:47730/mcp</span>
            <span className="sep">·</span>
            <span>CRIU auto-sleep · edge-authenticated proxies</span>
          </div>
        </footer>
      </div>
    </div>
  );
}

function MissionBriefing({ creating, onNew }: { creating: boolean; onNew: () => void }) {
  return (
    <div className="wc-briefing">
      <span className="wc-card-eyebrow">{creating ? "DEPLOYMENT IN PROGRESS" : "MISSION BRIEFING"}</span>
      <h2 className="wc-briefing-title">
        {creating
          ? "Provisioning your Agent37 machine…"
          : "Run your T3 Code agents in the cloud, on Agent37."}
      </h2>
      <p className="lede">
        {creating
          ? "Your sandbox is booting on Agent37, installing T3 Code + agents, and pairing over localhost edge auth. Its full dossier will appear here once ready."
          : "Create a machine, pair it into T3 Code with one click, and let agents work in the cloud. When idle, CRIU checkpoints RAM to disk so you only pay storage."}
      </p>
      <ol className="wc-steps">
        <li>
          <span className="wc-step-num">01</span>
          <div>
            <strong>Provision on Agent37</strong>
            <p>Choose container or VM, region, CPU/RAM shape, and disk size.</p>
          </div>
        </li>
        <li>
          <span className="wc-step-num">02</span>
          <div>
            <strong>Pair into T3 Code</strong>
            <p>Localhost proxy injects edge credentials and keeps threads cached while asleep.</p>
          </div>
        </li>
        <li>
          <span className="wc-step-num">03</span>
          <div>
            <strong>CRIU auto-sleep</strong>
            <p>Sleeps 3 minutes after agents finish; wakes in ~3.5s on your next message.</p>
          </div>
        </li>
      </ol>
      {!creating ? (
        <button type="button" className="wc-cta-btn" onClick={onNew}>
          <Icon name="plus" size={14} /> Launch your first machine
        </button>
      ) : null}
    </div>
  );
}
