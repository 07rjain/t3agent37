// The hero: the app's pixel sea, full bleed, drawn by the app's own engine. Nav and copy sit
// on plain sky; the sky's gradient, the horizon and the boats come below them.
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useAnimation, useTheme } from "@t3boat/desktop/pixel/clock";
import { drawBoat, poseFor } from "@t3boat/desktop/pixel/boat";
import { palettes } from "@t3boat/desktop/pixel/palette";
import { Px } from "@t3boat/desktop/pixel/px";
import { drawScene, HORIZON, SEA, type Rect } from "@t3boat/desktop/pixel/scene";
import { seedOf, stateTag } from "@t3boat/desktop/machine";
import type { MachineState } from "@t3boat/desktop/types";

/** Left to right; `depth` is the waterline between horizon (0) and the hero's bottom (1), `room` how far it sails. */
// the first two sail on narrow screens
const FLEET: { key: string; state: MachineState; depth: number; room: number }[] = [
  { key: "t3-main", state: "working", depth: 0.5, room: 14 },
  { key: "side-quest", state: "working", depth: 0.72, room: 10 },
  { key: "big-refactor", state: "asleep", depth: 0.36, room: 0 },
  { key: "docs", state: "waking", depth: 0.62, room: 0 },
  { key: "scratch", state: "asleep", depth: 0.44, room: 0 },
];
/** Scene px each boat needs; a narrower sea carries fewer. */
const SLOT = 68;
/** Scene px the scene's layout (drawn for the app's 190 px strip) spreads over. */
const LAYOUT_W = 175;
/** Share of the sky's gradient, from its top, that is still plain sky in the light theme. */
const PLAIN = 0.45;
/** The sea zooms out this much more than the sky: a short band of sky, deep water. */
const SEA_PER_SKY = 2;

/** CSS px per scene px: twice the app's. */
const PIXEL = 4;

/** What `el` shows: its box if it paints one, else its lines' text and inline boxes, not the wider block around them. */
function inkRects(el: Element): DOMRect[] {
  const style = getComputedStyle(el);
  const paints = style.backgroundColor !== "rgba(0, 0, 0, 0)" || parseFloat(style.borderTopWidth) > 0;
  if (paints) return [el.getBoundingClientRect()];
  const range = document.createRange();
  range.selectNodeContents(el);
  return [...range.getClientRects()].filter((r) => r.width > 0 && r.height > 0);
}

/** `nav` and the copy (`children`) sit on plain sky; the scene fills the hero below them. */
export function Hero({ nav, children }: { nav: ReactNode; children: ReactNode }) {
  const box = useRef<HTMLElement>(null);
  const navBox = useRef<HTMLElement>(null);
  const copy = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const buffer = useRef<Px | null>(null);
  const tags = useRef(new Map<string, HTMLSpanElement>());
  /** hero size and where the copy ends, in CSS px; the text blocks, in scene px */
  const [layout, setLayout] = useState({ w: 0, h: 0, copy: 0, text: [] as Rect[] });
  const theme = useTheme();

  useLayoutEffect(() => {
    const el = box.current!;
    const measure = () => {
      const origin = el.getBoundingClientRect();
      const text = [...navBox.current!.children, ...copy.current!.children].flatMap(inkRects).map((r) => ({
        x0: Math.floor((r.left - origin.left) / PIXEL),
        y0: Math.floor((r.top - origin.top) / PIXEL),
        x1: Math.ceil((r.right - origin.left) / PIXEL),
        y1: Math.ceil((r.bottom - origin.top) / PIXEL),
      }));
      setLayout({ w: el.clientWidth, h: el.clientHeight, copy: copy.current!.offsetTop + copy.current!.offsetHeight, text });
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    ro.observe(copy.current!);
    // line boxes move once the web fonts land
    document.fonts.ready.then(measure);
    return () => ro.disconnect();
  }, []);

  const s = PIXEL;
  const w = Math.ceil(layout.w / s);
  const h = Math.ceil(layout.h / s);
  // the scene fills what the copy leaves, its plain share of sky behind the copy's last rows
  const zoom = (h - layout.copy / s) / ((1 - PLAIN) * HORIZON + SEA_PER_SKY * SEA);
  const band = Math.round(HORIZON * zoom);
  const horizon = h - Math.round(SEA_PER_SKY * SEA * zoom);
  const fleet = FLEET.slice(0, Math.max(2, Math.min(FLEET.length, Math.floor(w / SLOT))));

  useAnimation(
    (t) => {
      const ctx = canvas.current?.getContext("2d");
      if (!ctx || !w || !h) return;
      if (buffer.current?.w !== w || buffer.current.h !== h) buffer.current = new Px(w, h);
      const px = buffer.current;
      const pal = palettes[theme];
      const time = t ?? 0;
      const at = { x: w / LAYOUT_W, sky: zoom, sea: SEA_PER_SKY * zoom, top: horizon - band };
      drawScene(px, at, pal, theme, time, layout.text);
      // back row first so nearer boats overlap farther ones
      const boats = fleet.map((b, i) => ({
        ...b,
        cx: Math.round((w * (i + 0.5)) / fleet.length),
        wy: Math.round(horizon + b.depth * (h - horizon)),
      }));
      boats.sort((a, z) => a.wy - z.wy);
      for (const b of boats) {
        const top = drawBoat(px, b.cx, b.wy, pal, poseFor(b.state, t, seedOf(b.key), b.room), time);
        const tag = tags.current.get(b.key);
        if (tag) tag.style.transform = `translate(${top.x * s}px, ${(top.y - 1) * s}px) translate(-50%, -100%)`;
      }
      px.blit(ctx);
    },
    [theme, layout, fleet.length],
  );

  return (
    <header ref={box} className="hero">
      <div className="sea" aria-hidden>
        <canvas ref={canvas} width={w} height={h} style={{ width: w * s, height: h * s }} />
        {fleet.map((b) => (
          <span
            key={b.key}
            ref={(el) => {
              if (el) tags.current.set(b.key, el);
              else tags.current.delete(b.key);
            }}
            className={`sea-tag is-${b.state}`}
          >
            {stateTag(b.state)}
          </span>
        ))}
      </div>
      <nav ref={navBox} className="nav">
        {nav}
      </nav>
      <div ref={copy} className="hero-copy">
        {children}
      </div>
    </header>
  );
}
