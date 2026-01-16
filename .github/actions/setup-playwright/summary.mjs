import fs from "node:fs";
import { execFileSync } from "node:child_process";
import { actionTimeoutMs } from "../_shared/exec.mjs";

function safeString(value) {
  try {
    return String(value ?? "");
  } catch {
    return "";
  }
}

const summaryPath = process.env.GITHUB_STEP_SUMMARY;
if (!summaryPath) process.exit(0);

const cachePath = safeString(process.env.CACHE_PATH);
const browsers = safeString(process.env.BROWSERS)
  .split(/\s+/u)
  .filter(Boolean);
const cli = safeString(process.env.PLAYWRIGHT_CLI);

let cacheEntries = [];
try {
  cacheEntries = fs
    .readdirSync(cachePath, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
} catch {
  // Cache path may not exist yet on a cold start; that's fine.
}

let dryRunSummary = "";
try {
  const dryRun = execFileSync(process.execPath, [cli, "install", "--dry-run", ...browsers], {
    env: process.env,
    maxBuffer: 10 * 1024 * 1024,
    timeout: actionTimeoutMs(120_000),
  }).toString("utf8");
  dryRunSummary = dryRun
    .split(/\r?\n/u)
    .filter((line) => line.startsWith("browser:") || line.trim().startsWith("Install location:"))
    .join("\n");
} catch (err) {
  dryRunSummary = `Failed to run Playwright dry-run: ${safeString(err)}`;
}

const lines = [
  "### Playwright setup",
  "",
  `- Working directory: \`${safeString(process.env.WORKING_DIRECTORY)}\``,
  `- Browsers: \`${safeString(process.env.BROWSERS)}\``,
  `- Playwright version: \`${safeString(process.env.PLAYWRIGHT_VERSION)}\``,
  `- Cache key file: \`${safeString(process.env.CACHE_KEY_FILE)}\``,
  `- Cache path: \`${cachePath}\``,
  `- Cache hit: \`${safeString(process.env.CACHE_HIT)}\``,
  `- Browser install needed: \`${safeString(process.env.NEEDS_INSTALL)}\``,
];

if (cacheEntries.length) {
  const shown = cacheEntries.slice(0, 40).map((entry) => `  - ${entry}`);
  lines.push("", "Cache entries (first 40):", ...shown);
}

if (dryRunSummary.trim()) {
  lines.push("", "Dry-run (install locations):", "```text", dryRunSummary.trim(), "```");
}

fs.appendFileSync(summaryPath, `${lines.join("\n")}\n`, "utf8");

