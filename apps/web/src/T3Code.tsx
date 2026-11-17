// T3 Code in a frame: public/t3code is a mock of its window built from T3 Code's own source
// (twittergame's apps/embed; `bun run embed` there rebuilds it). This draws one thread in it,
// living on a t3boat machine, as far along as the demo's step says.
import { useEffect, useState } from "react";

/** A row of the thread, as the mock draws them. */
type Message =
  | { kind: "user" | "assistant"; id: string; text: string }
  /** something the agent did */
  | { kind: "work"; id: string; label: string; icon: "eye" | "square-pen" | "terminal" }
  /** the running turn's header: "Working for 12s" */
  | { kind: "working"; id: string; elapsed: string };

/** The mock's handle on its frame's window. */
interface T3CodeFrame {
  show(scene: { title: string; messages: Message[]; draft: string; running: boolean; machine?: string }): void;
  /** the send button was pressed */
  onSend: (() => void) | null;
}

declare global {
  interface Window {
    t3code?: T3CodeFrame;
  }
}

interface Turn {
  prompt: string;
  steps: Pick<Message & { kind: "work" }, "label" | "icon">[];
  reply: string;
}

const TITLE = "Fix the flaky checkout test";

/** In the thread from the start: chats stay readable while their machine sleeps. */
const EARLIER: Turn = {
  prompt: "Why is the checkout test flaky on CI?",
  steps: [
    { label: "Read 3 files", icon: "eye" },
    { label: "Ran tests", icon: "terminal" },
  ],
  reply: "It races the cart's debounce: the assertion sometimes runs before the cart flushes.",
};

/** What the composer sends, in order, over and over. */
const SCRIPT: Turn[] = [
  {
    prompt: "Fix it, then run the whole suite.",
    steps: [
      { label: "Edited cart.ts", icon: "square-pen" },
      { label: "Edited checkout.test.ts", icon: "square-pen" },
      { label: "Ran tests", icon: "terminal" },
    ],
    reply: "The test now awaits the flush. All 212 tests pass.",
  },
  {
    prompt: "Open a PR for it.",
    steps: [
      { label: "Read 2 files", icon: "eye" },
      { label: "Ran 2 commands", icon: "terminal" },
    ],
    reply: "Opened #482, “Await the cart flush in checkout”.",
  },
  {
    prompt: "Add a regression test for the race.",
    steps: [
      { label: "Edited checkout.test.ts", icon: "square-pen" },
      { label: "Ran tests", icon: "terminal" },
    ],
    reply: "Added and pushed. 213 tests pass.",
  },
];

/** How long the agent works on a message: its steps, its reply, and a moment to read that. */
export const WORK_MS = 7500;

/** `turn`'s rows: its prompt, the first `steps` things the agent did, then what is in `tail`. */
const rows = (turn: Turn, key: number, steps: number, tail: "working" | "reply" | null, secs = 0): Message[] => [
  { kind: "user", id: `${key}`, text: turn.prompt },
  ...(tail === "working" ? [{ kind: "working" as const, id: `${key}:working`, elapsed: `${secs}s` }] : []),
  ...turn.steps.slice(0, steps).map((step, i) => ({ kind: "work" as const, id: `${key}:${i}`, ...step })),
  ...(tail === "reply" ? [{ kind: "assistant" as const, id: `${key}:reply`, text: turn.reply }] : []),
];

/**
 * The thread at `stage` of its `turn`th message: waiting in the composer while the machine is
 * "asleep", sent as it is "waking", the agent "working" on it for `WORK_MS`, answered by "idle".
 * `machine` is the name of the one it lives on. `onSend`: the send button, live while asleep.
 */
export function T3Code({
  machine,
  turn,
  stage,
  onSend,
}: {
  machine?: string;
  turn: number;
  stage: "asleep" | "waking" | "working" | "idle";
  onSend: () => void;
}) {
  const [frame, setFrame] = useState<T3CodeFrame>();
  /** how far the working agent is: its steps one by one, then its reply */
  const [done, setDone] = useState(0);
  const [secs, setSecs] = useState(0);

  const now = SCRIPT[turn % SCRIPT.length];
  const before = turn ? SCRIPT[(turn - 1) % SCRIPT.length] : EARLIER;
  const answered = stage === "idle" || done > now.steps.length;
  const running = stage !== "asleep" && !answered;

  // the working agent: its clock, and one thing after another, evenly over its time
  useEffect(() => {
    if (stage !== "working") return;
    const clock = setInterval(() => setSecs((n) => n + 1), 1000);
    const work = setInterval(() => setDone((n) => n + 1), WORK_MS / (now.steps.length + 2));
    return () => {
      clearInterval(clock);
      clearInterval(work);
      setSecs(0);
      setDone(0);
    };
  }, [stage, now]);

  useEffect(() => {
    if (!frame) return;
    frame.onSend = onSend;
    frame.show({
      title: TITLE,
      messages: [
        ...rows(before, turn - 1, before.steps.length, "reply"),
        ...(stage === "asleep"
          ? []
          : answered
            ? rows(now, turn, now.steps.length, "reply")
            : rows(now, turn, done, "working", secs)),
      ],
      draft: stage === "asleep" ? now.prompt : "",
      running,
      machine,
    });
  });

  return (
    <div className="t3">
      <iframe
        src="/t3code/index.html"
        title="T3 Code demo"
        loading="lazy"
        onLoad={(e) => setFrame(e.currentTarget.contentWindow?.t3code)}
      />
    </div>
  );
}
