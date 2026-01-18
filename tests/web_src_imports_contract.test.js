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

async function exists(absPath) {
  try {
    await fs.access(absPath);
    return true;
  } catch {
    return false;
  }
}

function shouldCheckWebSrcSpecifier(specifier) {
  if (!specifier.startsWith(".")) return false;
  if (!specifier.includes("/web/src/")) return false;
  // Only enforce when the import has an explicit extension; we don't want to
  // reimplement bundler/TS resolution rules here.
  const ext = path.posix.extname(specifier);
  if (!ext) return false;
  return ext === ".js" || ext === ".mjs" || ext === ".cjs" || ext === ".ts" || ext === ".tsx";
}

function scanImportSpecifiers(source) {
  /** @type {{ specifier: string; quoteIdx: number }[]} */
  const out = [];
  const masked = stripStringsAndComments(source);

  // Conservative scan: look for `import` keywords and then parse either:
  // - `import "x"`
  // - `import ... from "x"`
  // We skip comments and string literals while searching for `from`.
  for (let i = 0; i < source.length; i++) {
    // Avoid false positives from help text / commented code.
    if (!matchKeyword(masked, i, "import")) continue;

    let j = skipWsAndComments(source, i + "import".length);
    const direct = parseStringLiteralOrNoSubstTemplate(source, j);
    if (direct) {
      out.push({ specifier: direct.value, quoteIdx: j });
      i = direct.endIdxExclusive - 1;
      continue;
    }

    // Scan ahead for `from "<specifier>"`.
    while (j < source.length) {
      j = skipWsAndComments(source, j);
      if (j >= source.length) break;

      const ch = source[j] || "";
      if (ch === "'" || ch === '"' || ch === "`") {
        const lit = parseStringLiteralOrNoSubstTemplate(source, j);
        if (!lit) break;
        j = lit.endIdxExclusive;
        continue;
      }

      if (matchKeyword(source, j, "from")) {
        const fromEnd = j + "from".length;
        const k = skipWsAndComments(source, fromEnd);
        const spec = parseStringLiteralOrNoSubstTemplate(source, k);
        if (spec) out.push({ specifier: spec.value, quoteIdx: k });
        break;
      }

      j++;
    }
  }

  return out;
}

async function collectSources(dirAbs, dirRel) {
  /** @type {{ abs: string; rel: string }[]} */
  const out = [];
  const entries = await fs.readdir(dirAbs, { withFileTypes: true });
  for (const entry of entries) {
    const abs = path.join(dirAbs, entry.name);
    const rel = path.posix.join(dirRel, entry.name);
    if (entry.isDirectory()) {
      // Keep it simple and explicit; this is the test tree.
      if (entry.name === "node_modules" || entry.name === "dist" || entry.name === "build") continue;
      out.push(...(await collectSources(abs, rel)));
      continue;
    }
    if (!entry.isFile()) continue;
    if (!(rel.endsWith(".js") || rel.endsWith(".ts") || rel.endsWith(".mjs"))) continue;
    out.push({ abs, rel });
  }
  return out;
}

test("test sources: web/src import specifiers must exist on disk (when explicit extension is used)", async () => {
  const repoRoot = process.cwd();
  const roots = [
    // Repo-root tests (node:test + contract tests).
    "tests",
    // Workspace tests that intentionally import web runtime modules.
    "backend/aero-gateway/test",
  ];

  /** @type {{ abs: string; rel: string }[]} */
  const testFiles = [];
  for (const root of roots) {
    try {
      testFiles.push(...(await collectSources(path.join(repoRoot, root), root)));
    } catch {
      // Ignore missing roots in pruned checkouts.
    }
  }
  testFiles.sort((a, b) => a.rel.localeCompare(b.rel));

  const offenders = [];
  for (const { abs: absTestFile, rel: relTestFile } of testFiles) {
    const source = await fs.readFile(absTestFile, "utf8");
    const specifiers = scanImportSpecifiers(source);

    for (const { specifier, quoteIdx } of specifiers) {
      if (!shouldCheckWebSrcSpecifier(specifier)) continue;

      const absTarget = path.resolve(path.dirname(absTestFile), specifier);
      if (await exists(absTarget)) continue;

      offenders.push({
        test: relTestFile,
        specifier,
        line: findLineNumber(source, quoteIdx),
      });
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Tests import missing web/src modules:\n${offenders
      .map((o) => `- ${o.test}:${o.line} imports ${JSON.stringify(o.specifier)}`)
      .join("\n")}`,
  );
});

