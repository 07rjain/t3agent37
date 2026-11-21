import { useEffect, useRef, type ReactNode } from "react";
import type { MachineState } from "../types";
import { stateTag } from "../machine";

export interface OceanBoat {
  key: string;
  name: string;
  state: MachineState;
  /** tag text instead of the state word */
  label?: string;
  /** clickable (opens its details) */
  pickable?: boolean;
}

interface Props {
  boats: OceanBoat[];
  /** key of the active machine (hovered row, draft machine) */
  active?: string | null;
  /** shorter strip for sub-views */
  compact?: boolean;
  /** a pickable machine node was clicked */
  onPick?: (key: string) => void;
  children?: ReactNode;
}

const ASCII_RAMP = " .:-=+*#%@";

/**
 * Agent37 007 Gun-Barrel ASCII Header: renders an animated rifled barrel spiral in crisp
 * monospace ASCII over obsidian, with interactive tactical agent nodes along the lower strip.
 */
export function Ocean({ boats, active, compact, onPick, children }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const el = host.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const ctx = cv.getContext("2d");
    if (!ctx) return;

    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    let w = 0;
    let h = 0;
    let dpr = 1;

    const resize = () => {
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = el.clientWidth;
      h = el.clientHeight;
      cv.width = Math.max(1, Math.floor(w * dpr));
      cv.height = Math.max(1, Math.floor(h * dpr));
      cv.style.width = `${w}px`;
      cv.style.height = `${h}px`;
    };

    const draw = (t: number) => {
      if (!w || !h) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.fillStyle = "#07080b";
      ctx.fillRect(0, 0, w, h);

      const cellW = 7;
      const cellH = 11;
      const cols = Math.ceil(w / cellW);
      const rows = Math.ceil(h / cellH);
      // Aperture center sits to the right so the t3agent37 title stays crisp on the left
      const cx = w * 0.68;
      const cy = h * 0.48;
      const apertureR = Math.min(w, h) * 0.24;
      const spin = t * 0.55;

      ctx.font = `500 10px "IBM Plex Mono", ui-monospace, monospace`;
      ctx.textBaseline = "top";

      for (let r = 0; r < rows; r++) {
        const py = r * cellH;
        const dy = py + cellH * 0.5 - cy;
        let line = "";
        for (let c = 0; c < cols; c++) {
          const px = c * cellW + cellW * 0.5;
          const dx = px - cx;
          const dist = Math.hypot(dx, dy);
          if (dist < apertureR * 0.92) {
            // Bright inner gun-barrel aperture with tiny centered agent silhouette
            const sx = dx / apertureR;
            const sy = dy / apertureR;
            const inHead = sx * sx + (sy + 0.36) * (sy + 0.36) < 0.045;
            const inTorso = Math.abs(sx) < 0.16 && sy > -0.18 && sy < 0.38;
            const inLegs = Math.abs(sx) < 0.22 && Math.abs(sx) > 0.03 && sy >= 0.38 && sy < 0.82;
            const lum = inHead || inTorso || inLegs ? 0.12 : 0.85;
            line += ASCII_RAMP[Math.min(ASCII_RAMP.length - 1, Math.floor(lum * ASCII_RAMP.length))];
          } else {
            // Rifled spiral grooves radiating out from the aperture
            const angle = Math.atan2(dy, dx);
            const radial = (dist - apertureR) / Math.max(w * 0.55, 1);
            const groove = Math.sin(angle * 7 - Math.log(dist * 0.06 + 1) * 5.2 + spin);
            const rim = Math.exp(-Math.pow((dist - apertureR) / 9, 2)) * 0.55;
            const falloff = Math.max(0, 1 - radial * 0.95);
            const lum = Math.max(0, Math.min(1, (groove * 0.38 + 0.42) * falloff + rim));
            line += ASCII_RAMP[Math.min(ASCII_RAMP.length - 1, Math.floor(lum * (ASCII_RAMP.length - 1)))];
          }
        }
        ctx.fillStyle = "rgba(228, 232, 240, 0.52)";
        ctx.fillText(line, 0, py);
      }

      // Subtle crimson crosshair ring around aperture
      ctx.strokeStyle = "rgba(220, 38, 38, 0.42)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.arc(cx, cy, apertureR, 0, Math.PI * 2);
      ctx.stroke();

      // Left-to-right obsidian veil for title legibility
      const veil = ctx.createLinearGradient(0, 0, w * 0.65, 0);
      veil.addColorStop(0, "rgba(7, 8, 11, 0.86)");
      veil.addColorStop(0.55, "rgba(7, 8, 11, 0.45)");
      veil.addColorStop(1, "rgba(7, 8, 11, 0)");
      ctx.fillStyle = veil;
      ctx.fillRect(0, 0, w, h);

      // Bottom edge fade into the panel
      const bottom = ctx.createLinearGradient(0, h * 0.65, 0, h);
      bottom.addColorStop(0, "rgba(9, 11, 16, 0)");
      bottom.addColorStop(1, "rgba(9, 11, 16, 0.96)");
      ctx.fillStyle = bottom;
      ctx.fillRect(0, h * 0.65, w, h * 0.35);
    };

    resize();
    const ro = new ResizeObserver(() => {
      resize();
      if (reduced.matches) draw(0);
    });
    ro.observe(el);

    let raf = 0;
    let last = 0;
    const start = performance.now();
    const loop = (now: number) => {
      if (now - last >= 70) {
        last = now;
        draw((now - start) / 1000);
      }
      raf = requestAnimationFrame(loop);
    };

    if (reduced.matches) draw(0);
    else raf = requestAnimationFrame(loop);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, []);

  return (
    <div ref={host} className={`ocean${compact ? " is-compact" : ""}`}>
      <canvas ref={canvas} className="ocean-canvas" aria-hidden />
      <div className="ocean-scanlines" aria-hidden />
      {boats.length > 0 ? (
        <div className="agent-strip" role="list" aria-label="Machines">
          {boats.map((b) => {
            const isActive = active === b.key;
            const text = b.label ?? b.name;
            const tag = stateTag(b.state);
            const cls = `agent-pill is-${b.state}${isActive ? " is-active" : ""}`;
            return b.pickable && onPick ? (
              <button
                key={b.key}
                type="button"
                className={cls}
                onClick={() => onPick(b.key)}
                title={`${b.name} · ${tag}`}
              >
                <i className="agent-pill-dot" aria-hidden />
                <span className="agent-pill-name">{text}</span>
                <span className="agent-pill-state">{tag}</span>
              </button>
            ) : (
              <span key={b.key} className={cls} title={`${b.name} · ${tag}`}>
                <i className="agent-pill-dot" aria-hidden />
                <span className="agent-pill-name">{text}</span>
                <span className="agent-pill-state">{tag}</span>
              </span>
            );
          })}
        </div>
      ) : null}
      {children}
    </div>
  );
}
