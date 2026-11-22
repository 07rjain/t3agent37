#!/usr/bin/env node
// Extracts the markdown section for a given SemVer version from CHANGELOG.md,
// and optionally verifies version alignment across package/Cargo/Tauri manifests.
//
// Usage:
//   node scripts/release-notes.mjs <version>          # prints release notes for X.Y.Z
//   node scripts/release-notes.mjs --verify           # verifies all manifests match & prints version

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");

function readVersions() {
  const tauriConf = JSON.parse(readFileSync(resolve(root, "apps/desktop/src-tauri/tauri.conf.json"), "utf8"));
  const desktopPkg = JSON.parse(readFileSync(resolve(root, "apps/desktop/package.json"), "utf8"));
  const rootPkg = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
  const cargoToml = readFileSync(resolve(root, "apps/desktop/src-tauri/Cargo.toml"), "utf8");
  const cargoMatch = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
  if (!cargoMatch) {
    throw new Error("Could not find version in apps/desktop/src-tauri/Cargo.toml");
  }
  return {
    tauri: tauriConf.version,
    cargo: cargoMatch[1],
    desktopPkg: desktopPkg.version,
    rootPkg: rootPkg.version,
  };
}

function extractNotes(version) {
  const clean = version.replace(/^v/, "");
  const lines = readFileSync(resolve(root, "CHANGELOG.md"), "utf8").split(/\r?\n/);
  const headerPrefix = `## [${clean}]`;
  const start = lines.findIndex((line) => line.startsWith(headerPrefix));
  if (start === -1) return null;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    if (lines[i].startsWith("## [")) {
      end = i;
      break;
    }
  }
  const body = lines.slice(start + 1, end).join("\n").trim();
  return body || null;
}

const arg = process.argv[2];
if (arg === "--verify") {
  const v = readVersions();
  const unique = new Set(Object.values(v));
  if (unique.size !== 1) {
    console.error("Version mismatch across manifests:", v);
    process.exit(1);
  }
  const version = v.tauri;
  if (!extractNotes(version)) {
    console.error(`CHANGELOG.md has no section for [${version}]`);
    process.exit(1);
  }
  process.stdout.write(`${version}\n`);
  process.exit(0);
}

const version = (arg || readVersions().tauri).replace(/^v/, "");
const notes = extractNotes(version);
if (notes) {
  process.stdout.write(`${notes}\n`);
} else {
  process.stdout.write(`Release v${version}\n`);
}
