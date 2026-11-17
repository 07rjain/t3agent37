import icon from "@t3boat/desktop/icon.png";
import { Together } from "./Together";
import { Hero } from "./Hero";
import { Relay } from "./Relay";
import { InstallButton } from "./InstallButton";
import { AppleMark, BoatLogo, GitHubMark, LinuxMark, T3CodeMark, WindowsMark } from "./logos";
import { BOAT, CREATOR, REPO, T3CODE } from "./links";

/** The systems t3boat runs on, as their marks. */
function OsMarks({ className }: { className: string }) {
  return (
    <span className={className} role="img" aria-label="macOS, Windows and Linux">
      <AppleMark aria-hidden />
      <WindowsMark aria-hidden />
      <LinuxMark aria-hidden />
    </span>
  );
}

function Wordmark({ withIcon = false }: { withIcon?: boolean }) {
  return (
    <a className="wordmark" href="/">
      {withIcon && <img src={icon} alt="" width={40} height={40} />}
      t3boat
    </a>
  );
}

export function Site() {
  return (
    <>
      <Hero
        nav={
          <>
            <Wordmark />
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
        {/* data-ai-content: what agents read in place of the marks (scripts/prerender.ts) */}
        <h1 data-ai-content="# Run your T3 Code agents in the cloud, on Boat">
          Run your{" "}
          <span className="h1-t3" role="img" aria-label="T3 Code">
            <T3CodeMark aria-hidden />
            Code
          </span>{" "}
          agents in the cloud, on <BoatLogo className="h1-boat" role="img" aria-label="Boat" />
        </h1>
        <p>t3boat creates the machines and adds them to T3 Code. They sleep when idle, so you only pay for the minutes your agents run.</p>
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
          <li data-ai-content={`[T3 Code](${T3CODE}): the desktop app`}>
            <a href={T3CODE} className="needs-logo" aria-label="T3 Code">
              <T3CodeMark className="needs-t3" aria-hidden />
              Code
            </a>
            <span>The desktop app</span>
          </li>
          <li data-ai-content={`A [Boat](${BOAT}) account. Your machines run there.`}>
            <a href={BOAT} className="needs-logo" aria-label="Boat">
              <BoatLogo className="needs-boat" aria-hidden />
            </a>
            <span>An account. Your machines run there.</span>
          </li>
          <li>
            <OsMarks className="needs-logo needs-os" />
            <span>macOS, Windows or Linux</span>
          </li>
        </ul>
        <InstallButton />
      </section>

      <footer className="footer">
        <div className="footer-main" data-ai-ignore>
          <Wordmark withIcon />
          <nav className="footer-links" aria-label="Elsewhere">
            <a href={T3CODE} aria-label="T3 Code">
              <T3CodeMark className="footer-t3" aria-hidden />
              Code
            </a>
            <a href={BOAT} aria-label="Boat">
              <BoatLogo className="footer-boat" aria-hidden />
            </a>
            <a href={REPO}>
              <GitHubMark aria-hidden />
              GitHub
            </a>
          </nav>
        </div>
        <div className="footer-fine">
          <p>
            Made by <a href={CREATOR}>Pau Kraft</a>
          </p>
          <p>Not affiliated with or endorsed by T3 Code or Boat.</p>
        </div>
      </footer>
    </>
  );
}
