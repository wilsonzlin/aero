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

function findUrlCtorCall(source, masked, idx) {
  if (!isTokenAt(masked, idx, "new")) return null;
  let i = skipWsAndComments(source, idx + "new".length);
  if (!isTokenAt(masked, i, "URL")) return null;
  i = skipWsAndComments(source, i + "URL".length);
  if ((source[i] || "") !== "(") return null;

  // Find the comma separating arg0/arg1 at depth 0, and the closing paren.
  const argStart = i + 1;
  let commaIdx = -1;
  let parenDepth = 0;
  let bracketDepth = 0;
  let braceDepth = 0;
  let closeIdx = -1;
  for (let j = argStart; j < masked.length; j++) {
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
  return { callIdx: idx, arg0Start: argStart, arg0EndExclusive: commaIdx };
}

function findReqUrlEmptyFallbackInArg0(source, masked, idx, identToken, arg0EndExclusive) {
  if (!isTokenAt(masked, idx, identToken)) return null;
  let i = skipWsAndComments(source, idx + identToken.length);
  if ((source[i] || "") === "?" && (source[i + 1] || "") === ".") i += 2;
  else if ((source[i] || "") === ".") i += 1;
  else return null;

  i = skipWsAndComments(source, i);
  const ident = parseIdentifierWithUnicodeEscapes(source, i);
  if (!ident || ident.value !== "url") return null;
  i = skipWsAndComments(source, ident.endIdxExclusive);

  const op0 = source[i] || "";
  const op1 = source[i + 1] || "";
  if (!((op0 === "?" && op1 === "?") || (op0 === "|" && op1 === "|"))) return null;
  i = skipWsAndComments(source, i + 2);

  const fallback = parseStringLiteralOrNoSubstTemplate(source, i);
  if (!fallback || fallback.value !== "") return null;

  i = skipWsAndComments(source, fallback.endIdxExclusive);
  if (i !== arg0EndExclusive) return null;

  return { idx };
}

function findTryGetUrlEmptyFallbackInArg0(source, masked, idx, fn, arg0EndExclusive) {
  if (!isTokenAt(masked, idx, fn)) return null;

  let i = idx + fn.length;
  i = skipWsAndComments(source, i);
  if ((source[i] || "") !== "(") return null;

  // Find the argument separator comma and closing paren of this call.
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

  let k = skipWsAndComments(source, commaIdx + 1);
  const key = parseStringLiteralOrNoSubstTemplate(source, k);
  if (!key || key.value !== "url") return null;
  k = skipWsAndComments(source, key.endIdxExclusive);
  if (k !== closeIdx) return null;

  k = skipWsAndComments(source, closeIdx + 1);
  const op0 = source[k] || "";
  const op1 = source[k + 1] || "";
  if (!((op0 === "?" && op1 === "?") || (op0 === "|" && op1 === "|"))) return null;
  k = skipWsAndComments(source, k + 2);

  const fallback = parseStringLiteralOrNoSubstTemplate(source, k);
  if (!fallback || fallback.value !== "") return null;

  k = skipWsAndComments(source, fallback.endIdxExclusive);
  if (k !== arg0EndExclusive) return null;

  return { idx };
}

function findFirstMatchInRange(masked, token, startIdxInclusive, endIdxExclusive) {
  const idx = masked.indexOf(token, startIdxInclusive);
  if (idx === -1) return -1;
  if (idx >= endIdxExclusive) return -1;
  return idx;
}

test('contract: do not call new URL(req.url ?? "", base) (or tryGet*Prop url ?? "")', async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    let foundInFile = false;
    let idx = masked.indexOf("new");
    while (idx !== -1) {
      const call = findUrlCtorCall(source, masked, idx);
      if (call) {
        // Only consider the first argument expression (everything before the top-level comma).
        // We scan conservatively for request-ish patterns only.
        const tokens = ["req", "_req", "tryGetProp", "tryGetStringProp"];
        for (const token of tokens) {
          let j = findFirstMatchInRange(masked, token, call.arg0Start, call.arg0EndExclusive);
          while (j !== -1) {
            const found =
              token === "req" || token === "_req"
                ? findReqUrlEmptyFallbackInArg0(source, masked, j, token, call.arg0EndExclusive)
                : findTryGetUrlEmptyFallbackInArg0(source, masked, j, token, call.arg0EndExclusive);
            if (found) {
              offenders.push({ rel, line: findLineNumber(source, found.idx) });
              foundInFile = true;
              break;
            }
            j = findFirstMatchInRange(masked, token, j + token.length, call.arg0EndExclusive);
          }
          if (foundInFile) break;
        }
      }
      idx = masked.indexOf("new", idx + "new".length);
      if (foundInFile) break;
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected new URL(<req-ish>.url (??| ||) "" , base) usage in production sources (empty string resolves to base URL like "/" when a base is provided):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

