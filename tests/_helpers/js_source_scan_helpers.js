import fs from "node:fs/promises";
import path from "node:path";

export const DEFAULT_SOURCE_ROOTS = ["src", "web", "backend", "server", "services", "tools", "scripts", "bench", "net-proxy", "proxy", "packages", "emulator"];
export const DEFAULT_EXTENSIONS = new Set([".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"]);

export function isIgnoredDir(name) {
  return name === "node_modules" || name === "dist" || name === "build" || name === "target" || name === ".git" || name === ".cargo" || name === ".turbo";
}

export function isTestPath(rel) {
  const parts = rel.split("/");
  if (parts.some((p) => p === "test" || p === "tests" || p === "__tests__" || p === "fixtures")) return true;
  const base = path.posix.basename(rel);
  return base.includes(".test.") || base.includes(".spec.");
}

export async function collectJsTsSourceFiles(repoRoot, roots = DEFAULT_SOURCE_ROOTS, extensions = DEFAULT_EXTENSIONS) {
  const files = [];
  for (const rootRel of roots) {
    const rootAbs = path.join(repoRoot, rootRel);
    try {
      files.push(...(await collectUnderRoot(rootAbs, rootRel, extensions)));
    } catch {
      // Ignore missing roots in pruned checkouts.
    }
  }
  return files.sort();
}

async function collectUnderRoot(rootAbs, rootRel, extensions) {
  const out = [];
  const entries = await fs.readdir(rootAbs, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(rootAbs, entry.name);
    const rel = `${rootRel}/${entry.name}`.replaceAll("\\", "/");
    if (entry.isDirectory()) {
      if (isIgnoredDir(entry.name)) continue;
      if (isTestPath(rel)) continue;
      out.push(...(await collectUnderRoot(full, rel, extensions)));
      continue;
    }
    if (!entry.isFile()) continue;
    const ext = path.extname(entry.name);
    if (!extensions.has(ext)) continue;
    if (isTestPath(rel)) continue;
    out.push(rel);
  }
  return out;
}

export function findLineNumber(text, index) {
  // 1-based line number.
  let line = 1;
  for (let i = 0; i < index; i++) {
    if (text.charCodeAt(i) === 10) line++;
  }
  return line;
}

export function stripStringsAndComments(source) {
  // Best-effort lexer to mask out string literals and comments so we avoid false positives
  // from help text / docs embedded in code. We preserve newlines and string length by replacing
  // masked characters with spaces.
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

