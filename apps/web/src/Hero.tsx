// Full-bleed James Bond 007 gun-barrel ASCII art hero for t3agent37.
// Decodes the 14fps pre-encoded ASCII frames from bondAscii.ts onto a high-DPI canvas with
// subtle CRT scanlines, radial obsidian vignette, and live Agent37 dossier telemetry badges.
import { useEffect, useRef, type ReactNode } from "react";
import { BOND_FPS, BOND_H, BOND_RLE, BOND_W, decodeBondFrame } from "./bondAscii";

const FRAMES = BOND_RLE.map(decodeBondFrame);
// Frames around Bond's turn & fire where a subtle crimson reticle tint pulses.
const FIRE_START = 34;
const FIRE_END = 46;
const EDGE_BLEED_COLS = 12;

const DOSSIER = [
  { code: "01", name: "mi6-dev", state: "working", meta: "2 vCPU · 4 GB" },
  { code: "02", name: "q-lab", state: "asleep", meta: "CRIU · $0.36/mo" },
  { code: "03", name: "skyfall", state: "waking", meta: "restoring 3.5s" },
  { code: "04", name: "spectre", state: "idle", meta: "sleeps in 3m" },
] as const;

export function Hero({ nav, children }: { nav: ReactNode; children: ReactNode }) {
  const box = useRef<HTMLElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const el = box.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const ctx = cv.getContext("2d");
    if (!ctx) return;

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
      // Obsidian noir backdrop
      ctx.fillStyle = "#07080b";
      ctx.fillRect(0, 0, width, height);

      const family = '"IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace';
      ctx.font = `500 10px ${family}`;
      const charAspect = ctx.measureText("M").width / 10;
      const cellH = Math.max(height / BOND_H, width / ((BOND_W - 2 * EDGE_BLEED_COLS) * charAspect));
      ctx.font = `500 ${cellH}px ${family}`;
      const cellW = ctx.measureText("M").width;
      const totalW = BOND_W * cellW;
      const totalH = BOND_H * cellH;
      // Trim the frame's blank margins and bias the subject to the right on wide screens.
      const bleed = EDGE_BLEED_COLS * cellW;
      const offsetX = -bleed - Math.max(0, totalW - 2 * bleed - width) * (width >= 900 ? 0.35 : 0.5);
      const offsetY = (height - totalH) / 2;

      ctx.textBaseline = "top";

      const frame = FRAMES[frameIdx % FRAMES.length];
      const firing = frameIdx >= FIRE_START && frameIdx <= FIRE_END;

      // Render row by row with subtle intensity gradient
      for (let y = 0; y < BOND_H; y++) {
        const py = offsetY + y * cellH;
        if (py + cellH < 0 || py > height) continue;
        const row = frame.slice(y * BOND_W, (y + 1) * BOND_W);
        // Subtle 007 crimson tint during the turn-and-fire frames
        ctx.fillStyle = firing ? "rgba(248, 196, 196, 0.72)" : "rgba(228, 232, 240, 0.62)";
        ctx.fillText(row, offsetX, py);
      }

      // Subtle crimson muzzle aura during turn/fire
      if (firing) {
        const cx = offsetX + totalW * 0.62;
        const cy = offsetY + totalH * 0.52;
        const pulse = ctx.createRadialGradient(cx, cy, 4, cx, cy, Math.max(width, height) * 0.42);
        pulse.addColorStop(0, "rgba(220, 38, 38, 0.22)");
        pulse.addColorStop(0.5, "rgba(220, 38, 38, 0.06)");
        pulse.addColorStop(1, "rgba(7, 8, 11, 0)");
        ctx.fillStyle = pulse;
        ctx.fillRect(0, 0, width, height);
      }

      // Left-to-right noir gradient veil so hero headline & CTA stay razor-legible
      const leftVeil = ctx.createLinearGradient(0, 0, width * 0.78, 0);
      leftVeil.addColorStop(0, "rgba(7, 8, 11, 0.92)");
      leftVeil.addColorStop(0.45, "rgba(7, 8, 11, 0.72)");
      leftVeil.addColorStop(1, "rgba(7, 8, 11, 0.16)");
      ctx.fillStyle = leftVeil;
      ctx.fillRect(0, 0, width, height);

      // Top & bottom edge vignettes
      const vertVeil = ctx.createLinearGradient(0, 0, 0, height);
      vertVeil.addColorStop(0, "rgba(7, 8, 11, 0.65)");
      vertVeil.addColorStop(0.2, "rgba(7, 8, 11, 0)");
      vertVeil.addColorStop(0.8, "rgba(7, 8, 11, 0)");
      vertVeil.addColorStop(1, "rgba(7, 8, 11, 0.94)");
      ctx.fillStyle = vertVeil;
      ctx.fillRect(0, 0, width, height);
    };

    resize();
    const ro = new ResizeObserver(() => {
      resize();
      if (reduced.matches) drawFrame(38);
    });
    ro.observe(el);

    let raf = 0;
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
      const idx = Math.floor(((now - startMs) / 1000) * BOND_FPS) % FRAMES.length;
      if (idx !== lastIdx) {
        lastIdx = idx;
        drawFrame(idx);
      }
      raf = requestAnimationFrame(tick);
    };

    if (reduced.matches) {
      drawFrame(38);
    } else {
      raf = requestAnimationFrame(tick);
    }

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, []);

  return (
    <header ref={box} className="hero hero-bond">
      <div className="bond-stage" aria-hidden>
        <canvas ref={canvas} />
        <div className="bond-scanlines" />
      </div>
      <nav className="nav">{nav}</nav>
      <div className="hero-copy">{children}</div>
      <div className="bond-dossier" aria-hidden data-ai-ignore>
        {DOSSIER.map((d) => (
          <span key={d.code} className={`dossier-chip is-${d.state}`}>
            <i className="dossier-dot" />
            <span className="dossier-code">{d.code}</span>
            <span className="dossier-name">{d.name}</span>
            <span className="dossier-meta">{d.meta}</span>
          </span>
        ))}
      </div>
    </header>
  );
}
