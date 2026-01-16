import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { repoRootFromTestUrl, runNodeScriptFromRepoRoot } from "./_helpers/action_contract_helpers.js";

const repoRoot = repoRootFromTestUrl(import.meta.url);

async function writeExecutableJs(filePath, content) {
  await fs.writeFile(filePath, content, { encoding: "utf8" });
}

test("setup-playwright precheck: writes missing=false when all install locations exist", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-pw-precheck-"));
  const outFile = path.join(tmp, "out.txt");

  const install1 = path.join(tmp, "pw-cache", "chromium");
  const install2 = path.join(tmp, "pw-cache", "firefox");
  await fs.mkdir(install1, { recursive: true });
  await fs.mkdir(install2, { recursive: true });

  const cliPath = path.join(tmp, "fake-playwright-cli.js");
  await writeExecutableJs(
    cliPath,
    `
      // Fake Playwright CLI: emit dry-run output.
      const installs = ${JSON.stringify([install1, install2])};
      for (const b of ['chromium', 'firefox']) console.log('browser:' + b);
      for (const p of installs) console.log('Install location: ' + p);
    `.trim(),
  );

  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-playwright/precheck.mjs", {
    GITHUB_OUTPUT: outFile,
    BROWSERS: "chromium firefox",
    PLAYWRIGHT_CLI: cliPath,
    AERO_ACTION_TIMEOUT_MS: "30000",
  });

  const raw = await fs.readFile(outFile, "utf8");
  assert.match(raw, /^missing=false$/m);
});

test("setup-playwright precheck: writes missing=true and lists missing paths", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-pw-precheck-"));
  const outFile = path.join(tmp, "out.txt");

  const missing1 = path.join(tmp, "pw-cache", "chromium");
  const missing2 = path.join(tmp, "pw-cache", "firefox");

  const cliPath = path.join(tmp, "fake-playwright-cli.js");
  await writeExecutableJs(
    cliPath,
    `
      const installs = ${JSON.stringify([missing1, missing2])};
      for (const b of ['chromium', 'firefox']) console.log('browser:' + b);
      for (const p of installs) console.log('Install location: ' + p);
    `.trim(),
  );

  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-playwright/precheck.mjs", {
    GITHUB_OUTPUT: outFile,
    BROWSERS: "chromium firefox",
    PLAYWRIGHT_CLI: cliPath,
    AERO_ACTION_TIMEOUT_MS: "30000",
  });

  const raw = await fs.readFile(outFile, "utf8");
  assert.match(raw, /^missing=true$/m);
  assert.match(raw, /missing_locations<<EOF_/);
  assert.ok(raw.includes(missing1));
  assert.ok(raw.includes(missing2));
});

