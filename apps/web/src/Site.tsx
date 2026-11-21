import { Together } from "./Together";
import { Hero } from "./Hero";
import { Relay } from "./Relay";
import { InstallButton } from "./InstallButton";
import { Agent37Mark, AppleMark, GitHubMark, LinuxMark, T3CodeMark, WindowsMark } from "./logos";
import { AGENT37, CREATOR, REPO, T3CODE, UPSTREAM } from "./links";

function OsMarks({ className }: { className: string }) {
  return (
    <span className={className} role="img" aria-label="macOS, Windows and Linux">
      <AppleMark aria-hidden />
      <WindowsMark aria-hidden />
      <LinuxMark aria-hidden />
    </span>
  );
}

function Wordmark({ withBadge = false }: { withBadge?: boolean }) {
  return (
    <a className="wordmark" href="./">
      {withBadge && (
        <span className="wordmark-badge" aria-hidden>
          37
        </span>
      )}
      t3agent37
    </a>
  );
}

export function Site() {
  return (
    <>
      <Hero
        nav={
          <>
            <Wordmark withBadge />
            <div className="nav-links">
              <a href="#how-it-works">How it works</a>
              <a className="nav-btn" href={REPO}>
                <GitHubMark aria-hidden />
                GitHub
              </a>
            </div>
          </>
        }
      >
        <p className="hero-kicker">AGENT37 · EDGE-AUTHENTICATED CLOUD SANDBOXES</p>
        <h1 data-ai-content="# Run your T3 Code agents in the cloud, on Agent37">
          Run your{" "}
          <span className="h1-t3" role="img" aria-label="T3 Code">
            <T3CodeMark aria-hidden />
            Code
          </span>{" "}
          agents in the cloud, on{" "}
          <span className="h1-t3 h1-a37" role="img" aria-label="Agent37">
            <Agent37Mark aria-hidden />
            Agent37
          </span>
        </h1>
        <p>
          t3agent37 spins up pay-as-you-go Agent37 containers and VMs, pairs them into T3 Code behind edge auth, and
          CRIU-checkpoints them to sleep when idle.
        </p>
        <div className="hero-cta">
          <InstallButton />
          <OsMarks className="hero-os" />
        </div>
      </Hero>

      <Together />

      <Relay />

      <section className="needs">
        <p className="eyebrow">Before you install</p>
        <h2>What you need</h2>
        <ul className="needs-list">
          <li data-ai-content={`[T3 Code](${T3CODE}): the desktop app or server`}>
            <a href={T3CODE} className="needs-logo" aria-label="T3 Code">
              <T3CodeMark className="needs-t3" aria-hidden />
              Code
            </a>
            <span>Desktop app or Linux server</span>
          </li>
          <li data-ai-content={`An [Agent37](${AGENT37}) API key. Pay-as-you-go wallet, no subscription ($4.76/mo entry container, $0.09/GB/mo asleep disk).`}>
            <a href={AGENT37} className="needs-logo" aria-label="Agent37">
              <Agent37Mark className="needs-t3" aria-hidden />
              Agent37
            </a>
            <span>Pay-as-you-go wallet · no subscription</span>
          </li>
          <li>
            <OsMarks className="needs-logo needs-os" />
            <span>macOS, Windows, or headless Linux VPS (+ live Web UI & MCP)</span>
          </li>
        </ul>
        <InstallButton />
      </section>

      <footer className="footer">
        <div className="footer-main" data-ai-ignore>
          <Wordmark withBadge />
          <nav className="footer-links" aria-label="Elsewhere">
            <a href={T3CODE} aria-label="T3 Code">
              <T3CodeMark className="footer-t3" aria-hidden />
              Code
            </a>
            <a href={AGENT37} aria-label="Agent37">
              <Agent37Mark className="footer-t3" aria-hidden />
              Agent37
            </a>
            <a href={REPO}>
              <GitHubMark aria-hidden />
              GitHub
            </a>
          </nav>
        </div>
        <div className="footer-fine">
          <p>
            Built by <a href={CREATOR}>Rishabh Bothra</a> · Forked from <a href={UPSTREAM}>t3boat</a> by Pau Kraft
          </p>
          <p>Not affiliated with or endorsed by T3 Code or Agent37.</p>
        </div>
      </footer>
    </>
  );
}
