// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { isIdentContinue, parseStringLiteralOrNoSubstTemplate, skipWsAndComments } from "./_helpers/js_scan_parse_helpers.js";
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

function findNewUrlEmptyFirstArg(source, masked, idx) {
  if (!isTokenAt(masked, idx, "new")) return null;
  let i = skipWsAndComments(source, idx + "new".length);
  if (!isTokenAt(masked, i, "URL")) return null;
  i = skipWsAndComments(source, i + "URL".length);
  if ((source[i] || "") !== "(") return null;
  i = skipWsAndComments(source, i + 1);

  const first = parseStringLiteralOrNoSubstTemplate(source, i);
  if (!first || first.value !== "") return null;
  i = skipWsAndComments(source, first.endIdxExclusive);

  // Only flag when the first argument expression is exactly the empty string literal.
  const next = source[i] || "";
  if (next !== "," && next !== ")") return null;

  return { idx };
}

test('contract: do not call new URL("") in production sources', async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    let idx = masked.indexOf("new");
    while (idx !== -1) {
      const found = findNewUrlEmptyFirstArg(source, masked, idx);
      if (found) {
        offenders.push({ rel, line: findLineNumber(source, found.idx) });
        break;
      }
      idx = masked.indexOf("new", idx + "new".length);
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected new URL("") usage in production sources (empty request targets can silently become "/" when a base URL is provided):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

