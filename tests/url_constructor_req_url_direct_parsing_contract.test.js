// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from "node:assert/strict";
import test from "node:test";

import { parseIdentifierWithUnicodeEscapes, skipWsAndComments } from "./_helpers/js_scan_parse_helpers.js";
import { stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

function isTokenAt(source, idx, token) {
  if (idx < 0) return false;
  if (source.slice(idx, idx + token.length) !== token) return false;
  const before = idx > 0 ? source[idx - 1] : "";
  const after = source[idx + token.length] || "";
  if (/[0-9A-Za-z_$]/u.test(before)) return false;
  if (/[0-9A-Za-z_$]/u.test(after)) return false;
  return true;
}

function findNewUrlCtorCall(source, masked, idx) {
  if (!isTokenAt(masked, idx, "new")) return null;
  let i = skipWsAndComments(source, idx + "new".length);
  if (!isTokenAt(masked, i, "URL")) return null;
  i = skipWsAndComments(source, i + "URL".length);
  if ((source[i] || "") !== "(") return null;
  return { callIdx: idx, arg0Start: i + 1 };
}

function findReqUrlAtStartOfArg0(source, masked, arg0Start, reqToken) {
  let i = skipWsAndComments(source, arg0Start);

  let parens = 0;
  while ((source[i] || "") === "(") {
    parens += 1;
    i = skipWsAndComments(source, i + 1);
  }

  if (!isTokenAt(masked, i, reqToken)) return null;
  i = skipWsAndComments(source, i + reqToken.length);

  while (parens > 0) {
    if ((source[i] || "") !== ")") return null;
    parens -= 1;
    i = skipWsAndComments(source, i + 1);
  }

  if ((source[i] || "") === "?" && (source[i + 1] || "") === ".") i += 2;
  else if ((source[i] || "") === ".") i += 1;
  else return null;

  i = skipWsAndComments(source, i);
  const ident = parseIdentifierWithUnicodeEscapes(source, i);
  if (!ident || ident.value !== "url") return null;

  return true;
}

function detect(code) {
  const masked = stripStringsAndComments(code);
  let idx = masked.indexOf("new");
  while (idx !== -1) {
    const call = findNewUrlCtorCall(code, masked, idx);
    if (call) {
      if (findReqUrlAtStartOfArg0(code, masked, call.arg0Start, "req")) return true;
      if (findReqUrlAtStartOfArg0(code, masked, call.arg0Start, "_req")) return true;
    }
    idx = masked.indexOf("new", idx + "new".length);
  }
  return false;
}

test("new URL req.url scan: detects direct req.url", () => {
  assert.equal(detect("new URL(req.url, base)"), true);
});

test("new URL req.url scan: detects parenthesized (req).url", () => {
  assert.equal(detect("new URL((req).url, base)"), true);
});

test("new URL req.url scan: detects double-parens ((req)).url", () => {
  assert.equal(detect("new URL(((req)).url, base)"), true);
});

test("new URL req.url scan: detects optional chaining req?.url", () => {
  assert.equal(detect("new URL(req?.url, base)"), true);
});

test("new URL req.url scan: detects unicode-escaped .u\\u0072l", () => {
  assert.equal(detect("new URL(req.u\\u0072l, base)"), true);
});

test("new URL req.url scan: does not match non-request variables", () => {
  assert.equal(detect("new URL(rawUrl, base)"), false);
  assert.equal(detect("new URL(request.url, base)"), false);
});

test("new URL req.url scan: does not match inside strings/comments", () => {
  assert.equal(detect('const s = "new URL(req.url, base)";'), false);
  assert.equal(detect("// new URL(req.url, base)\nnew URL(x, base)"), false);
});

