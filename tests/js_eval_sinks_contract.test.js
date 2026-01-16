import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const SOURCE_ROOTS = ["src", "web", "backend", "server", "services", "tools", "scripts", "bench", "net-proxy", "proxy", "packages", "emulator"];
const EXTENSIONS = new Set([".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"]);

function isIgnoredDir(name) {
  return name === "node_modules" || name === "dist" || name === "build" || name === "target" || name === ".git" || name === ".cargo" || name === ".turbo";
}

function isTestPath(rel) {
  const parts = rel.split("/");
  if (parts.some((p) => p === "test" || p === "tests" || p === "__tests__" || p === "fixtures")) return true;
  const base = path.posix.basename(rel);
  return base.includes(".test.") || base.includes(".spec.");
}

function leadingSpaces(line) {
  let n = 0;
  while (n < line.length && line.charCodeAt(n) === 32) n++;
  return n;
}

async function collectSourceFiles(rootAbs, rootRel) {
  const out = [];
  const entries = await fs.readdir(rootAbs, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(rootAbs, entry.name);
    const rel = `${rootRel}/${entry.name}`.replaceAll("\\", "/");
    if (entry.isDirectory()) {
      if (isIgnoredDir(entry.name)) continue;
      if (isTestPath(rel)) continue;
      out.push(...(await collectSourceFiles(full, rel)));
      continue;
    }
    if (!entry.isFile()) continue;
    const ext = path.extname(entry.name);
    if (!EXTENSIONS.has(ext)) continue;
    if (isTestPath(rel)) continue;
    out.push(rel);
  }
  return out;
}

function findLineNumber(text, index) {
  // 1-based line number.
  let line = 1;
  for (let i = 0; i < index; i++) {
    if (text.charCodeAt(i) === 10) line++;
  }
  return line;
}

function findEvalSinks(content) {
  const hits = [];

  // Direct eval() call (not obj.eval()).
  const directEvalRe = /(^|[^.\w$])eval\s*\(/gmu;
  for (;;) {
    const m = directEvalRe.exec(content);
    if (!m) break;
    hits.push({ kind: "eval", index: m.index });
  }

  // Explicit global eval references (these are still eval sinks).
  const globalEvalRe = /\b(globalThis|window|self)\s*\.\s*eval\s*\(/gmu;
  for (;;) {
    const m = globalEvalRe.exec(content);
    if (!m) break;
    hits.push({ kind: "globalEval", index: m.index });
  }

  // Function constructor.
  const newFunctionRe = /\bnew\s+Function\s*\(/gmu;
  for (;;) {
    const m = newFunctionRe.exec(content);
    if (!m) break;
    hits.push({ kind: "newFunction", index: m.index });
  }

  return hits;
}

function stripStringsAndComments(source) {
  // Best-effort lexer to mask out string literals and comments so we avoid false positives
  // from help text / docs embedded in code.
  //
  // We preserve newlines and string length by replacing masked characters with spaces.
  const out = source.split("");
  const len = out.length;

  let i = 0;
  let state = "normal"; // normal | sq | dq | template | line_comment | block_comment | template_expr
  let templateExprDepth = 0;

  const maskChar = (idx) => {
    if (out[idx] !== "\n") out[idx] = " ";
  };

  while (i < len) {
    const ch = source[i];
    const next = i + 1 < len ? source[i + 1] : "";

    if (state === "normal") {
      if (ch === "'" || ch === '"' || ch === "`") {
        maskChar(i);
        state = ch === "'" ? "sq" : ch === '"' ? "dq" : "template";
        i++;
        continue;
      }
      if (ch === "/" && next === "/") {
        maskChar(i);
        maskChar(i + 1);
        state = "line_comment";
        i += 2;
        continue;
      }
      if (ch === "/" && next === "*") {
        maskChar(i);
        maskChar(i + 1);
        state = "block_comment";
        i += 2;
        continue;
      }
      i++;
      continue;
    }

    if (state === "sq" || state === "dq") {
      maskChar(i);
      if (ch === "\\") {
        if (i + 1 < len) maskChar(i + 1);
        i += 2;
        continue;
      }
      if ((state === "sq" && ch === "'") || (state === "dq" && ch === '"')) {
        state = "normal";
      }
      i++;
      continue;
    }

    if (state === "template") {
      maskChar(i);
      if (ch === "\\") {
        if (i + 1 < len) maskChar(i + 1);
        i += 2;
        continue;
      }
      if (ch === "$" && next === "{") {
        maskChar(i + 1);
        state = "template_expr";
        templateExprDepth = 1;
        i += 2;
        continue;
      }
      if (ch === "`") {
        state = "normal";
      }
      i++;
      continue;
    }

    if (state === "template_expr") {
      // Inside `${ ... }`, we keep code (do not mask), but we still need to track nested braces.
      if (ch === "'" || ch === '"' || ch === "`") {
        // Enter a nested string state; mask the string itself but preserve surrounding code.
        maskChar(i);
        state = ch === "'" ? "sq" : ch === '"' ? "dq" : "template";
        i++;
        continue;
      }
      if (ch === "/" && next === "/") {
        maskChar(i);
        maskChar(i + 1);
        state = "line_comment";
        i += 2;
        continue;
      }
      if (ch === "/" && next === "*") {
        maskChar(i);
        maskChar(i + 1);
        state = "block_comment";
        i += 2;
        continue;
      }
      if (ch === "{") {
        templateExprDepth++;
      } else if (ch === "}") {
        templateExprDepth--;
        if (templateExprDepth === 0) {
          // Closing brace of `${...}`; return to template text.
          state = "template";
        }
      }
      i++;
      continue;
    }

    if (state === "line_comment") {
      maskChar(i);
      if (ch === "\n") state = "normal";
      i++;
      continue;
    }

    if (state === "block_comment") {
      maskChar(i);
      if (ch === "*" && next === "/") {
        maskChar(i + 1);
        state = "normal";
        i += 2;
        continue;
      }
      i++;
      continue;
    }
  }

  return out.join("");
}

test("contract: no JS eval sinks in production sources", async () => {
  const roots = SOURCE_ROOTS.map((p) => ({ rel: p, abs: path.join(repoRoot, p) }));

  const files = [];
  for (const root of roots) {
    try {
      files.push(...(await collectSourceFiles(root.abs, root.rel)));
    } catch {
      // Some roots may not exist in pruned checkouts; ignore.
    }
  }

  const allowlist = new Set([
    // Intentional CSP gate fixture: contains eval() to prove CSP blocks it.
    "web/public/assets/security_headers_worker.js",
  ]);

  const violations = [];
  for (const rel of files.sort()) {
    if (allowlist.has(rel)) continue;
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);
    const hits = findEvalSinks(masked);
    for (const hit of hits) {
      const line = findLineNumber(content, hit.index);
      violations.push({ file: rel, line, kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `eval sink violations: ${JSON.stringify(violations, null, 2)}`);
});

