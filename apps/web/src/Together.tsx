// T3 Code and t3boat side by side, each in a frame: a mock of T3 Code's window, and the real
// popover on its mock backend with one machine. The bar above walks a message's round on that
// machine: the visitor acts where a person would (send, come back later), what follows from it
// plays by itself, and the bar says what happens while an arrow points at where it shows.
import { useEffect, useRef, useState, type CSSProperties } from "react";
import type { MockBackend } from "@t3boat/desktop/mock";
import { useAnimation } from "@t3boat/desktop/pixel/clock";
import type { UiState } from "@t3boat/desktop/types";
import { T3Code, WORK_MS } from "./T3Code";

/**
 * The round, a stage a step, from asleep to asleep again; the next round starts from that last
 * step, at waking. For each: what to read, and where it shows: the window (`look`) and the
 * spot in it, an element of its page and a point of that element's box (`at`, `x`, `y`). A
 * stage ends either by itself after `ms`, or when the visitor does what `act` says: with the
 * bar's button, or at the spot, which is the control for it or else gets a button of its own
 * (`byArrow`). `then` is how the machine takes it.
 */
const STEPS = [
  {
    stage: "asleep",
    name: "Asleep",
    text: "The machine is asleep and costs nothing. Send a message.",
    look: "t3",
    at: 'button[type="submit"]',
    x: 0.9,
    y: 0.9,
    act: "Send it",
    then: "wake",
  },
  {
    stage: "waking",
    name: "Waking",
    text: "Your message wakes the machine. That takes seconds.",
    look: "app",
    at: ".row-detail",
    x: 0.3,
    y: 1,
    ms: 2500,
    then: "work",
  },
  {
    stage: "working",
    name: "Working",
    text: "The agent works on the machine, laptop lid open or closed.",
    look: "t3",
    // the newest row of the thread: the last child is a spacer
    at: ".messages-timeline-row-frame:nth-last-child(2)",
    x: 0.42,
    y: 0.45,
    ms: WORK_MS,
    then: "done",
  },
  {
    stage: "idle",
    name: "Idle",
    text: "Done. If the machine is idle for a bit, it goes back to sleep.",
    look: "app",
    at: ".row-detail",
    x: 0.3,
    y: 1,
    act: "A bit later",
    byArrow: true,
    then: "sleep",
  },
  {
    stage: "asleep",
    name: "Asleep",
    text: "The machine is asleep again. It costs nothing until you send the next message.",
    look: "app",
    at: ".row-detail",
    x: 0.3,
    y: 1,
    act: "Next message",
    byArrow: true,
    then: "wake",
  },
] as const;

const WINDOWS = { t3: "T3 Code", app: "t3boat" };

/** How long "a bit later" takes: the idle machine's wait, fast-forwarded. */
const SKIP_MS = 3000;

export function Together() {
  const [mock, setMock] = useState<MockBackend>();
  const [state, setState] = useState<UiState>();
  /** the machine T3 Code's thread lives on: the first found asleep */
  const [home, setHome] = useState<string>();
  /** messages sent and answered so far */
  const [turn, setTurn] = useState(0);
  const [step, setStep] = useState(0);
  /** fast-forwarding through the idle machine's wait */
  const [skipping, setSkipping] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const pointer = useRef<HTMLDivElement>(null);
  const arrow = useRef<SVGSVGElement>(null);

  useEffect(() => {
    if (!mock) return;
    mock.call("get_state", undefined).then((s) => {
      setState(s);
      setHome(s.machines.find((m) => m.paired && m.state === "asleep")?.id);
    });
    return mock.subscribe(setState);
  }, [mock]);

  const machine = state?.machines.find((m) => m.id === home);
  const now = STEPS[step];
  /** nothing to step to: the machine is gone, or on its way to sleep */
  const held = !mock || !machine || skipping || machine.state === "sleeping";
  /** how long the stage plays by itself, when it does */
  const playing = "ms" in now ? now.ms : skipping ? SKIP_MS : null;

  const advance = () => {
    if (held) return;
    if (now.then === "sleep") {
      setSkipping(true);
      return void mock.later(machine.id, SKIP_MS);
    }
    mock.turn(machine.id, now.then);
    setStep(step + 1 < STEPS.length ? step + 1 : 1);
  };

  // a stage that passes by itself
  useEffect(() => {
    if (!("ms" in now) || held) return;
    const over = setTimeout(advance, now.ms);
    return () => clearTimeout(over);
  }, [step, held]);

  // the idle machine goes to sleep, however it got there: the last stage, the next message up
  useEffect(() => {
    if (now.stage !== "idle" || (machine?.state !== "sleeping" && machine?.state !== "asleep")) return;
    setSkipping(false);
    setTurn((t) => t + 1);
    setStep(step + 1);
  }, [now, machine?.state]);

  // the arrow follows its spot, which moves with the window's layout and what the window shows
  useAnimation(
    (t) => {
      const frame = box.current!.querySelector<HTMLIFrameElement>(`.pane-${now.look} iframe`)!;
      const spot = frame.contentDocument?.querySelector(now.at);
      pointer.current!.style.visibility = spot ? "" : "hidden";
      if (!spot) return;
      const origin = box.current!.getBoundingClientRect();
      const win = frame.getBoundingClientRect();
      const rect = spot.getBoundingClientRect();
      // T3 Code's frame is zoomed
      const zoom = win.width / frame.offsetWidth;
      const x = win.left - origin.left + frame.clientLeft + (rect.left + rect.width * now.x) * zoom;
      const y = win.top - origin.top + frame.clientTop + (rect.top + rect.height * now.y) * zoom;
      pointer.current!.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
      // steps back and forth along its own line
      const nudge = t === null ? 0 : Math.round(2 + 2 * Math.sin(t * 6));
      arrow.current!.style.transform = `translate(${nudge}px, ${nudge}px)`;
    },
    [step],
  );

  return (
    <main id="how-it-works" className="together">
      <p className="eyebrow">T3 Code + t3boat</p>
      <h2>Chat in T3 Code as usual. The agent runs in the cloud.</h2>

      <div ref={box} className="stage" data-look={now.look}>
        <div className="now">
          <ol>
            {STEPS.map((s, i) => (
              <li
                key={i}
                aria-current={i === step ? "step" : undefined}
                data-past={i < step || undefined}
                style={i === step && playing ? ({ "--ms": `${playing}ms` } as CSSProperties) : undefined}
              >
                {s.name}
              </li>
            ))}
          </ol>
          <p key={step} data-ai-ignore>
            <span className="now-window">{WINDOWS[now.look]}</span>
            {now.text}
          </p>
          {"act" in now && !skipping ? (
            <button type="button" disabled={held} onClick={advance}>
              {now.act}
            </button>
          ) : null}
        </div>

        <div className="pane-t3">
          <T3Code machine={machine?.name} turn={turn} stage={now.stage} onSend={advance} />
        </div>

        <div ref={pointer} className="pointer">
          {/* pixels, its tip at the top left */}
          <svg ref={arrow} viewBox="-0.5 -0.5 10 10" aria-hidden>
            <path d="M0 0h6v1H0zM0 1h5v1H0zM0 2h4v1H0zM0 3h5v1H0zM0 4h2v1H0zM3 4h3v1H3zM0 5h1v1H0zM4 5h3v1H4zM5 6h3v1H5zM6 7h3v1H6zM7 8h2v1H7z" />
          </svg>
          {!("byArrow" in now) ? null : skipping ? (
            <span className="skip" role="img" aria-label="Fast forward" style={{ "--ms": `${SKIP_MS}ms` } as CSSProperties}>
              {/* a clock, its hands flying */}
              <svg className="skip-clock" viewBox="0 0 11 11">
                <path d="M3 0h5v1H3zM1 1h2v1H1zM8 1h2v1H8zM1 2h1v1H1zM9 2h1v1H9zM0 3h1v5H0zM10 3h1v5h-1zM1 8h1v1H1zM9 8h1v1H9zM1 9h2v1H1zM8 9h2v1H8zM3 10h5v1H3z" />
                <rect className="skip-minutes" x="5" y="2" width="1" height="4" />
                <rect className="skip-hours" x="5" y="5" width="3" height="1" />
              </svg>
              <svg className="skip-sign" viewBox="0 0 9 7">
                <path d="M0 0h1v1h1v1h1v1h1v1H3v1H2v1H1v1H0zM5 0h1v1h1v1h1v1h1v1H8v1H7v1H6v1H5z" />
              </svg>
            </span>
          ) : (
            <button type="button" disabled={held} onClick={advance}>
              {now.act}
            </button>
          )}
        </div>

        <div className="pane-app">
          <iframe
            src="/demo.html?machines=1"
            title="t3boat demo"
            width={380}
            height={560}
            loading="lazy"
            onLoad={(e) => e.currentTarget.contentWindow?.mock?.then(setMock)}
          />
        </div>
      </div>
    </main>
  );
}
