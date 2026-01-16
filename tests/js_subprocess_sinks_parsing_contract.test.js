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

test("subprocess sink scan: detects escaped child_process static import specifiers", () => {
  const src = [
    'import { exec } from "child\\x5fprocess";',
    'import * as cp from "node:child\\u005fprocess";',
    'cp.execSync("echo hi");',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("import{exec} child_process"), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync("), kinds.join("\n"));
});

test("subprocess sink scan: detects line-continuation child_process static import specifiers", () => {
  const src = `import { execSync } from "child_\\
process";
export const ok = 1;`;
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("import{execSync} child_process"), kinds.join("\n"));
});

test("subprocess sink scan: static import parsing is statement-local (handles side-effect imports and identifiers named from)", () => {
  const src = [
    'import "child_process";',
    'import { from } from "child_process";',
    'import { execSync } from "child_process";',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(kinds, ["import{execSync} child_process"]);
});

test("subprocess sink scan: detects require(child_process).exec/execSync", () => {
  const src = [
    'require("child_process").exec("echo hi");',
    'require("node:child_process").execSync("echo hi");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["require(child_process).exec(", "require(child_process).execSync("].sort());
});

test("subprocess sink scan: detects exec/execSync property access via require(child_process)", () => {
  const src = [
    'const e = require("child_process").exec;',
    'const s = require("node:child_process")["execSync"];',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["require(child_process).exec", "require(child_process).execSync"].sort());
});

test("subprocess sink scan: detects escaped child_process module specifiers", () => {
  const src = [
    'require("child\\x5fprocess").exec("echo hi");',
    'require("node:child\\u005fprocess").execSync("echo hi");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["require(child_process).exec(", "require(child_process).execSync("].sort());
});

test("subprocess sink scan: detects template literal child_process module specifiers", () => {
  const src = [
    "require(`child_process`).exec('echo hi');",
    "require(`node:child_process`).execSync('echo hi');",
    "const cp = await import(`child_process`); cp.exec('echo hi');",
    "const { execSync } = await import(`node:child_process`); execSync('echo hi');",
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("require(child_process).exec("), kinds.join("\n"));
  assert.ok(kinds.includes("require(child_process).execSync("), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.exec("), kinds.join("\n"));
  assert.ok(kinds.includes("destructure execSync child_process"), kinds.join("\n"));
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

test("subprocess sink scan: detects unicode-escaped exec/execSync property names", () => {
  const src = [
    'import * as cp from "child_process";',
    'cp.e\\u0078ec("echo hi");',
    'cp.e\\u{78}ec("echo hi");',
    'cp.e\\u0078ecSync("echo hi");',
    'cp.e\\u{78}ecSync("echo hi");',
    'require("node:child_process").e\\u0078ecSync("echo hi");',
    'require("node:child_process").e\\u{78}ecSync("echo hi");',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind);
  assert.ok(kinds.includes("child_processNamespace.exec("), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync("), kinds.join("\n"));
  assert.ok(kinds.includes("require(child_process).execSync("), kinds.join("\n"));
});

test("subprocess sink scan: detects exec/execSync property access via child_process namespace aliases", () => {
  const src = [
    'import * as cp from "child_process";',
    "const e = cp.exec;",
    'const s = cp["execSync"];',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("child_processNamespace.exec"), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync"), kinds.join("\n"));
});

test("subprocess sink scan: detects destructuring exec/execSync from child_process", () => {
  const src = [
    'const { exec } = require("child_process");',
    'let { execSync } = require("node:child_process");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["destructure exec child_process", "destructure execSync child_process"].sort());
});

test("subprocess sink scan: detects destructuring exec/execSync from dynamic import child_process", () => {
  const src = [
    'const { exec } = await import("child_process");',
    'let { execSync } = await import("node:child_process");',
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["destructure exec child_process", "destructure execSync child_process"].sort());
});

test("subprocess sink scan: detects exec usage via dynamic import child_process aliases", () => {
  const src = [
    'const cp = await import("child_process");',
    'cp.exec("echo hi");',
    'const cp2 = await import(/*ok*/ "node:child_process");',
    'cp2.execSync("echo hi");',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind);
  assert.ok(kinds.includes("child_processNamespace.exec("), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync("), kinds.join("\n"));
});

test("subprocess sink scan: detects exec/execSync via awaited dynamic import member access", () => {
  const src = [
    '(await import("child_process")).exec("echo hi");',
    '(await import("node:child_process"))?.["execSync"]?.("echo hi");',
    '(await import(`child_process`)).default.execSync("echo hi");',
    '(await import(`node:child_process`))?.["default"]?.["exec"]?.("echo hi");',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind);
  assert.ok(kinds.includes("awaitImport(child_process).exec("), kinds.join("\n"));
  assert.ok(kinds.includes("awaitImport(child_process).execSync("), kinds.join("\n"));
  assert.ok(kinds.includes("awaitImport(child_process).default.execSync("), kinds.join("\n"));
  assert.ok(kinds.includes("awaitImport(child_process).default.exec("), kinds.join("\n"));
});

test("subprocess sink scan: detects bracket-notation exec/execSync calls", () => {
  const src = [
    'require("child_process")["exec"]("echo hi");',
    'require("node:child_process")[\'execSync\']("echo hi");',
    'require("child_process")?.exec?.("echo hi");',
    'require("node:child_process")?.["execSync"]?.("echo hi");',
    'import * as cp from "child_process";',
    'cp["execSync"]?.("echo hi");',
  ].join("\n");
  const kinds = findSubprocessSinksInSource(src).map((h) => h.kind);
  assert.ok(kinds.includes("require(child_process).exec("), kinds.join("\n"));
  assert.ok(kinds.includes("require(child_process).execSync("), kinds.join("\n"));
  assert.ok(kinds.includes("child_processNamespace.execSync("), kinds.join("\n"));
});

test("subprocess sink scan: detects destructuring exec/execSync from child_process aliases", () => {
  const src = [
    'const cp = require("child_process");',
    "const { exec, execSync } = cp;",
  ].join("\n");
  const hits = findSubprocessSinksInSource(src).map((h) => h.kind).sort();
  assert.deepEqual(hits, ["destructure exec child_processNamespace", "destructure execSync child_processNamespace"].sort());
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

