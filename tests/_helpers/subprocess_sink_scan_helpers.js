import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import {
  isOptionalCallStart,
  isIdentContinue,
  isSpace,
  matchKeyword,
  parseBracketStringProperty,
  parseIdentifierWithUnicodeEscapes,
  parseStringLiteralOrNoSubstTemplate,
  parseQuotedStringLiteral,
  skipWsAndComments,
} from "./js_scan_parse_helpers.js";

function isChildProcessSpecifier(spec) {
  return spec === "child_process" || spec === "node:child_process";
}

function escapeRegex(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function isIdentPart(ch) {
  return isIdentContinue(ch);
}

const KEYWORDS_EXPECT_EXPR = new Set([
  "return",
  "throw",
  "case",
  "else",
  "do",
  "yield",
  "await",
  "typeof",
  "void",
  "delete",
  "new",
]);

function wordBefore(source, idxExclusive) {
  let end = idxExclusive;
  while (end > 0 && isSpace(source[end - 1] || "")) end--;
  if (end <= 0) return "";
  if (!isIdentPart(source[end - 1] || "")) return "";
  let start = end - 1;
  while (start > 0 && isIdentPart(source[start - 1] || "")) start--;
  return source.slice(start, end);
}

function isLikelyGroupingParenBefore(source, parenIdx) {
  if ((source[parenIdx] || "") !== "(") return false;
  const beforeWord = wordBefore(source, parenIdx);
  if (beforeWord) return KEYWORDS_EXPECT_EXPR.has(beforeWord);
  const before = parenIdx > 0 ? source[parenIdx - 1] || "" : "";
  if (!before) return true;
  if (isIdentPart(before)) return false;
  if (before === ")" || before === "]") return false;
  return true;
}

function parseMemberAccess(source, startIdx) {
  let j = skipWsAndComments(source, startIdx);
  if (source[j] === "[" || (source[j] === "?" && source[j + 1] === "." && source[j + 2] === "[")) {
    const isOptionalBracket = source[j] === "?";
    const openBracket = j + (isOptionalBracket ? 2 : 0);
    const parsed = parseBracketStringProperty(source, openBracket);
    if (!parsed) return null;
    return { property: parsed.property, endIdxExclusive: parsed.endIdxExclusive };
  }
  if (source[j] === "." || (source[j] === "?" && source[j + 1] === ".")) {
    const isOptionalProp = source[j] === "?";
    j = skipWsAndComments(source, j + (isOptionalProp ? 2 : 1));
    const ident = parseIdentifierWithUnicodeEscapes(source, j);
    if (!ident) return null;
    return { property: ident.value, endIdxExclusive: ident.endIdxExclusive };
  }
  return null;
}

function pushAwaitImportMemberHits(source, masked, hits) {
  const awaitImportRe = /\bawait\s+import\s*\(/gmu;
  for (;;) {
    const m = awaitImportRe.exec(masked);
    if (!m) break;
    const awaitIdx = m.index;
    const openParen = m.index + m[0].lastIndexOf("(");
    if (openParen < 0) continue;

    const specStart = skipWsAndComments(source, openParen + 1);
    const lit = parseStringLiteralOrNoSubstTemplate(source, specStart);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    const afterLit = skipWsAndComments(source, lit.endIdxExclusive);
    if (source[afterLit] !== ")") continue;
    let after = skipWsAndComments(source, afterLit + 1);

    // Common grouping pattern: `(await import("child_process")).exec(...)`.
    // Skip a small number of closing parens that are likely just grouping, not function calls.
    for (let skipped = 0; skipped < 3 && (source[after] || "") === ")"; skipped++) {
      const beforeAwait = awaitIdx;
      let i = beforeAwait;
      while (i > 0 && isSpace(source[i - 1] || "")) i--;
      const openGroup = i > 0 ? i - 1 : -1;
      if (!isLikelyGroupingParenBefore(source, openGroup)) break;
      after = skipWsAndComments(source, after + 1);
    }

    const first = parseMemberAccess(source, after);
    if (!first) continue;
    const afterFirst = skipWsAndComments(source, first.endIdxExclusive);
    const firstIsCall = isOptionalCallStart(source, afterFirst);

    if (first.property === "exec" || first.property === "execSync") {
      const base = `awaitImport(child_process).${first.property}`;
      hits.push({ kind: firstIsCall ? `${base}(` : base, index: awaitIdx });
      continue;
    }

    if (first.property !== "default") continue;
    const second = parseMemberAccess(source, first.endIdxExclusive);
    if (!second) continue;
    if (second.property !== "exec" && second.property !== "execSync") continue;
    const afterSecond = skipWsAndComments(source, second.endIdxExclusive);
    const secondIsCall = isOptionalCallStart(source, afterSecond);
    const base = `awaitImport(child_process).default.${second.property}`;
    hits.push({ kind: secondIsCall ? `${base}(` : base, index: awaitIdx });
  }
}

function scanToMatchingBrace(masked, openIdx) {
  if (masked[openIdx] !== "{") return null;
  let depth = 0;
  for (let i = openIdx; i < masked.length; i++) {
    if (masked[i] === "{") depth++;
    else if (masked[i] === "}") {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  return null;
}

function parseStaticImportDeclaration(source, masked, importIdx) {
  let i = skipWsAndComments(source, importIdx + "import".length);

  // `import type ...` is type-only (TS) and not runtime-relevant.
  if (matchKeyword(source, i, "type")) {
    const afterType = source[i + 4] || "";
    if (isSpace(afterType) || afterType === "{" || afterType === "*") return null;
  }

  // Dynamic import expression: `import("...")`.
  if (source[i] === "(") return null;

  // Side-effect import: `import "x";`
  if (source[i] === "'" || source[i] === '"') return null;

  let defaultId = null;
  let namespaceId = null;
  let namedSpan = null; // [startIdx, endIdxExclusive] in masked/source

  const first = source[i] || "";
  if (first === "*") {
    i = skipWsAndComments(source, i + 1);
    if (!matchKeyword(source, i, "as")) return null;
    i = skipWsAndComments(source, i + 2);
    const ident = parseIdentifierWithUnicodeEscapes(source, i);
    if (!ident) return null;
    namespaceId = ident.value;
    i = ident.endIdxExclusive;
  } else if (first === "{") {
    const end = scanToMatchingBrace(masked, i);
    if (!end) return null;
    namedSpan = [i, end];
    i = end;
  } else {
    const ident = parseIdentifierWithUnicodeEscapes(source, i);
    if (!ident) return null;
    defaultId = ident.value;
    i = skipWsAndComments(source, ident.endIdxExclusive);
    if (source[i] === ",") {
      i = skipWsAndComments(source, i + 1);
      const ch = source[i] || "";
      if (ch === "*") {
        i = skipWsAndComments(source, i + 1);
        if (!matchKeyword(source, i, "as")) return null;
        i = skipWsAndComments(source, i + 2);
        const ns = parseIdentifierWithUnicodeEscapes(source, i);
        if (!ns) return null;
        namespaceId = ns.value;
        i = ns.endIdxExclusive;
      } else if (ch === "{") {
        const end = scanToMatchingBrace(masked, i);
        if (!end) return null;
        namedSpan = [i, end];
        i = end;
      } else {
        return null;
      }
    }
  }

  i = skipWsAndComments(source, i);
  if (!matchKeyword(source, i, "from")) return null;
  i = skipWsAndComments(source, i + 4);
  const lit = parseQuotedStringLiteral(source, i);
  if (!lit) return null;
  if (!isChildProcessSpecifier(lit.value)) return null;

  let importsExec = false;
  let importsExecSync = false;
  if (namedSpan) {
    const inside = masked.slice(namedSpan[0], namedSpan[1]);
    importsExec = /\bexec\b/u.test(inside);
    importsExecSync = /\bexecSync\b/u.test(inside);
  }

  const namespaceIds = [];
  if (defaultId) namespaceIds.push(defaultId);
  if (namespaceId) namespaceIds.push(namespaceId);
  return { importsExec, importsExecSync, namespaceIds };
}

export function findSubprocessSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];
  const childProcessNamespaces = new Set();

  // `shell: true` is unsafe in `spawn`/`spawnSync`-style code paths.
  const shellTrueRe = /\bshell\s*:\s*true\b/gmu;
  for (;;) {
    const m = shellTrueRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "shell: true", index: m.index });
  }

  // Named imports of exec/execSync from child_process are forbidden.
  const importRe = /\bimport\b/gmu;
  for (;;) {
    const m = importRe.exec(masked);
    if (!m) break;
    const idx = m.index;

    const parsed = parseStaticImportDeclaration(source, masked, idx);
    if (!parsed) continue;
    const { importsExec, importsExecSync, namespaceIds } = parsed;
    if (importsExec) hits.push({ kind: "import{exec} child_process", index: idx });
    if (importsExecSync) hits.push({ kind: "import{execSync} child_process", index: idx });
    for (const id of namespaceIds) childProcessNamespaces.add(id);
  }

  // CommonJS: track aliases assigned to require("child_process").
  const requireAssignRe = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*require\b/gmu;
  for (;;) {
    const m = requireAssignRe.exec(masked);
    if (!m) break;
    const id = m[1];
    const requireIdx = m.index + m[0].length - "require".length;

    let j = skipWsAndComments(source, requireIdx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWsAndComments(source, j + 1);
    const lit = parseStringLiteralOrNoSubstTemplate(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;
    childProcessNamespaces.add(id);
  }

  // ESM dynamic import: track aliases assigned to (await) import("child_process").
  const importAssignRe = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:await\s+)?import\s*\(/gmu;
  for (;;) {
    const m = importAssignRe.exec(masked);
    if (!m) break;
    const id = m[1];
    const openParen = m.index + m[0].lastIndexOf("(");
    if (openParen < 0) continue;

    const specStart = skipWsAndComments(source, openParen + 1);
    const lit = parseStringLiteralOrNoSubstTemplate(source, specStart);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;
    childProcessNamespaces.add(id);
  }

  // CommonJS destructuring: forbid pulling exec/execSync out of child_process.
  const requireDestructureRe = /\b(?:const|let|var)\s*\{([^}]*)\}\s*=\s*require\b/gmu;
  for (;;) {
    const m = requireDestructureRe.exec(masked);
    if (!m) break;
    const destructure = m[1] || "";
    const hasExec = /\bexec\b/u.test(destructure);
    const hasExecSync = /\bexecSync\b/u.test(destructure);
    if (!hasExec && !hasExecSync) continue;

    const requireIdx = m.index + m[0].length - "require".length;
    let j = skipWsAndComments(source, requireIdx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWsAndComments(source, j + 1);
    const lit = parseStringLiteralOrNoSubstTemplate(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    if (hasExec) hits.push({ kind: "destructure exec child_process", index: m.index });
    if (hasExecSync) hits.push({ kind: "destructure execSync child_process", index: m.index });
  }

  // ESM dynamic import destructuring: forbid pulling exec/execSync out of import("child_process").
  const importDestructureRe = /\b(?:const|let|var)\s*\{([^}]*)\}\s*=\s*(?:await\s+)?import\s*\(/gmu;
  for (;;) {
    const m = importDestructureRe.exec(masked);
    if (!m) break;
    const destructure = m[1] || "";
    const hasExec = /\bexec\b/u.test(destructure);
    const hasExecSync = /\bexecSync\b/u.test(destructure);
    if (!hasExec && !hasExecSync) continue;

    const openParen = m.index + m[0].lastIndexOf("(");
    if (openParen < 0) continue;
    const specStart = skipWsAndComments(source, openParen + 1);
    const lit = parseStringLiteralOrNoSubstTemplate(source, specStart);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;
    const afterLit = skipWsAndComments(source, lit.endIdxExclusive);
    if (source[afterLit] !== ")") continue; // keep parsing simple/strict

    if (hasExec) hits.push({ kind: "destructure exec child_process", index: m.index });
    if (hasExecSync) hits.push({ kind: "destructure execSync child_process", index: m.index });
  }

  // Destructuring from a known child_process namespace/default/alias (e.g. `const { exec } = cp;`).
  const nsDestructureRe = /\b(?:const|let|var)\s*\{([^}]*)\}\s*=\s*([A-Za-z_$][\w$]*)\b/gmu;
  for (;;) {
    const m = nsDestructureRe.exec(masked);
    if (!m) break;
    const destructure = m[1] || "";
    const rhs = m[2] || "";
    if (!childProcessNamespaces.has(rhs)) continue;

    const hasExec = /\bexec\b/u.test(destructure);
    const hasExecSync = /\bexecSync\b/u.test(destructure);
    if (!hasExec && !hasExecSync) continue;

    if (hasExec) hits.push({ kind: "destructure exec child_processNamespace", index: m.index });
    if (hasExecSync) hits.push({ kind: "destructure execSync child_processNamespace", index: m.index });
  }

  // Direct awaited dynamic import member access: `(await import("child_process")).exec(...)`.
  pushAwaitImportMemberHits(source, masked, hits);

  // Catch exec/execSync usage via a namespace/default alias like `cp.exec(`.
  for (const ns of childProcessNamespaces) {
    for (const prop of ["exec", "execSync"]) {
      const propRe = new RegExp(`\\b${escapeRegex(ns)}\\s*(?:\\.|\\?\\.)\\s*${prop}\\b`, "gmu");
      for (;;) {
        const m = propRe.exec(masked);
        if (!m) break;
        const afterProp = m.index + m[0].length;
        const j = skipWsAndComments(source, afterProp);
        const isCall = isOptionalCallStart(source, j);
        if (prop === "exec") {
          hits.push({ kind: isCall ? "child_processNamespace.exec(" : "child_processNamespace.exec", index: m.index });
        } else {
          hits.push({ kind: isCall ? "child_processNamespace.execSync(" : "child_processNamespace.execSync", index: m.index });
        }
      }
    }

    if (masked.includes("\\u")) {
      const dotRe = new RegExp(`\\b${escapeRegex(ns)}\\s*(?:\\.|\\?\\.)`, "gmu");
      for (;;) {
        const m = dotRe.exec(masked);
        if (!m) break;
        let j = skipWsAndComments(source, m.index + m[0].length);
        const ident = parseIdentifierWithUnicodeEscapes(source, j);
        if (!ident) continue;
        const raw = source.slice(j, ident.endIdxExclusive);
        if (!raw.includes("\\u")) continue;
        if (ident.value !== "exec" && ident.value !== "execSync") continue;
        j = skipWsAndComments(source, ident.endIdxExclusive);
        const isCall = isOptionalCallStart(source, j);
        const kind = ident.value === "exec" ? "child_processNamespace.exec" : "child_processNamespace.execSync";
        hits.push({ kind: isCall ? `${kind}(` : kind, index: m.index });
      }
    }

    const bracketRe = new RegExp(`\\b${escapeRegex(ns)}\\s*(?:\\?\\.)?\\s*\\[`, "gmu");
    for (;;) {
      const m = bracketRe.exec(masked);
      if (!m) break;
      const openBracket = m.index + m[0].length - 1;
      const parsed = parseBracketStringProperty(source, openBracket);
      if (!parsed) continue;
      if (parsed.property !== "exec" && parsed.property !== "execSync") continue;
      let j = skipWsAndComments(source, parsed.endIdxExclusive);
      const isCall = isOptionalCallStart(source, j);
      if (parsed.property === "exec") {
        hits.push({ kind: isCall ? "child_processNamespace.exec(" : "child_processNamespace.exec", index: m.index });
      } else {
        hits.push({ kind: isCall ? "child_processNamespace.execSync(" : "child_processNamespace.execSync", index: m.index });
      }
    }
  }

  // require("child_process").exec(/execSync)(...) is forbidden.
  const requireRe = /\brequire\b/gmu;
  for (;;) {
    const m = requireRe.exec(masked);
    if (!m) break;
    const idx = m.index;

    let j = skipWsAndComments(source, idx + "require".length);
    if (source[j] !== "(") continue;
    j = skipWsAndComments(source, j + 1);

    const lit = parseStringLiteralOrNoSubstTemplate(source, j);
    if (!lit) continue;
    if (!isChildProcessSpecifier(lit.value)) continue;

    j = skipWsAndComments(source, lit.endIdxExclusive);
    if (source[j] !== ")") continue; // keep parsing simple/strict
    const afterClose = j + 1;

    const member = parseMemberAccess(source, afterClose);
    if (member) {
      const j2 = skipWsAndComments(source, member.endIdxExclusive);
      const isCall = isOptionalCallStart(source, j2);
      if (member.property === "execSync") hits.push({ kind: isCall ? "require(child_process).execSync(" : "require(child_process).execSync", index: idx });
      if (member.property === "exec") hits.push({ kind: isCall ? "require(child_process).exec(" : "require(child_process).exec", index: idx });
      continue;
    }
  }

  return hits;
}

