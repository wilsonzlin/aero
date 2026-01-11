import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function parseVersion(version) {
  const match = version.trim().match(/^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?$/);
  if (!match) return null;
  return {
    major: Number(match[1]),
    minor: match[2] === undefined ? 0 : Number(match[2]),
    patch: match[3] === undefined ? 0 : Number(match[3]),
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
  console.error(`Expected a version like "20.11.1", got: ${JSON.stringify(expectedRaw.trim())}`);
  process.exit(1);
}

const current = parseVersion(process.versions.node);
if (!current) {
  console.error("error: Unable to parse the current Node.js version");
  console.error(`node reports: ${JSON.stringify(process.versions.node)}`);
  process.exit(1);
}

const upperBoundMajor = expected.major + 1;
const supportedRange = `>=${formatVersion(expected)} <${upperBoundMajor}.0.0`;

const versionCmp = compareVersions(current, expected);
const majorMismatch = current.major !== expected.major;
const tooOld = majorMismatch || versionCmp < 0;

if (tooOld) {
  console.error("error: Unsupported Node.js version for this repo.");
  console.error(`- Detected: v${current.raw}`);
  console.error(`- Supported: ${supportedRange}`);
  console.error("");
  console.error("Fix:");
  console.error(`- Install/use Node v${expected.raw} (the version CI uses).`);
  console.error("  If you use nvm:");
  console.error("    nvm install");
  console.error("    nvm use");
  process.exit(1);
}

if (current.raw !== expected.raw) {
  console.warn("note: Node.js version differs from CI baseline.");
  console.warn(`- Detected: v${current.raw}`);
  console.warn(`- CI uses:  v${expected.raw} (from .nvmrc)`);
  console.warn("If you see odd toolchain issues, align your local version:");
  console.warn("  nvm install && nvm use");
}
