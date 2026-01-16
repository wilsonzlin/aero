import fs from "node:fs";
import { execFileSync } from "node:child_process";
import { fail, requireEnv } from "../_shared/github_io.mjs";
import { actionTimeoutMs } from "../_shared/exec.mjs";

function writeOutput(text) {
  const outPath = requireEnv("GITHUB_OUTPUT");
  fs.appendFileSync(outPath, text, "utf8");
}

const browsers = (process.env.BROWSERS || "").split(/\s+/u).filter(Boolean);
if (!browsers.length) fail("setup-playwright: no browsers requested");

const cli = process.env.PLAYWRIGHT_CLI;
if (!cli) fail("setup-playwright: PLAYWRIGHT_CLI is not set");

const output = execFileSync(process.execPath, [cli, "install", "--dry-run", ...browsers], {
  env: process.env,
  stdio: ["ignore", "pipe", "inherit"],
  maxBuffer: 10 * 1024 * 1024,
  timeout: actionTimeoutMs(120_000),
}).toString("utf8");

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

const delimiter = `EOF_${Math.random().toString(16).slice(2)}`;
writeOutput(`missing=${missing.length > 0 ? "true" : "false"}\n`);
writeOutput(`missing_locations<<${delimiter}\n${missing.join("\n")}\n${delimiter}\n`);

