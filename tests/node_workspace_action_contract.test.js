import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { readKeyValueFile, repoRootFromTestUrl, runNodeScriptFromRepoRoot } from "./_helpers/action_contract_helpers.js";

const repoRoot = repoRootFromTestUrl(import.meta.url);

test("setup-node-workspace action: resolve-node-version trims CRLF and leading v", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-node-workspace-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.writeFile(path.join(tmp, ".nvmrc"), "v22.11.0\r\n", "utf8");
  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-node-workspace/resolve-node-version.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    AERO_ACTION_WORKING_DIRECTORY: ".",
    AERO_ACTION_NODE_VERSION: "",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("node_version"), "22.11.0");
});

test("setup-node-workspace action: detect-node-workspace selects root package.json and lockfile", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-node-workspace-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.writeFile(path.join(tmp, "package.json"), '{"name":"tmp","private":true}', "utf8");
  await fs.writeFile(path.join(tmp, "package-lock.json"), '{"name":"tmp","lockfileVersion":3}', "utf8");
  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-node-workspace/detect-node-workspace.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    AERO_ACTION_WORKING_DIRECTORY: ".",
    AERO_NODE_DIR: "",
    AERO_WEB_DIR: "",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("dir"), ".");
  assert.equal(outputs.get("lockfile"), "package-lock.json");
  assert.equal(outputs.get("install_dir"), ".");
});

test("setup-node-workspace action: detect-node-workspace falls back to web/ when root has no package.json", async () => {
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "aero-node-workspace-"));
  const outFile = path.join(tmp, "out.txt");

  await fs.mkdir(path.join(tmp, "web"));
  await fs.writeFile(path.join(tmp, "web", "package.json"), '{"name":"tmp-web","private":true}', "utf8");
  await fs.writeFile(path.join(tmp, "package-lock.json"), '{"name":"tmp","lockfileVersion":3}', "utf8");
  await fs.writeFile(outFile, "", "utf8");

  runNodeScriptFromRepoRoot(repoRoot, ".github/actions/setup-node-workspace/detect-node-workspace.mjs", {
    GITHUB_WORKSPACE: tmp,
    GITHUB_OUTPUT: outFile,
    AERO_ACTION_WORKING_DIRECTORY: ".",
    AERO_NODE_DIR: "",
    AERO_WEB_DIR: "",
  });

  const outputs = await readKeyValueFile(outFile);
  assert.equal(outputs.get("dir"), "web");
  assert.equal(outputs.get("lockfile"), "package-lock.json");
  assert.equal(outputs.get("install_dir"), ".");
});

