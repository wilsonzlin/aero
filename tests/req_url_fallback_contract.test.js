// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import {
  isIdentContinue,
  parseIdentifierWithUnicodeEscapes,
  parseStringLiteralOrNoSubstTemplate,
  skipWsAndComments,
} from "./_helpers/js_scan_parse_helpers.js";
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

function findTryGetUrlCallPattern(source, masked, idx, fn) {
  if (!isTokenAt(masked, idx, fn)) return null;

  let i = idx + fn.length;
  i = skipWsAndComments(source, i);
  if ((source[i] || "") !== "(") return null;

  // Find the argument separator comma and closing paren of this call using the masked source so
  // strings/comments/regex don't interfere with depth tracking.
  let commaIdx = -1;
  let parenDepth = 0;
  let bracketDepth = 0;
  let braceDepth = 0;
  let closeIdx = -1;

  for (let j = i + 1; j < masked.length; j++) {
    const ch = masked[j] || "";
    if (ch === "(") parenDepth++;
    else if (ch === ")") {
      if (parenDepth > 0) parenDepth--;
      else {
        closeIdx = j;
        break;
      }
    } else if (ch === "[") bracketDepth++;
    else if (ch === "]") {
      if (bracketDepth > 0) bracketDepth--;
    } else if (ch === "{") braceDepth++;
    else if (ch === "}") {
      if (braceDepth > 0) braceDepth--;
    } else if (ch === "," && parenDepth === 0 && bracketDepth === 0 && braceDepth === 0 && commaIdx === -1) {
      commaIdx = j;
    }
  }

  if (commaIdx === -1 || closeIdx === -1) return null;

  // Parse the second arg; we only care about string-literal `"url"` / `'url'` / `\`url\``.
  let k = skipWsAndComments(source, commaIdx + 1);
  const key = parseStringLiteralOrNoSubstTemplate(source, k);
  if (!key || key.value !== "url") return null;

  k = skipWsAndComments(source, key.endIdxExclusive);
  if (k !== closeIdx) return null;

  // Check for `?? "/"` or `|| "/"` fallback.
  k = skipWsAndComments(source, closeIdx + 1);
  const op0 = source[k] || "";
  const op1 = source[k + 1] || "";
  if (!((op0 === "?" && op1 === "?") || (op0 === "|" && op1 === "|"))) return null;
  k = skipWsAndComments(source, k + 2);

  const fallback = parseStringLiteralOrNoSubstTemplate(source, k);
  if (!fallback || fallback.value !== "/") return null;

  return { idx };
}

async function findTryGetUrlFallbackOffenders(repoRoot, sources, fn) {
  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    let idx = masked.indexOf(fn);
    while (idx !== -1) {
      const found = findTryGetUrlCallPattern(source, masked, idx, fn);
      if (found) {
        offenders.push({ rel, line: findLineNumber(source, found.idx) });
        break;
      }
      idx = masked.indexOf(fn, idx + fn.length);
    }
  }
  return offenders;
}

function findReqUrlNullishFallback(source, masked, idx, identToken) {
  // Match: `<ident>.url ?? "/"` / `<ident>.url || "/"` (or `'/'` or `\`/\``), optionally with
  // `<ident>?.url`.
  //
  // We restrict this to a small allow-list of request-shaped identifiers (currently: `req`, `_req`)
  // to keep the rule conservative: the intended guardrail is "don't silently default missing/hostile
  // request URLs to /".
  if (idx < 0) return null;
  if (!isTokenAt(masked, idx, identToken)) return null;

  let i = skipWsAndComments(source, idx + identToken.length);
  if ((source[i] || "") === "?" && (source[i + 1] || "") === ".") {
    i += 2;
  } else if ((source[i] || "") === ".") {
    i += 1;
  } else {
    return null;
  }

  i = skipWsAndComments(source, i);
  const ident = parseIdentifierWithUnicodeEscapes(source, i);
  if (!ident || ident.value !== "url") return null;
  i = skipWsAndComments(source, ident.endIdxExclusive);

  const op0 = source[i] || "";
  const op1 = source[i + 1] || "";
  if (!((op0 === "?" && op1 === "?") || (op0 === "|" && op1 === "|"))) return null;
  i = skipWsAndComments(source, i + 2);

  const fallback = parseStringLiteralOrNoSubstTemplate(source, i);
  if (!fallback || fallback.value !== "/") return null;

  return { idx };
}

test('contract: do not default tryGetProp(..., "url") to "/"', async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = await findTryGetUrlFallbackOffenders(repoRoot, sources, "tryGetProp");

  assert.equal(
    offenders.length,
    0,
    `Unexpected tryGetProp(..., "url") (??| ||) "/" usage in production sources (avoid treating missing/hostile URL as "/"):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

test('contract: do not default tryGetStringProp(..., "url") to "/"', async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = await findTryGetUrlFallbackOffenders(repoRoot, sources, "tryGetStringProp");

  assert.equal(
    offenders.length,
    0,
    `Unexpected tryGetStringProp(..., "url") (??| ||) "/" usage in production sources (avoid treating missing/hostile URL as "/"):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

test('contract: do not default req.url to "/"', async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    const scan = (token) => {
      let idx = masked.indexOf(token);
      while (idx !== -1) {
        const found = findReqUrlNullishFallback(source, masked, idx, token);
        if (found) {
          offenders.push({ rel, line: findLineNumber(source, found.idx) });
          return true;
        }
        idx = masked.indexOf(token, idx + token.length);
      }
      return false;
    };
    // `isTokenAt` rejects identifier-part boundaries, so we must scan `_req` separately from `req`.
    if (scan("req")) continue;
    if (scan("_req")) continue;
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected (req|_req).url (??| ||) "/" usage in production sources (avoid treating missing/hostile URL as "/"):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

