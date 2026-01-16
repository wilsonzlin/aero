import fs from "node:fs";
import { appendMultilineOutput, appendOutput, fail } from "../_shared/github_io.mjs";
import { actionTimeoutMs, execNodeCliUtf8 } from "../_shared/exec.mjs";

const browsers = (process.env.BROWSERS || "").split(/\s+/u).filter(Boolean);
if (!browsers.length) fail("setup-playwright: no browsers requested");

const cli = process.env.PLAYWRIGHT_CLI;
if (!cli) fail("setup-playwright: PLAYWRIGHT_CLI is not set");

const output = execNodeCliUtf8([cli, "install", "--dry-run", ...browsers], {
  env: process.env,
  stdio: ["ignore", "pipe", "inherit"],
  maxBuffer: 10 * 1024 * 1024,
  timeout: actionTimeoutMs(120_000),
});

const conciseOutput = output
  .split(/\r?\n/u)
  .filter((line) => line.startsWith("browser:") || line.trim().startsWith("Install location:"))
  .join("\n");
if (conciseOutput.trim()) process.stdout.write(`${conciseOutput}\n`);

const locations = [...output.matchAll(/^\s*Install location:\s*(.+)\s*$/gmu)].map((match) => match[1].trim());
const unique = [...new Set(locations)];
const missing = unique.filter((p) => !fs.existsSync(p));

if (missing.length) {
  console.log("Missing install locations:");
  for (const location of missing) console.log(`- ${location}`);
}

appendOutput("missing", missing.length > 0 ? "true" : "false");
appendMultilineOutput("missing_locations", missing.join("\n"));

