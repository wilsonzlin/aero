import assert from "node:assert/strict";
import test from "node:test";
import { findEvalSinksInSource } from "./_helpers/eval_sink_scan_helpers.js";

test("eval sink scan: detects string-based timer eval", () => {
  const src = [
    'setTimeout("alert(1)", 0);',
    "setInterval(`alert(2)`, 0);",
    'window.setTimeout(/*x*/ "alert(3)", 0);',
    "setTimeout(() => 1, 0);",
  ].join("\n");

  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(kinds.includes("setIntervalString"), kinds.join("\n"));
  assert.ok(!kinds.includes("setTimeout"), "should not flag non-string timer usage");
});

test("eval sink scan: detects bracket-notation global eval and timers", () => {
  const src = [
    'globalThis["eval"]("alert(1)");',
    'window["setTimeout"]("alert(2)", 0);',
    'self["setInterval"](() => 1, 0);',
  ].join("\n");

  const kinds = findEvalSinksInSource(src).map((h) => h.kind).sort();
  assert.ok(kinds.includes("globalEvalBracket"), kinds.join("\n"));
  assert.ok(kinds.includes("setTimeoutString"), kinds.join("\n"));
  assert.ok(!kinds.includes("setIntervalString"), kinds.join("\n"));
});

