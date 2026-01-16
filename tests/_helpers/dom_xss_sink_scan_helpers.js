import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import {
  isIdentContinue,
  isSpace,
  parseBracketStringProperty,
  parseIdentifierWithUnicodeEscapes,
  skipWsAndComments,
} from "./js_scan_parse_helpers.js";

function findRegexHits(masked, re, kind) {
  const hits = [];
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    hits.push({ kind, index: m.index });
  }
  return hits;
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
  "instanceof",
  "void",
  "delete",
  "new",
  "in",
  "of",
]);

const CONTROL_FLOW_PAREN_KEYWORDS = new Set([
  // After `if (...)`, `while (...)`, etc, `[` can begin an expression statement (array literal),
  // not a computed property access.
  "if",
  "for",
  "while",
  "switch",
  "catch",
  "with",
]);

const EXPR_START_PUNCT = new Set([
  "(",
  "[",
  "{",
  ",",
  ":",
  ";",
  "=",
  "?",
  "!",
  "~",
  "+",
  "-",
  "*",
  "%",
  "&",
  "|",
  "^",
  "<",
  ">",
  "/",
]);

function prevNonWs(masked, idx) {
  let i = idx;
  while (i > 0 && isSpace(masked[i - 1] || "")) i--;
  return i - 1;
}

function wordBefore(masked, idxExclusive) {
  let end = idxExclusive;
  while (end > 0 && isSpace(masked[end - 1] || "")) end--;
  if (end <= 0) return "";
  if (!isIdentPart(masked[end - 1] || "")) return "";
  let start = end - 1;
  while (start > 0 && isIdentPart(masked[start - 1] || "")) start--;
  return masked.slice(start, end);
}

function isControlFlowParenBefore(masked, closeParenIdx) {
  if ((masked[closeParenIdx] || "") !== ")") return false;
  let depth = 0;
  for (let i = closeParenIdx; i >= 0; i--) {
    const ch = masked[i] || "";
    if (ch === ")") {
      depth++;
      continue;
    }
    if (ch === "(") {
      depth--;
      if (depth === 0) {
        const word = wordBefore(masked, i);
        return CONTROL_FLOW_PAREN_KEYWORDS.has(word);
      }
      continue;
    }
  }
  return false;
}

function likelyComputedPropertyAccess(masked, openBracketIdx) {
  const prevIdx = prevNonWs(masked, openBracketIdx);
  if (prevIdx < 0) return false;
  const prev = masked[prevIdx] || "";

  // If we're in an expression-start position, `[` is likely an array literal.
  if (EXPR_START_PUNCT.has(prev)) return false;

  // Control-flow headers like `if (x)` can be followed by an array literal statement.
  if (prev === ")" && isControlFlowParenBefore(masked, prevIdx)) return false;

  // If the previous token is a keyword that expects an expression, `[` is likely an array literal.
  if (isIdentPart(prev)) {
    let start = prevIdx;
    while (start > 0 && isIdentPart(masked[start - 1] || "")) start--;
    const word = masked.slice(start, prevIdx + 1);
    if (KEYWORDS_EXPECT_EXPR.has(word)) return false;
  }

  return true;
}

function findBracketPropertyHits(source, masked, propName, kind) {
  const hits = [];
  let i = 0;
  for (;;) {
    const idx = masked.indexOf("[", i);
    if (idx < 0) break;
    i = idx + 1;
    if (!likelyComputedPropertyAccess(masked, idx)) continue;

    const parsed = parseBracketStringProperty(source, idx);
    if (!parsed) continue;
    if (parsed.property !== propName) continue;

    hits.push({ kind, index: idx });
  }
  return hits;
}

function findDocumentBracketWriteHits(source, masked) {
  const hits = [];
  const re = /\bdocument\s*(?:\.|\?\.)\s*\[/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const openBracket = masked.indexOf("[", m.index);
    if (openBracket < 0) continue;

    const parsed = parseBracketStringProperty(source, openBracket);
    if (!parsed) continue;
    if (parsed.property !== "write" && parsed.property !== "writeln") continue;

    hits.push({ kind: "document.write", index: m.index });
  }
  return hits;
}

function findDotPropertyHitsWithUnicodeEscapes(source, masked, propName, kind) {
  if (!masked.includes("\\u")) return [];
  const hits = [];
  const re = /(?:\.|\?\.)/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const start = skipWsAndComments(source, m.index + m[0].length);
    const ident = parseIdentifierWithUnicodeEscapes(source, start);
    if (!ident) continue;
    const raw = source.slice(start, ident.endIdxExclusive);
    if (!raw.includes("\\u")) continue;
    if (ident.value !== propName) continue;
    hits.push({ kind, index: m.index });
  }
  return hits;
}

function findDocumentDotWriteHitsWithUnicodeEscapes(source, masked) {
  if (!masked.includes("\\u")) return [];
  const hits = [];
  const re = /\bdocument\s*(?:\.|\?\.)/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const start = skipWsAndComments(source, m.index + m[0].length);
    const ident = parseIdentifierWithUnicodeEscapes(source, start);
    if (!ident) continue;
    const raw = source.slice(start, ident.endIdxExclusive);
    if (!raw.includes("\\u")) continue;
    if (ident.value !== "write" && ident.value !== "writeln") continue;
    hits.push({ kind: "document.write", index: m.index });
  }
  return hits;
}

export function findDomXssSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // React sink.
  hits.push(...findRegexHits(masked, /\bdangerouslySetInnerHTML\b/gmu, "dangerouslySetInnerHTML"));

  // DOM sinks (dot access).
  hits.push(...findRegexHits(masked, /(?:\.|\?\.)\s*innerHTML\b/gmu, ".innerHTML"));
  hits.push(...findRegexHits(masked, /(?:\.|\?\.)\s*outerHTML\b/gmu, ".outerHTML"));
  hits.push(...findRegexHits(masked, /(?:\.|\?\.)\s*insertAdjacentHTML\b/gmu, ".insertAdjacentHTML"));
  hits.push(...findRegexHits(masked, /\bdocument\s*(?:\.|\?\.)\s*writeln?\b/gmu, "document.write"));
  hits.push(...findRegexHits(masked, /(?:\.|\?\.)\s*createContextualFragment\b/gmu, ".createContextualFragment"));
  hits.push(...findDotPropertyHitsWithUnicodeEscapes(source, masked, "innerHTML", ".innerHTML"));
  hits.push(...findDotPropertyHitsWithUnicodeEscapes(source, masked, "outerHTML", ".outerHTML"));
  hits.push(...findDotPropertyHitsWithUnicodeEscapes(source, masked, "insertAdjacentHTML", ".insertAdjacentHTML"));
  hits.push(...findDotPropertyHitsWithUnicodeEscapes(source, masked, "createContextualFragment", ".createContextualFragment"));
  hits.push(...findDocumentDotWriteHitsWithUnicodeEscapes(source, masked));
  hits.push(...findDocumentBracketWriteHits(source, masked));

  // DOM sinks (bracket access): `el["innerHTML"] = ...`.
  hits.push(...findBracketPropertyHits(source, masked, "innerHTML", '["innerHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "outerHTML", '["outerHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "insertAdjacentHTML", '["insertAdjacentHTML"]'));
  hits.push(...findBracketPropertyHits(source, masked, "createContextualFragment", '["createContextualFragment"]'));
  hits.push(...findBracketPropertyHits(source, masked, "dangerouslySetInnerHTML", '["dangerouslySetInnerHTML"]'));

  return hits;
}

