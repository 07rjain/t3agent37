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
    await onChoose(enabled);
    setBusy(false);
  };

  return (
    <div className="view">
      <div className="view-body">
        <h1 className="display">Share usage and reliability data?</h1>
        <p className="lede">{telemetrySummary}</p>
        <p className="hint">This is optional. You can change your choice in Settings under Privacy.</p>
        <button type="button" className="btn btn-link privacy-link" onClick={onPrivacy}>
          Telemetry privacy details
        </button>
      </div>
      <div className="view-foot privacy-actions">
        <button type="button" className="btn btn-outline" disabled={busy} onClick={() => void choose(false)}>
          Skip
        </button>
        <button type="button" className="btn btn-outline" disabled={busy} onClick={() => void choose(true)}>
          Share telemetry
        </button>
      </div>
    </div>
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
