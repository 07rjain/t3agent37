import { useState, type FormEvent } from "react";
import type { Setup, SetupAction } from "../types";
import { Icon } from "./Icons";
import { Dots } from "./common";

const STEPS: { id: Exclude<Setup, "starting" | "ready">; title: string; body: string }[] = [
  {
    id: "apiKey",
    title: "Add your Agent37 API key",
    body: "Stays on this computer, never inside a machine. Machines bill to your Agent37 wallet.",
  },
  {
    id: "installT3",
    title: "Install T3 Code",
    body: "Your agents' chats live there. t3agent37 adds machines to it.",
  },
];

export function SetupView({
  setup,
  busy,
  onAction,
  onSaveKey,
}: {
  setup: Setup;
  busy: boolean;
  onAction: (a: SetupAction) => void;
  /** never rejects; errors are shown by the caller */
  onSaveKey: (apiKey: string) => Promise<void>;
}) {
  if (setup === "starting" || setup === "ready") {
    return (
      <div className="view center">
        <p className="caption">
          <Dots /> checking your setup
        </p>
      </div>
    );
  }
  const current = STEPS.findIndex((s) => s.id === setup);

  return (
    <div className="view">
      <div className="view-body">
        <h1 className="display">Set sail in two steps</h1>
        <p className="lede">
          Then launch Agent37 machines for T3 Code that sleep when no agent is working. Asleep, only the disk bills.
        </p>
        <ol className="steps">
          {STEPS.map((s, i) => {
            const status = i < current ? "done" : i === current ? "current" : "todo";
            return (
              <li key={s.id} className={`step is-${status}`} aria-current={status === "current" ? "step" : undefined}>
                <span className="step-num">{status === "done" ? <Icon name="check" size={12} /> : `0${i + 1}`}</span>
                <div className="step-text">
                  <span className="step-title">{s.title}</span>
                  {status === "current" ? <span className="step-body">{s.body}</span> : null}
                  {status !== "current" ? null : s.id === "apiKey" ? (
                    <ApiKeyForm busy={busy} onSave={onSaveKey} onGetKey={() => onAction("openApiKeys")} />
                  ) : (
                    <button className="btn btn-solid btn-lg" disabled={busy} aria-busy={busy} onClick={() => onAction("installT3")}>
                      {busy ? (
                        <>
                          <Dots /> Installing
                        </>
                      ) : (
                        "Install T3 Code"
                      )}
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ol>
      </div>
      <div className="view-foot">
        <button className="btn btn-link" disabled={busy} onClick={() => onAction("recheck")}>
          Done it already? Check again
        </button>
      </div>
    </div>
  );
}

function ApiKeyForm({ busy, onSave, onGetKey }: { busy: boolean; onSave: (key: string) => Promise<void>; onGetKey: () => void }) {
  const [key, setKey] = useState("");
  const [shown, setShown] = useState(false);
  const [saving, setSaving] = useState(false);
  const trimmed = key.trim();
  const looksWrong = !!trimmed && !trimmed.startsWith("sk_");
  const working = busy || saving;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!trimmed || working) return;
    setSaving(true);
    onSave(trimmed).finally(() => setSaving(false));
  };

  return (
    <form className="api-key" onSubmit={submit}>
      <span className="prompt-input">
        <input
          type={shown ? "text" : "password"}
          aria-label="Agent37 API key"
          placeholder="sk_live_…"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          spellCheck={false}
          autoComplete="off"
          autoFocus
          aria-invalid={looksWrong}
        />
        <button type="button" className="btn btn-link api-key-show" onClick={() => setShown((s) => !s)}>
          {shown ? "Hide" : "Show"}
        </button>
      </span>
      {looksWrong ? <span className="hint is-error">Agent37 keys start with sk_live_</span> : null}
      <button type="submit" className="btn btn-solid btn-lg" disabled={!trimmed || working} aria-busy={working}>
        {working ? (
          <>
            <Dots /> Checking key
          </>
        ) : (
          "Save API key"
        )}
      </button>
      <button type="button" className="btn btn-link api-key-get" onClick={onGetKey}>
        Get API key <Icon name="open" size={10} />
      </button>
    </form>
  );
}
