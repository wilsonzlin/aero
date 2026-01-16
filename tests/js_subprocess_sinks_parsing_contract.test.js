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

test("subprocess sink scan: detects exec usage via child_process namespace aliases", () => {
  const src = [
    'import * as cp from "node:child_process";',
    'cp.exec("echo hi");',
    'import cp2 from "child_process";',
    'cp2.execSync("echo hi");',
    'const cp3 = require("node:child_process");',
    'cp3.exec("echo hi");',
  ].join("\n");

  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind);
  assert.ok(kinds.includes("child_processNamespace.exec("), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync("), kinds.join("\n"));
});

test("subprocess sink scan: detects destructuring exec/execSync from child_process", () => {
  const src = [
    'const { exec } = require("child_process");',
    'let { execSync } = require("node:child_process");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["destructure exec child_process", "destructure execSync child_process"].sort());
});

test("subprocess sink scan: does not flag safe imports", () => {
  const src = [
    'import { execFileSync } from "child_process";',
    'import { spawnSync } from "node:child_process";',
    'require("child_process").execFileSync("echo hi");',
    'const { spawn, spawnSync } = require("node:child_process");',
    "export const ok = 1;",
  ].join("\n");
  assert.deepEqual(findSubprocessSinksInSource(src), []);
});

