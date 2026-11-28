// Renders the t3agent37 social card (public/og.png), animated Bond ASCII hero GIF (public/og.gif),
// and MP4 loop (public/og.mp4) directly from the landing page hero.
// Needs Google Chrome and ffmpeg. Starts a local Vite dev server automatically if SITE is unset.
// Run: bun --filter @t3boat/web og
import { chromium, type Page } from "playwright-core";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const W = 1200;
const H = 630;
/** Matches BOND_FPS and BOND_RLE.length in src/bondAscii.ts (84 frames @ 14fps = 6.0s seamless loop). */
const FPS = 14;
const FRAMES = 84;
const WEB_DIR = join(import.meta.dir, "..");
const OUT = join(WEB_DIR, "public");
const PORT = 4179;

/** Hero framed for 1200x630 social cards (X summary_large_image & WhatsApp) with safe bottom clearance. */
const CARD_CSS = `
  html, body {
    overflow: hidden !important;
    background: #07080b !important;
  }
  .hero-bond {
    display: flex !important;
    flex-direction: column !important;
    justify-content: space-between !important;
    height: ${H}px !important;
    min-height: 0 !important;
    padding: 0 !important;
    border-top: 3px solid #dc2626 !important;
    border-bottom: 0 !important;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08) !important;
  }
  .nav, .hero-copy, .bond-dossier {
    width: calc(100% - 112px) !important;
    max-width: none !important;
  }
  .nav {
    height: auto !important;
    padding-top: 38px !important;
  }
  .wordmark {
    font-size: 38px !important;
    gap: 14px !important;
  }
  .wordmark-badge {
    width: 38px !important;
    height: 38px !important;
    font-size: 18px !important;
    border-width: 2px !important;
  }
  .nav-links,
  .hero-cta,
  .hero-privacy {
    display: none !important;
  }
  .hero-copy {
    padding-block: 0 !important;
    margin-top: 8px !important;
  }
  .hero-kicker {
    max-width: none !important;
    white-space: nowrap !important;
    font-size: 14px !important;
    letter-spacing: 0.16em !important;
    margin-bottom: 16px !important;
  }
  .hero h1 {
    font-size: 64px !important;
    line-height: 1.04 !important;
    max-width: 14em !important;
    text-shadow: 0 2px 24px rgba(7, 8, 11, 0.96), 0 1px 4px rgba(7, 8, 11, 0.92) !important;
  }
  .hero-copy > p:not(.hero-kicker) {
    margin-top: 20px !important;
    font-size: 20px !important;
    line-height: 1.45 !important;
    max-width: 33em !important;
    color: rgba(244, 244, 245, 0.88) !important;
    text-shadow: 0 1px 16px rgba(7, 8, 11, 0.95) !important;
  }
  .bond-dossier {
    margin: 0 auto 48px !important;
    gap: 10px !important;
  }
  .dossier-chip {
    padding: 8px 13px !important;
    font-size: 13px !important;
    background: rgba(11, 13, 19, 0.9) !important;
    border-color: rgba(255, 255, 255, 0.16) !important;
  }
`;

async function waitForServer(url: string, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      // Server not ready yet.
    }
    await Bun.sleep(150);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

function runFfmpeg(args: string[]) {
  const res = Bun.spawnSync(["ffmpeg", "-y", "-loglevel", "error", ...args], {
    stdio: ["ignore", "inherit", "inherit"],
  });
  if (!res.success) {
    throw new Error(`ffmpeg failed: ${args.join(" ")}`);
  }
}

async function open(page: Page, url: string) {
  await page.goto(url, { waitUntil: "networkidle" });
  await page.addStyleTag({ content: CARD_CSS });
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
}

let server: ReturnType<typeof Bun.spawn> | null = null;
const url = process.env.SITE ?? `http://127.0.0.1:${PORT}/`;

if (!process.env.SITE) {
  server = Bun.spawn([process.execPath, "run", "dev", "--", "--host", "127.0.0.1", "--port", String(PORT), "--strictPort"], {
    cwd: WEB_DIR,
    stdout: "ignore",
    stderr: "pipe",
  });
  await waitForServer(url);
}

const workDir = await mkdtemp(join(tmpdir(), "t3agent37-og-"));

try {
  const browser = await chromium.launch({ channel: "chrome" });
  try {
    // 1) Crisp static 1200x630 social card (rendered at 2x, downscaled + palette-optimized under 300 KB for WhatsApp & X).
    // reducedMotion: "reduce" locks Hero.tsx onto frame 38 (Bond turn & fire with crimson aura).
    const still = await browser.newPage({
      viewport: { width: W, height: H },
      deviceScaleFactor: 2,
      colorScheme: "dark",
      reducedMotion: "reduce",
    });
    await open(still, url);
    const rawStill = join(workDir, "still-2x.png");
    await still.screenshot({ path: rawStill });
    await still.close();

    const stillPalette = join(workDir, "still-palette.png");
    runFfmpeg([
      "-i", rawStill,
      "-vf", `scale=${W}:${H}:flags=lanczos,palettegen=max_colors=192:stats_mode=full`,
      stillPalette,
    ]);
    runFfmpeg([
      "-i", rawStill,
      "-i", stillPalette,
      "-lavfi", `scale=${W}:${H}:flags=lanczos [x]; [x][1:v] paletteuse=dither=none`,
      "-compression_level", "9",
      join(OUT, "og.png"),
    ]);

    // 2) Seamless 6-second (84 frames @ 14fps) Bond ASCII gun-barrel animation.
    const anim = await browser.newPage({
      viewport: { width: W, height: H },
      deviceScaleFactor: 1,
      colorScheme: "dark",
      reducedMotion: "no-preference",
    });
    await anim.clock.install();
    await open(anim, url);

    const framesDir = join(workDir, "frames");
    await Bun.$`mkdir -p ${framesDir}`.quiet();
    const stepMs = 1000 / FPS;
    for (let i = 0; i < FRAMES; i++) {
      await anim.screenshot({ path: join(framesDir, `${String(i).padStart(4, "0")}.png`) });
      await anim.clock.runFor(stepMs);
    }
    await anim.close();

    // High-clarity looping GIF (840x441, 14fps, stats_mode=full preserves dossier dot colors).
    const gifPalette = join(workDir, "gif-palette.png");
    const gifScale = "scale=840:441:flags=lanczos";
    runFfmpeg([
      "-framerate", String(FPS),
      "-i", join(framesDir, "%04d.png"),
      "-vf", `${gifScale},palettegen=max_colors=96:stats_mode=full`,
      gifPalette,
    ]);
    runFfmpeg([
      "-framerate", String(FPS),
      "-i", join(framesDir, "%04d.png"),
      "-i", gifPalette,
      "-lavfi", `${gifScale} [x]; [x][1:v] paletteuse=dither=none:diff_mode=rectangle`,
      "-loop", "0",
      join(OUT, "og.gif"),
    ]);

    // Crisp 1200x630 H.264 MP4 loop at native 14fps for social video sharing.
    runFfmpeg([
      "-framerate", String(FPS),
      "-i", join(framesDir, "%04d.png"),
      "-c:v", "libx264",
      "-preset", "slow",
      "-crf", "22",
      "-pix_fmt", "yuv420p",
      "-movflags", "+faststart",
      join(OUT, "og.mp4"),
    ]);
  } finally {
    await browser.close();
  }

  for (const name of ["og.png", "og.gif", "og.mp4"]) {
    const info = await stat(join(OUT, name));
    console.log(`${name}: ${(info.size / 1024).toFixed(1)} KB`);
  }
} finally {
  await rm(workDir, { recursive: true, force: true });
  server?.kill();
}
