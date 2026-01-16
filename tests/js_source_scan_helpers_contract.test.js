import assert from "node:assert/strict";
import test from "node:test";
import { findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

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

test("js_source_scan: line comments end on U+2028/U+2029 line terminators", () => {
  const LS = "\u2028";
  const PS = "\u2029";
  const src = `// x${LS}const ok1 = 1; // y${PS}const ok2 = 2;`;
  const masked = stripStringsAndComments(src);
  assert.ok(masked.includes("const ok1 = 1;"), masked);
  assert.ok(masked.includes("const ok2 = 2;"), masked);
});

test("js_source_scan: line comments end on CR and CRLF", () => {
  const src = "// x\rconst ok1 = 1; // y\r\nconst ok2 = 2;";
  const masked = stripStringsAndComments(src);
  assert.ok(masked.includes("const ok1 = 1;"), masked);
  assert.ok(masked.includes("const ok2 = 2;"), masked);
});

test("js_source_scan: findLineNumber counts U+2028/U+2029 as line breaks", () => {
  const LS = "\u2028";
  const PS = "\u2029";
  const text = `a${LS}b${PS}c\nz`;
  assert.equal(findLineNumber(text, text.indexOf("b")), 2);
  assert.equal(findLineNumber(text, text.indexOf("c")), 3);
  assert.equal(findLineNumber(text, text.indexOf("z")), 4);
});

test("js_source_scan: findLineNumber treats CRLF as one line break", () => {
  const text = "a\r\nb\nc";
  assert.equal(findLineNumber(text, text.indexOf("b")), 2);
  assert.equal(findLineNumber(text, text.indexOf("c")), 3);
});

