import assert from "node:assert/strict";
import test from "node:test";
import { findSubprocessSinksInSource } from "./_helpers/subprocess_sink_scan_helpers.js";

test("subprocess sink scan: detects named import of exec/execSync from child_process", () => {
  const src = [
    'import { exec } from "node:child_process";',
    'import { execSync } from "child_process";',
    "export const ok = 1;",
  ].join("\n");

  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["import{execSync} child_process", "import{exec} child_process"].sort());
});

test("subprocess sink scan: detects require(child_process).exec/execSync", () => {
  const src = [
    'require("child_process").exec("echo hi");',
    'require("node:child_process").execSync("echo hi");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["require(child_process).exec(", "require(child_process).execSync("].sort());
});

test("subprocess sink scan: does not flag safe imports", () => {
  const src = [
    'import { execFileSync } from "child_process";',
    'import { spawnSync } from "node:child_process";',
    'require("child_process").execFileSync("echo hi");',
    "export const ok = 1;",
  ].join("\n");
  assert.deepEqual(findSubprocessSinksInSource(src), []);
});

