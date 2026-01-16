import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { readKeyValueFile, repoRootFromTestUrl, runNodeScriptFromRepoRoot } from "./_helpers/action_contract_helpers.js";

const repoRoot = repoRootFromTestUrl(import.meta.url);

test("setup-playwright action: resolves working directory + lockfile without detect-node-dir", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-setup-playwright-"));
  const outFile = path.join(tmp, "out.txt");
  const envFile = path.join(tmp, "env.txt");

  await fs.mkdir(path.join(tmp, "web"), { recursive: true });
  await fs.writeFile(path.join(tmp, "web", "package.json"), '{"name":"tmp-web","private":true}', "utf8");
  await fs.writeFile(path.join(tmp, "package-lock.json"), '{"name":"tmp","lockfileVersion":3}', "utf8");
  await fs.writeFile(outFile, "", "utf8");
  await fs.writeFile(envFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-playwright/resolve-inputs.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    GITHUB_ENV: envFile,
    RUNNER_OS: "Linux",
    INPUT_BROWSERS: "",
    INPUT_PROJECT: "",
    INPUT_WORKING_DIRECTORY: "",
    INPUT_CACHE_PATH: "~/.cache/ms-playwright",
    INPUT_LOCKFILE: "",
    INPUT_WITH_DEPS: "",
    AERO_NODE_DIR: "",
    AERO_WEB_DIR: "",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("working_directory"), "web");
  assert.equal(outputs.get("lockfile"), "package-lock.json");
  assert.equal(outputs.get("cache_key_file"), "package-lock.json");
  assert.equal(outputs.get("browsers"), "chromium");
  assert.equal(outputs.get("with_deps"), "true");

  const envs = await readKeyValueFile(envFile);
  assert.ok(envs.get("PLAYWRIGHT_BROWSERS_PATH")?.includes("/.cache/ms-playwright"));
  assert.equal(envs.get("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD"), "1");
});

test("setup-playwright action: browsers_key is stable and deduped", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-setup-playwright-"));
  const outFile = path.join(tmp, "out.txt");
  const envFile = path.join(tmp, "env.txt");

  await fs.writeFile(path.join(tmp, "package.json"), '{"name":"tmp","private":true}', "utf8");
  await fs.writeFile(path.join(tmp, "package-lock.json"), '{"name":"tmp","lockfileVersion":3}', "utf8");
  await fs.writeFile(outFile, "", "utf8");
  await fs.writeFile(envFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-playwright/resolve-inputs.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    GITHUB_ENV: envFile,
    RUNNER_OS: "Linux",
    INPUT_BROWSERS: "firefox chromium chromium",
    INPUT_PROJECT: "",
    INPUT_WORKING_DIRECTORY: ".",
    INPUT_CACHE_PATH: "cache",
    INPUT_LOCKFILE: "",
    INPUT_WITH_DEPS: "false",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("browsers"), "firefox chromium chromium");
  assert.equal(outputs.get("browsers_key"), "chromium-firefox");
  assert.equal(outputs.get("with_deps"), "false");
});

