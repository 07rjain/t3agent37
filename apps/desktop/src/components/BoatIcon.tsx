import type { MachineState } from "../types";

/** Agent37 monochrome/crimson tactical aperture emblem for machine list rows and details. */
export function BoatIcon({ state }: { state: MachineState; seed?: number }) {
  return (
    <span className={`boat-icon agent-badge is-${state}`} aria-hidden>
      <svg viewBox="0 0 28 28" fill="none" stroke="currentColor" strokeWidth="1.6">
        <circle cx="14" cy="14" r="10.5" strokeDasharray={state === "asleep" || state === "missing" ? "3 2.5" : undefined} />
        <line x1="14" y1="1.5" x2="14" y2="6" />
        <line x1="14" y1="22" x2="14" y2="26.5" />
        <line x1="1.5" y1="14" x2="6" y2="14" />
        <line x1="22" y1="14" x2="26.5" y2="14" />
        <circle cx="14" cy="14" r="3" fill="currentColor" stroke="none" />
      </svg>
    </span>
  );
}
