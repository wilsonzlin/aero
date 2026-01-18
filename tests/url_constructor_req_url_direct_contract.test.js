// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { isIdentContinue, parseIdentifierWithUnicodeEscapes, skipWsAndComments } from "./_helpers/js_scan_parse_helpers.js";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

function isIdentChar(ch) {
  return Boolean(ch) && isIdentContinue(ch);
}

function isTokenAt(source, idx, token) {
  if (idx < 0) return false;
  if (source.slice(idx, idx + token.length) !== token) return false;
  const before = idx > 0 ? source[idx - 1] : "";
  const after = source[idx + token.length] || "";
  if (isIdentChar(before)) return false;
  if (isIdentChar(after)) return false;
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

  // Accept harmless grouping parentheses around `req` / `_req`, e.g. `((req)).url`.
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

  // Require dot/optional-dot, then an identifier that decodes to `url`.
  if ((source[i] || "") === "?" && (source[i + 1] || "") === ".") i += 2;
  else if ((source[i] || "") === ".") i += 1;
  else return null;

  i = skipWsAndComments(source, i);
  const ident = parseIdentifierWithUnicodeEscapes(source, i);
  if (!ident || ident.value !== "url") return null;

  return { idx: arg0Start };
}

test("contract: do not call new URL(req.url, base) directly in production sources", async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    let idx = masked.indexOf("new");
    while (idx !== -1) {
      const call = findNewUrlCtorCall(source, masked, idx);
      if (call) {
        const foundReq = findReqUrlAtStartOfArg0(source, masked, call.arg0Start, "req");
        const found_Req = foundReq ? null : findReqUrlAtStartOfArg0(source, masked, call.arg0Start, "_req");
        if (foundReq || found_Req) {
          offenders.push({ rel, line: findLineNumber(source, call.callIdx) });
          break;
        }
      }
      idx = masked.indexOf("new", idx + "new".length);
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected new URL((req|_req).url, base) usage in production sources (avoid implicit coercion/trimming of hostile request targets):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

