import { useEffect, useState } from "react";
import type { McpClient, Settings, UiState } from "../types";
import { AgentName, AgentNames } from "./AgentLogos";
import { Icon } from "./Icons";
import { Dots } from "./common";
import { SubHeader } from "./SubHeader";

const CLIENTS: { id: McpClient; name: string }[] = [
  { id: "claudeCode", name: "Claude Code" },
  { id: "codex", name: "Codex" },
];

const DEFAULT_TEMPLATE = "agent37-base";

/** Laptop-closed fuse choices, in minutes; null = off. */
const FUSES: { minutes: number | null; label: string }[] = [
  { minutes: null, label: "Off" },
  { minutes: 30, label: "30m" },
  { minutes: 60, label: "1h" },
  { minutes: 120, label: "2h" },
  { minutes: 240, label: "4h" },
];

interface Props {
  state: UiState;
  onSave: (s: Settings) => void;
  /** never rejects; errors are shown by the caller */
  onAddMcp: (client: McpClient) => Promise<void>;
  onQuit: () => void;
  onBack: () => void;
}

export function SettingsView({ state, onSave, onAddMcp, onQuit, onBack }: Props) {
  // optimistic copy; the next "state" event is the source of truth
  const [s, setS] = useState(state.settings);
  useEffect(() => setS(state.settings), [state.settings]);

  // hidden by default so screen recordings don't leak the email
  const [showEmail, setShowEmail] = useState(false);
  const email = state.account?.identifier;
  const plan = state.account?.plan;

  const [adding, setAdding] = useState<McpClient | null>(null);
  const add = (client: McpClient) => {
    setAdding(client);
    onAddMcp(client).finally(() => setAdding(null));
  };
  const missing = CLIENTS.filter((c) => state.mcp[c.id] === null).map((c) => c.name);

  const change = (p: Partial<Settings>) => {
    const next = { ...s, ...p };
    setS(next);
    onSave(next);
  };

  // saved on blur or Enter, not on every keystroke
  const [template, setTemplate] = useState(state.settings.template);
  useEffect(() => setTemplate(state.settings.template), [state.settings.template]);
  const saveTemplate = () => {
    const t = template.trim() || DEFAULT_TEMPLATE;
    setTemplate(t);
    if (t !== s.template) change({ template: t });
  };

  return (
    <div className="view">
      <SubHeader title="Settings" onBack={onBack} />
      <div className="view-body settings">
        <p className="hint">
          Machines sleep 3 minutes after their agents finish and wake by themselves when you use them in T3 Code. Asleep,
          only their disk bills.
        </p>
        <label className="setting setting-row">
          <span className="setting-name">Start at login</span>
          <input
            type="checkbox"
            role="switch"
            className="switch"
            checked={s.startAtLogin}
            onChange={(e) => change({ startAtLogin: e.target.checked })}
          />
        </label>
        <div className="setting">
          <span className="caption">laptop closed</span>
          <span className="setting-name">Sleep every machine after</span>
          <div className="segmented" role="radiogroup" aria-label="Sleep every machine after this long without this computer">
            {FUSES.map((f) => (
              <button
                key={f.label}
                role="radio"
                aria-checked={s.autoSleepFuseMinutes === f.minutes}
                className={s.autoSleepFuseMinutes === f.minutes ? "is-on" : undefined}
                onClick={() => change({ autoSleepFuseMinutes: f.minutes })}
              >
                {f.label}
              </button>
            ))}
          </div>
          <span className="hint">
            A fuse against runaway bills when this computer closes or goes offline. A long turn that runs on with no
            word from here may be checkpointed to sleep, then resumes when you're back.
          </span>
        </div>
        <div className="setting">
          <label className="caption" htmlFor="template">
            template
          </label>
          <span className="prompt-input prompt-input-sm">
            <input
              id="template"
              value={template}
              placeholder={DEFAULT_TEMPLATE}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setTemplate(e.target.value)}
              onBlur={saveTemplate}
              onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
            />
          </span>
          <span className="hint">New machines start from it · a custom t3agent37 template works too</span>
        </div>
        <div className="setting">
          <span className="caption">mcp</span>
          <span className="setting-name">
            Let <AgentNames names={CLIENTS.map((c) => c.name)} /> control t3agent37
          </span>
          <div className="mcp-buttons">
            {CLIENTS.map((c) => {
              const added = state.mcp[c.id];
              return added ? (
                <span key={c.id} className="mcp-added">
                  <Icon name="check" size={10} />
                  <span>
                    Added to <AgentName name={c.name} />
                  </span>
                </span>
              ) : (
                <button
                  key={c.id}
                  className="btn btn-sm btn-outline"
                  disabled={added === null || adding !== null}
                  aria-busy={adding === c.id}
                  onClick={() => add(c.id)}
                >
                  {adding === c.id ? (
                    <>
                      <Dots /> Adding…
                    </>
                  ) : (
                    <span>
                      Add to <AgentName name={c.name} />
                    </span>
                  )}
                </button>
              );
            })}
          </div>
          <span className="hint">
            {missing.length ? (
              <>
                <AgentNames names={missing} /> {missing.length === 1 ? "isn't" : "aren't"} installed on this computer
              </>
            ) : (
              "Their agents can then create, wake and send work to machines"
            )}
          </span>
        </div>
      </div>
      <div className="view-foot settings-foot">
        <span className="setting-value" title="Agent37 account">
          {email &&
            (email.includes("@") ? (
              <button className="account-email" onClick={() => setShowEmail((v) => !v)}>
                {showEmail ? email : "Show email"}
              </button>
            ) : (
              email
            ))}
          {email && plan && " · "}
          {plan}
        </span>
        <button className="btn btn-sm btn-quiet" onClick={onQuit}>
          Quit t3agent37
        </button>
      </div>
    </div>
  );
}
