import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readFile } from "node:fs/promises";

import { listFilesRecursive } from "./_helpers/fs_walk.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

function isExportStarShimForRepoSrc(text) {
  const trimmed = text.trim();
  // Keep this intentionally strict: shim modules should be single-line `export *` re-exports.
  return /^export\s+\*\s+from\s+["']\.\.(?:\/\.\.)+\/src\/[^"']+["'];$/.test(trimmed);
}

function findRepoSrcDeepImports(text) {
  /** @type {string[]} */
  const matches = [];

  // Treat runs of `/` and `\\` as separators (handles escaped or doubled separators).
  const deepRepoSrcRe = /^\.\.(?:[\\/]+\.\.)+[\\/]+src[\\/]+/;

  /** @param {string} spec */
  function isDeepRepoSrc(spec) {
    return deepRepoSrcRe.test(spec);
  }

  function isIdentStart(c) {
    return (c >= "A" && c <= "Z") || (c >= "a" && c <= "z") || c === "_" || c === "$";
  }

  function isIdentChar(c) {
    return isIdentStart(c) || (c >= "0" && c <= "9");
  }

  /** @param {number} i */
  function skipWhitespace(i) {
    while (i < text.length) {
      const c = text[i];
      if (c !== " " && c !== "\t" && c !== "\r" && c !== "\n") break;
      i += 1;
    }
    return i;
  }

  /**
   * Parse a JS/TS string literal starting at `i` and return `{ value, nextIdx }`.
   * Supports `'` and `"`; returns null for non-literals.
   *
   * @param {number} i
   * @returns {{ value: string; nextIdx: number } | null}
   */
  function parseStringLiteral(i) {
    const quote = text[i];
    if (quote !== "'" && quote !== '"') return null;
    i += 1;
    let out = "";
    while (i < text.length) {
      const c = text[i];
      if (c === quote) return { value: out, nextIdx: i + 1 };
      if (c === "\\") {
        const next = text[i + 1];
        if (next === undefined) return null;
        if (next === "u") {
          const u2 = text[i + 2];
          if (u2 === "{") {
            let j = i + 3;
            let hex = "";
            while (j < text.length) {
              const hc = text[j];
              if (hc === "}") break;
              hex += hc;
              j += 1;
            }
            if (j < text.length && text[j] === "}" && /^[0-9a-fA-F]{1,6}$/.test(hex)) {
              const cp = Number.parseInt(hex, 16);
              if (Number.isFinite(cp) && cp >= 0 && cp <= 0x10ffff) {
                out += String.fromCodePoint(cp);
                i = j + 1;
                continue;
              }
            }
            // Fallback: treat as a literal escape target.
            out += "u";
            i += 2;
            continue;
          }
          const hex = text.slice(i + 2, i + 6);
          if (/^[0-9a-fA-F]{4}$/.test(hex)) {
            out += String.fromCharCode(Number.parseInt(hex, 16));
            i += 6;
            continue;
          }
          out += "u";
          i += 2;
          continue;
        }
        if (next === "x") {
          const hex = text.slice(i + 2, i + 4);
          if (/^[0-9a-fA-F]{2}$/.test(hex)) {
            out += String.fromCharCode(Number.parseInt(hex, 16));
            i += 4;
            continue;
          }
          out += "x";
          i += 2;
          continue;
        }
        // Preserve the escape target as a best-effort approximation of the runtime string.
        // This is sufficient for detecting `../..` and separators.
        out += next;
        i += 2;
        continue;
      }
      out += c;
      i += 1;
    }
    return null;
  }

  let i = 0;
  while (i < text.length) {
    const c = text[i];

    // Line comment
    if (c === "/" && text[i + 1] === "/") {
      i += 2;
      while (i < text.length && text[i] !== "\n") i += 1;
      continue;
    }
    // Block comment
    if (c === "/" && text[i + 1] === "*") {
      i += 2;
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i += 1;
      i += 2;
      continue;
    }
    // Skip strings (we only want real import/require sites in code)
    if (c === "'" || c === '"') {
      const parsed = parseStringLiteral(i);
      if (!parsed) {
        i += 1;
      } else {
        i = parsed.nextIdx;
      }
      continue;
    }
    // Skip template literals entirely (rare in import specifiers; we keep this conservative)
    if (c === "`") {
      i += 1;
      while (i < text.length) {
        const tc = text[i];
        if (tc === "\\") {
          i += 2;
          continue;
        }
        if (tc === "`") {
          i += 1;
          break;
        }
        i += 1;
      }
      continue;
    }

    if (!isIdentStart(c)) {
      i += 1;
      continue;
    }

    const start = i;
    i += 1;
    while (i < text.length && isIdentChar(text[i])) i += 1;
    const word = text.slice(start, i);

    if (word === "from") {
      let j = skipWhitespace(i);
      const parsed = parseStringLiteral(j);
      if (parsed && isDeepRepoSrc(parsed.value)) matches.push(parsed.value);
      continue;
    }

    if (word === "import") {
      let j = skipWhitespace(i);
      if (text[j] === "(") {
        j = skipWhitespace(j + 1);
        const parsed = parseStringLiteral(j);
        if (parsed && isDeepRepoSrc(parsed.value)) matches.push(parsed.value);
        continue;
      }
      const parsed = parseStringLiteral(j);
      if (parsed && isDeepRepoSrc(parsed.value)) matches.push(parsed.value);
      continue;
    }

    if (word === "require") {
      let j = skipWhitespace(i);
      if (text[j] !== "(") continue;
      j = skipWhitespace(j + 1);
      const parsed = parseStringLiteral(j);
      if (parsed && isDeepRepoSrc(parsed.value)) matches.push(parsed.value);
      continue;
    }
  }

  return matches;
}

async function assertNoWorkspaceDeepImportsExceptShims(workspaceRel) {
  const srcDir = path.join(repoRoot, workspaceRel, "src");
  const relFiles = await listFilesRecursive(srcDir);
  const sourceFiles = relFiles
    .filter((rel) => {
      return (
        rel.endsWith(".ts") ||
        rel.endsWith(".tsx") ||
        rel.endsWith(".mts") ||
        rel.endsWith(".cts") ||
        rel.endsWith(".js") ||
        rel.endsWith(".mjs") ||
        rel.endsWith(".cjs")
      );
    })
    .sort();

  /** @type {Array<{file: string, spec: string, reason: string}>} */
  const violations = [];

  for (const rel of sourceFiles) {
    const abs = path.join(srcDir, rel);
    const content = await readFile(abs, "utf8");
    const specs = findRepoSrcDeepImports(content);
    if (specs.length === 0) continue;

    if (!isExportStarShimForRepoSrc(content)) {
      for (const spec of specs) {
        violations.push({
          file: `${workspaceRel}/src/${rel}`,
          spec,
          reason: "deep imports into repo-root src/ are only allowed in single-line export-star shim modules",
        });
      }
      continue;
    }

    if (specs.length !== 1) {
      for (const spec of specs) {
        violations.push({
          file: `${workspaceRel}/src/${rel}`,
          spec,
          reason: "shim modules must contain exactly one repo-root src/ re-export",
        });
      }
    }
  }

  assert.deepEqual(
    violations,
    [],
    `Workspace deep-import violations for ${workspaceRel}:\n${JSON.stringify(violations, null, 2)}`,
  );
}

test("module boundaries: workspaces must not deep-import repo-root src/ (except shim modules)", async () => {
  await assertNoWorkspaceDeepImportsExceptShims("net-proxy");
  await assertNoWorkspaceDeepImportsExceptShims("backend/aero-gateway");
});

test("module boundaries: workspace deep-import scan is comment-safe and decodes basic escapes", () => {
  assert.deepEqual(findRepoSrcDeepImports(`// import "../../src/x"\n`), []);
  assert.deepEqual(findRepoSrcDeepImports(`/* from "../../src/x" */\n`), []);
  assert.deepEqual(findRepoSrcDeepImports(`const s = "import \\"../../src/x\\"";\n`), []);

  assert.deepEqual(findRepoSrcDeepImports(`import "../../src/x";\n`), ["../../src/x"]);
  assert.deepEqual(findRepoSrcDeepImports(`import("..//..//src//x");\n`), ["..//..//src//x"]);
  assert.deepEqual(findRepoSrcDeepImports(`require("..\\\\..\\\\src\\\\x");\n`), ["..\\..\\src\\x"]);

  // Escaped separators should still be detected.
  assert.deepEqual(findRepoSrcDeepImports(`import "..\\u002f..\\u002fsrc\\u002fx";\n`), ["../../src/x"]);
});

