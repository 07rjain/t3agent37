// Outside Tauri the desktop UI runs on its mock backend: sample machines, nothing real.
// The site around this frame plays T3 Code on that backend (see Together.tsx).
import "@t3boat/desktop/ui";
import "./demo.css";
import type { MockBackend } from "@t3boat/desktop/mock";

declare global {
  interface Window {
    mock?: Promise<MockBackend>;
  }
}

window.mock = import("@t3boat/desktop/mock").then((m) => m.mockBackend());
