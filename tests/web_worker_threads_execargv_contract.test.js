import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";
import {
  matchKeyword,
  parseStringLiteralOrNoSubstTemplate,
  skipWsAndComments,
} from "./_helpers/js_scan_parse_helpers.js";

async function collectSources(dirAbs, dirRel) {
  /** @type {{ abs: string; rel: string }[]} */
  const out = [];
  const entries = await fs.readdir(dirAbs, { withFileTypes: true });
  for (const entry of entries) {
    const abs = path.join(dirAbs, entry.name);
    const rel = path.posix.join(dirRel, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "dist" || entry.name === "build") continue;
      out.push(...(await collectSources(abs, rel)));
      continue;
    }
    if (!entry.isFile()) continue;
    if (
      !(
        rel.endsWith(".js") ||
        rel.endsWith(".mjs") ||
        rel.endsWith(".cjs") ||
        rel.endsWith(".ts") ||
        rel.endsWith(".tsx")
      )
    ) {
      continue;
    }
    out.push({ abs, rel });
  }
  return out;
}

function parseExecArgvStringLiterals(source, startIdx) {
  // Parses an `execArgv: [ ... ]` array and returns:
  // - the set of *string literal* values observed
  // - the index of the property key (for line-number reporting)
  //
  // This is intentionally conservative:
  // - It only cares whether `--experimental-strip-types` appears.
  // - If so, it requires `--import` to also appear somewhere in the array.
  // - It ignores non-string elements (e.g. `registerUrl.href`) but still parses
  //   commas/brackets safely with a shallow expression skipper.

  const keyIdx = startIdx;
  let i = skipWsAndComments(source, startIdx + "execArgv".length);
  if ((source[i] || "") !== ":") return null;
  i = skipWsAndComments(source, i + 1);
  if ((source[i] || "") !== "[") return null;
  i++;

  /** @type {string[]} */
  const strings = [];

  for (;;) {
    i = skipWsAndComments(source, i);
    if (i >= source.length) return null;
    const ch = source[i] || "";
    if (ch === "]") return { keyIdx, strings };

    const lit = parseStringLiteralOrNoSubstTemplate(source, i);
    if (lit) {
      strings.push(lit.value);
      i = lit.endIdxExclusive;
    } else {
      // Skip a single non-string expression until the next comma or closing `]`
      // at the current nesting depth. This handles common cases like
      // `registerUrl.href` without trying to parse JS fully.
      let depth = 0;
      while (i < source.length) {
        i = skipWsAndComments(source, i);
        if (i >= source.length) return null;
        const c = source[i] || "";

        if (c === "'" || c === '"' || c === "`") {
          const s = parseStringLiteralOrNoSubstTemplate(source, i);
          if (!s) return null;
          i = s.endIdxExclusive;
          continue;
        }

        if (depth === 0 && (c === "," || c === "]")) break;

        if (c === "(" || c === "[" || c === "{") {
          depth++;
          i++;
          continue;
        }
        if (c === ")" || c === "]" || c === "}") {
          if (depth > 0) depth--;
          i++;
          continue;
        }
        i++;
      }
    }

    i = skipWsAndComments(source, i);
    const tail = source[i] || "";
    if (tail === ",") {
      i++;
      continue;
    }
    if (tail === "]") return { keyIdx, strings };
    return null;
  }
}

test("web worker_threads tests: execArgv using --experimental-strip-types must include --import", async () => {
  const repoRoot = process.cwd();
  const roots = ["web/test", "web/src"];

  /** @type {{ abs: string; rel: string }[]} */
  const files = [];
  for (const root of roots) {
    try {
      files.push(...(await collectSources(path.join(repoRoot, root), root)));
    } catch {
      // Ignore missing roots in pruned checkouts.
    }
  }
  files.sort((a, b) => a.rel.localeCompare(b.rel));

  const offenders = [];

  for (const { abs, rel } of files) {
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    for (let i = 0; i < masked.length; i++) {
      if (!matchKeyword(masked, i, "execArgv")) continue;
      const parsed = parseExecArgvStringLiterals(source, i);
      if (!parsed) continue;
      const set = new Set(parsed.strings);
      if (!set.has("--experimental-strip-types")) continue;
      if (set.has("--import")) continue;

      offenders.push({
        file: rel,
        line: findLineNumber(source, parsed.keyIdx),
        strings: parsed.strings,
      });
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Some web Node worker_threads tests set execArgv with "--experimental-strip-types" but no "--import" (workers won't register the TS-strip loader):\n${offenders
      .map((o) => `- ${o.file}:${o.line} execArgv strings=${JSON.stringify(o.strings)}`)
      .join("\n")}`,
  );
});

