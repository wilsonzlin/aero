import assert from "node:assert/strict";
import test from "node:test";
import { findEvalSinksInSource } from "./_helpers/eval_sink_scan_helpers.js";

test("eval sink scan: detects string-based timer eval", () => {
  const src = [
    'setTimeout("alert(1)", 0);',
    'setTimeout?.("alert(1b)", 0);',
    "setInterval(`alert(2)`, 0);",
    "setInterval?.(`alert(2b)`, 0);",
    'setTime\\u006fut("alert(2b)", 0);',
    'setInter\\u0076al("alert(2c)", 0);',
    'setTime\\u{6f}ut("alert(2d)", 0);',
    'window.setTimeout(/*x*/ "alert(3)", 0);',
    'window.setTimeout?.("alert(3b)", 0);',
    "setTimeout(() => 1, 0);",
  ].join("\n");

  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(kinds.includes("setIntervalString"), kinds.join("\n"));
  assert.ok(!kinds.includes("setTimeout"), "should not flag non-string timer usage");
});

test("eval sink scan: detects optional-call timer strings without direct calls", () => {
  const src = [
    'setTimeout?.("alert(1)", 0);',
    "setInterval?.(`alert(2)`, 0);",
    'window.setTimeout?.("alert(3)", 0);',
    'window?.setInterval?.("alert(4)", 0);',
  ].join("\n");
  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(kinds.includes("setIntervalString"), kinds.join("\n"));
});

test("eval sink scan: does not flag optional-call timers when arg0 is not a string", () => {
  const src = [
    "setTimeout?.(() => 1, 0);",
    "window?.setInterval?.(() => 1, 0);",
  ].join("\n");
  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(!kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(!kinds.includes("setIntervalString"), kinds.join("\n"));
});

test("eval sink scan: detects unicode-escaped direct eval and Function identifiers", () => {
  const src = [
    'ev\\u0061l("alert(1)");',
    'ev\\u{61}l("alert(1b)");',
    'Funct\\u0069on("return 1")();',
    'Funct\\u{69}on("return 2")();',
  ].join("\n");
  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("eval"), kinds.join("\n"));
  assert.ok(kinds.includes("Function"), kinds.join("\n"));
});

test("eval sink scan: detects bracket-notation global eval and timers", () => {
  const src = [
    'globalThis["eval"]("alert(1)");',
    'globalThis?.["eval"]("alert(1b)");',
    'globalThis?.eval("alert(1c)");',
    'globalThis.e\\u0076al("alert(1c2)");',
    'globalThis.e\\u{76}al("alert(1c3)");',
    'globalThis["e\\u0076al"]("alert(1d)");',
    "globalThis[`eval`]('alert(1e)');",
    'const e0 = globalThis["eval"];',
    "const e1 = globalThis?.eval;",
    'window["setTimeout"]("alert(2)", 0);',
    'window?.["setTimeout"]?.("alert(2b)", 0);',
    'window?.setTimeout("alert(2c)", 0);',
    'window.setTime\\u006fut("alert(2c2)", 0);',
    'window.setTime\\u{6f}ut("alert(2c3)", 0);',
    'window["setTime\\x6fut"]("alert(2d)", 0);',
    "window[`setTime\\x6fut`]('alert(2e)', 0);",
    'self["setInterval"](() => 1, 0);',
  ].join("\n");

  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("globalEvalBracket"), kinds.join("\n"));
  assert.ok(kinds.includes("globalEval"), kinds.join("\n"));
  assert.ok(kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(!kinds.includes("setIntervalString"), kinds.join("\n"));
});

test("eval sink scan: detects global Function via bracket/dot access (including unicode escapes)", () => {
  const src = [
    'globalThis["Function"]("return 1");',
    'const F0 = globalThis?.["Function"];',
    "const F1 = globalThis?.Function;",
    "globalThis.Funct\\u0069on('return 2');",
    "globalThis[`Funct\\u0069on`]('return 3');",
  ].join("\n");
  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("Function"), kinds.join("\n"));
});

