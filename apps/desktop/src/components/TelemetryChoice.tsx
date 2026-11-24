import { useState } from "react";

const telemetrySummary =
  "Share setup, sleep, wake, and update results with the app version, system details, and a random installation ID.";

export function TelemetryChoice({
  onChoose,
  onPrivacy,
}: {
  onChoose: (enabled: boolean) => Promise<void>;
  onPrivacy: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const choose = async (enabled: boolean) => {
    setBusy(true);
    try {
      await onChoose(enabled);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="view" aria-labelledby="telemetry-title">
      <div className="view-body privacy-choice">
        <div className="privacy-content">
          <h1 className="display" id="telemetry-title">Share optional telemetry?</h1>
          <p className="lede">{telemetrySummary}</p>
          <p className="hint">You can change this anytime in Settings under Privacy.</p>
          <button type="button" className="btn btn-link privacy-link" onClick={onPrivacy}>
            Telemetry privacy details
          </button>
          <div className="privacy-actions">
            <button type="button" className="btn btn-lg btn-quiet" disabled={busy} onClick={() => void choose(false)}>
              Skip
            </button>
            <button type="button" className="btn btn-lg btn-solid" disabled={busy} onClick={() => void choose(true)}>
              Share telemetry
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}

export function TelemetrySetting({
  enabled,
  onChoose,
  onPrivacy,
}: {
  enabled: boolean;
  onChoose: (enabled: boolean) => Promise<void>;
  onPrivacy: () => void;
}) {
  const [busy, setBusy] = useState(false);

  return (
    <div className="setting">
      <span className="caption">Privacy</span>
      <label className="setting-row privacy-switch">
        <span className="setting-name">Share optional telemetry</span>
        <input
          type="checkbox"
          role="switch"
          className="switch"
          checked={enabled}
          disabled={busy}
          aria-describedby="telemetry-summary"
          onChange={(e) => {
            setBusy(true);
            void onChoose(e.target.checked).finally(() => setBusy(false));
          }}
        />
      </label>
      <span id="telemetry-summary" className="hint">{telemetrySummary}</span>
      <button type="button" className="btn btn-sm btn-link privacy-link" onClick={onPrivacy}>
        Telemetry privacy details
      </button>
    </div>
  );
}
