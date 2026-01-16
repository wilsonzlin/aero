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
  // Best-effort lexer to mask out string literals, regex literals, and comments so we avoid
  // false positives from help text / docs embedded in code. We preserve newlines and string
  // length by replacing masked characters with spaces.
  const out = source.split("");
  const len = out.length;

  let i = 0;
  let state = "normal"; // normal | template | template_expr | sq | dq | line_comment | block_comment | regex
  let resumeState = "normal"; // where sq/dq/comments/regex return
  let templateResumeState = "normal"; // where a template literal returns on closing backtick
  let templateExprDepth = 0;
  let regexInCharClass = false;

  // Context-sensitive regex literal detection.
  let canStartRegex = true;
  const KEYWORDS_EXPECT_EXPR = new Set([
    "return",
    "throw",
    "case",
    "else",
    "do",
    "yield",
    "await",
    "typeof",
    "instanceof",
    "void",
    "delete",
    "new",
    "in",
    "of",
  ]);

  const maskChar = (idx) => {
    if (out[idx] !== "\n") out[idx] = " ";
  };

  const isIdentStart = (c) => {
    const code = c.charCodeAt(0);
    return (code >= 65 && code <= 90) || (code >= 97 && code <= 122) || c === "_" || c === "$";
  };
  const isIdentPart = (c) => {
    const code = c.charCodeAt(0);
    return (
      (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122) ||
      (code >= 48 && code <= 57) ||
      c === "_" ||
      c === "$"
    );
  };
  const isDigit = (c) => {
    const code = c.charCodeAt(0);
    return code >= 48 && code <= 57;
  };

  const onPunct = (ch, next) => {
    // Tokens that strongly suggest an expression can start next.
    if (ch === "(" || ch === "[" || ch === "{" || ch === "," || ch === ";" || ch === ":" || ch === "?" || ch === "=") {
      canStartRegex = true;
      return false;
    }
    if (ch === ")" || ch === "]" || ch === "}") {
      canStartRegex = false;
      return false;
    }

    // Postfix operators end an expression.
    if ((ch === "+" && next === "+") || (ch === "-" && next === "-")) {
      canStartRegex = false;
      return true; // consumed 2 chars
    }

    // Most operators allow a new expression next.
    if (ch === "!" || ch === "~" || ch === "+" || ch === "-" || ch === "*" || ch === "%" || ch === "&" || ch === "|" || ch === "^" || ch === "<" || ch === ">") {
      canStartRegex = true;
      return false;
    }

    // Dot expects an identifier next; treat as "can't start regex".
    if (ch === ".") {
      canStartRegex = false;
      return false;
    }

    return false;
  };

  while (i < len) {
    const ch = source[i];
    const next = i + 1 < len ? source[i + 1] : "";

    if (state === "normal" || state === "template_expr") {
      // Regex literal start (only where it's grammatically plausible).
      if (ch === "/" && next !== "/" && next !== "*" && canStartRegex) {
        const returnTo = state;
        maskChar(i);
        state = "regex";
        resumeState = returnTo;
        regexInCharClass = false;
        i++;
        continue;
      }

      if (ch === "'" || ch === '"') {
        maskChar(i);
        resumeState = state;
        state = ch === "'" ? "sq" : "dq";
        canStartRegex = false;
        i++;
        continue;
      }

      if (ch === "`") {
        maskChar(i);
        templateResumeState = state;
        state = "template";
        canStartRegex = false;
        i++;
        continue;
      }

      if (ch === "/" && next === "/") {
        maskChar(i);
        maskChar(i + 1);
        resumeState = state;
        state = "line_comment";
        i += 2;
        continue;
      }
      if (ch === "/" && next === "*") {
        maskChar(i);
        maskChar(i + 1);
        resumeState = state;
        state = "block_comment";
        i += 2;
        continue;
      }

      if (state === "template_expr") {
        if (ch === "{") {
          templateExprDepth++;
          canStartRegex = true;
          i++;
          continue;
        }
        if (ch === "}") {
          templateExprDepth--;
          canStartRegex = false;
          if (templateExprDepth === 0) {
            state = "template";
          }
          i++;
          continue;
        }
      }

      if (isIdentStart(ch)) {
        let j = i + 1;
        while (j < len && isIdentPart(source[j])) j++;
        const word = source.slice(i, j);
        canStartRegex = KEYWORDS_EXPECT_EXPR.has(word);
        i = j;
        continue;
      }
      if (isDigit(ch)) {
        let j = i + 1;
        while (j < len && isDigit(source[j])) j++;
        canStartRegex = false;
        i = j;
        continue;
      }

      if (ch !== " " && ch !== "\t" && ch !== "\r" && ch !== "\n") {
        const consumed2 = onPunct(ch, next);
        if (consumed2) {
          i += 2;
          continue;
        }
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
        state = resumeState;
        canStartRegex = false;
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
        canStartRegex = true;
        i += 2;
        continue;
      }
      if (ch === "`") {
        state = templateResumeState;
        canStartRegex = false;
      }
      i++;
      continue;
    }

    if (state === "regex") {
      maskChar(i);
      if (ch === "\\") {
        if (i + 1 < len) maskChar(i + 1);
        i += 2;
        continue;
      }
      if (ch === "[") {
        regexInCharClass = true;
        i++;
        continue;
      }
      if (ch === "]") {
        regexInCharClass = false;
        i++;
        continue;
      }
      if (ch === "/" && !regexInCharClass) {
        i++;
        while (i < len) {
          const c = source[i];
          const code = c.charCodeAt(0);
          const isAlpha = (code >= 65 && code <= 90) || (code >= 97 && code <= 122);
          if (!isAlpha) break;
          maskChar(i);
          i++;
        }
        state = resumeState;
        canStartRegex = false;
        continue;
      }
      i++;
      continue;
    }

    if (state === "line_comment") {
      maskChar(i);
      if (ch === "\n") state = resumeState;
      i++;
      continue;
    }

    if (state === "block_comment") {
      maskChar(i);
      if (ch === "*" && next === "/") {
        maskChar(i + 1);
        state = resumeState;
        i += 2;
        continue;
      }
      i++;
      continue;
    }
  }

  return out.join("");
}

