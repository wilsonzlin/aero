import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function parseVersion(version) {
  const match = version.trim().match(/^v?(\d+)\.(\d+)\.(\d+)$/);
  if (!match) return null;
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    raw: version.trim().replace(/^v/, ""),
  };
}

function compareVersions(a, b) {
  if (a.major !== b.major) return a.major - b.major;
  if (a.minor !== b.minor) return a.minor - b.minor;
  return a.patch - b.patch;
}

function formatVersion(v) {
  return `${v.major}.${v.minor}.${v.patch}`;
}

function envFlag(name) {
  const raw = process.env[name];
  if (!raw) return false;
  const v = raw.trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes" || v === "on";
}

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const nvmrcPath = path.join(repoRoot, ".nvmrc");

let expectedRaw;
try {
  expectedRaw = await fs.readFile(nvmrcPath, "utf8");
} catch (err) {
  console.error(`error: Unable to read ${path.relative(process.cwd(), nvmrcPath)}`);
  console.error("This repo expects a root .nvmrc file to declare the canonical Node.js version.");
  process.exit(1);
}

const expected = parseVersion(expectedRaw);
if (!expected) {
  console.error("error: Unable to parse .nvmrc");
  console.error(
    `Expected an exact version like "22.11.0" (major.minor.patch), got: ${JSON.stringify(expectedRaw.trim())}`,
  );
  process.exit(1);
}

// For tests and hermetic tooling, allow overriding the detected Node.js version.
// (Node itself doesn't provide a supported way to spoof `process.versions.node`.)
const currentRaw = process.env.AERO_NODE_VERSION_OVERRIDE ?? process.versions.node;
const current = parseVersion(currentRaw);
if (!current) {
  console.error("error: Unable to parse the current Node.js version");
  console.error(`node reports: ${JSON.stringify(currentRaw)}`);
  process.exit(1);
}

const versionCmp = compareVersions(current, expected);
const allowUnsupported = envFlag("AERO_ALLOW_UNSUPPORTED_NODE");
// Some commands (notably `cargo xtask test-all` when running wasm-pack tests) require the same Node
// *major* as CI to avoid toolchain flakiness/hangs in unsupported Node releases.
const enforceMajor = envFlag("AERO_ENFORCE_NODE_MAJOR");
const tooOld = versionCmp < 0;

// CI pins an exact Node.js version in .nvmrc. We enforce this as a minimum, but allow newer
// Node versions so contributors (and CI-like sandbox environments) can still run the repo.
const recommendedUpperBoundMajor = expected.major + 1;
const recommendedRange = `>=${formatVersion(expected)} <${recommendedUpperBoundMajor}.0.0`;
const supportedRange = `>=${formatVersion(expected)}`;

if (tooOld) {
  const log = allowUnsupported ? console.warn : console.error;
  log(`${allowUnsupported ? "warning" : "error"}: Unsupported Node.js version for this repo.`);
  log(`- Detected: v${current.raw}`);
  log(`- Supported: ${supportedRange}`);
  log(`- Recommended: ${recommendedRange}`);

  if (allowUnsupported) {
    log(`- Override: AERO_ALLOW_UNSUPPORTED_NODE=1 (skipping Node version enforcement)`);
  } else {
    log("");
    log("Override (not recommended):");
    log("  Set AERO_ALLOW_UNSUPPORTED_NODE=1 to bypass this check.");
    log(
      `  (CI is pinned to Node v${expected.raw}; unsupported versions may cause toolchain hangs/flakiness.)`,
    );
    log("");
    log("Fix:");
    log(`- Install/use Node v${expected.raw} (the version CI uses).`);
    log("  If you use nvm:");
    log("    nvm install");
    log("    nvm use");
    log("");
    log("Override (unsupported):");
    log("  AERO_ALLOW_UNSUPPORTED_NODE=1 (skip Node version enforcement)");
    process.exit(1);
  }
} else if (enforceMajor && current.major !== expected.major) {
  const log = allowUnsupported ? console.warn : console.error;
  log(`${allowUnsupported ? "warning" : "error"}: Unsupported Node.js major version.`);
  log(`- Detected: v${current.raw}`);
  log(`- CI uses:  v${expected.raw} (from .nvmrc)`);
  log(`- Required (major): ${expected.major}.x`);
  log(`- Recommended: ${recommendedRange}`);

  if (allowUnsupported) {
    log(`- Override: AERO_ALLOW_UNSUPPORTED_NODE=1 (skipping Node version enforcement)`);
  } else {
    log("");
    log("Override (not recommended):");
    log("  Set AERO_ALLOW_UNSUPPORTED_NODE=1 to bypass this check.");
    log(
      `  (CI is pinned to Node v${expected.raw}; unsupported majors may cause toolchain hangs/flakiness.)`,
    );
    log("");
    log("Fix:");
    log(`- Install/use Node ${expected.major}.x (CI baseline).`);
    log("  If you use nvm:");
    log("    nvm install");
    log("    nvm use");
    log("");
    log("Override (unsupported):");
    log("  AERO_ALLOW_UNSUPPORTED_NODE=1 (skip Node version enforcement)");
    process.exit(1);
  }
} else if (current.raw !== expected.raw) {
  if (current.major !== expected.major) {
    console.warn("note: Node.js major version differs from CI baseline.");
  } else {
    console.warn("note: Node.js version differs from CI baseline.");
  }
  console.warn(`- Detected: v${current.raw}`);
  console.warn(`- CI uses:  v${expected.raw} (from .nvmrc)`);
  console.warn(`- Recommended: ${recommendedRange}`);
  if (current.major !== expected.major) {
    console.warn(
      `- This repo is CI-tested on Node ${expected.major}.x; newer majors may work but aren't covered.`,
    );
  }
  console.warn("If you see odd toolchain issues, align your local version:");
  console.warn("  nvm install && nvm use");
}
