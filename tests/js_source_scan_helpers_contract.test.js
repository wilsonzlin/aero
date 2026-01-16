import assert from "node:assert/strict";
import test from "node:test";
import { stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

test("js_source_scan: regex literals do not start line comments", () => {
  // `/\//` ends with `//` in source; a naive comment masker can treat that as `//` comment start.
  const src = "const re = /\\//; const ok = 1;";
  const masked = stripStringsAndComments(src);
  assert.ok(masked.includes("const ok = 1;"), masked);
});

test("js_source_scan: regex literals after return are handled", () => {
  const src = "function f(){ return /\\//.test(x); } const ok = 1;";
  const masked = stripStringsAndComments(src);
  assert.ok(masked.includes("const ok = 1;"), masked);
});

test("js_source_scan: template expressions keep scanning after nested strings/regex", () => {
  const src = "const s = `${/\\//.test(x)}`; const ok = 1;";
  const masked = stripStringsAndComments(src);
  assert.ok(masked.includes("const ok = 1;"), masked);
});

