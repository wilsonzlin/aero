import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import {
  isOptionalCallStart,
  isIdentContinue,
  isSpace,
  parseBracketStringProperty,
  parseIdentifierWithUnicodeEscapes,
  skipWsAndComments,
} from "./js_scan_parse_helpers.js";

function prevNonWs(masked, idx) {
  let i = idx;
  while (i > 0 && isSpace(masked[i - 1] || "")) i--;
  return i - 1;
}

function findEscapedDirectIdentifierCalls(masked, source, name, kind, { requireStringArg0 } = {}) {
  if (!masked.includes("\\u")) return [];
  const hits = [];
  const seenStarts = new Set();
  const re = /\\u/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const escIdx = m.index;
    let start = escIdx;
    while (start > 0 && isIdentContinue(masked[start - 1] || "")) start--;
    if (seenStarts.has(start)) continue;
    seenStarts.add(start);

    const parsed = parseIdentifierWithUnicodeEscapes(source, start);
    if (!parsed) continue;
    if (parsed.value !== name) continue;
    const raw = source.slice(start, parsed.endIdxExclusive);
    if (!raw.includes("\\u")) continue;

    const prev = prevNonWs(masked, start);
    if (prev >= 0) {
      const pch = masked[prev] || "";
      if (pch === "." || isIdentContinue(pch)) continue;
    }

    let j = skipWsAndComments(source, parsed.endIdxExclusive);
    if (!isOptionalCallStart(source, j)) continue;
    const openParenIndex = (source[j] || "") === "(" ? j : j + 2;

    if (requireStringArg0) {
      const arg0 = skipWsAndComments(source, openParenIndex + 1);
      const ch = source[arg0] || "";
      if (ch !== "'" && ch !== '"' && ch !== "`") continue;
    }

    hits.push({ kind, index: start, openParenIndex });
  }
  return hits;
}

function findDirectIdentifierStringArgCall(masked, source, name, kind) {
  const hits = [];
  const re = new RegExp(`\\b${name}\\b`, "gmu");
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const start = m.index;
    const prev = prevNonWs(masked, start);
    if (prev >= 0) {
      const pch = masked[prev] || "";
      if (pch === "." || isIdentContinue(pch)) continue;
    }

    let j = skipWsAndComments(source, start + name.length);
    if (!isOptionalCallStart(source, j)) continue;
    const openParenIndex = (source[j] || "") === "(" ? j : j + 2;
    const arg0 = skipWsAndComments(source, openParenIndex + 1);
    const ch = source[arg0] || "";
    if (ch !== "'" && ch !== '"' && ch !== "`") continue;
    hits.push({ kind, index: start });
  }
  return hits;
}

function findGlobalDotProperty(masked, source, propName, kind) {
  const hits = [];
  const re = /\b(globalThis|window|self)\s*(?:\.|\?\.)/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const afterDot = m.index + m[0].length;
    const start = skipWsAndComments(source, afterDot);
    const ident = parseIdentifierWithUnicodeEscapes(source, start);
    if (!ident) continue;
    if (ident.value !== propName) continue;
    hits.push({ kind, index: m.index, endIdxExclusive: ident.endIdxExclusive });
  }
  return hits;
}

function findGlobalDotStringArgCall(masked, source, propName, kind) {
  const hits = [];
  const re = /\b(globalThis|window|self)\s*(?:\.|\?\.)/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const afterDot = m.index + m[0].length;
    const start = skipWsAndComments(source, afterDot);
    const ident = parseIdentifierWithUnicodeEscapes(source, start);
    if (!ident) continue;
    if (ident.value !== propName) continue;
    let j = skipWsAndComments(source, ident.endIdxExclusive);
    if (!isOptionalCallStart(source, j)) continue;
    const openParenIndex = (source[j] || "") === "(" ? j : j + 2;
    const arg0 = skipWsAndComments(source, openParenIndex + 1);
    const ch = source[arg0] || "";
    if (ch !== "'" && ch !== '"' && ch !== "`") continue;
    hits.push({ kind, index: m.index });
  }
  return hits;
}

function findGlobalBracketProperty(masked, source, propName, kind) {
  const hits = [];
  const re = /\b(globalThis|window|self)\s*(?:\?\.)?\s*\[/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const openBracket = masked.indexOf("[", m.index);
    if (openBracket < 0) continue;

    const parsed = parseBracketStringProperty(source, openBracket);
    if (!parsed) continue;
    if (parsed.property !== propName) continue;

    hits.push({ kind, index: m.index, endIdxExclusive: parsed.endIdxExclusive });
  }
  return hits;
}

function findGlobalBracketCall(masked, source, propName, kind) {
  const hits = [];
  const re = /\b(globalThis|window|self)\s*(?:\?\.)?\s*\[/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const openBracket = masked.indexOf("[", m.index);
    if (openBracket < 0) continue;

    const parsed = parseBracketStringProperty(source, openBracket);
    if (!parsed) continue;
    if (parsed.property !== propName) continue;

    let j = skipWsAndComments(source, parsed.endIdxExclusive);
    if (!isOptionalCallStart(source, j)) continue;
    const openParenIndex = (source[j] || "") === "(" ? j : j + 2;

    hits.push({ kind, index: m.index, openParenIndex });
  }
  return hits;
}

function findGlobalDotCallWithUnicodeEscapes(masked, source, propName, kind) {
  if (!masked.includes("\\u")) return [];
  const hits = [];
  const re = /\b(globalThis|window|self)\s*(?:\.|\?\.)/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const afterDot = m.index + m[0].length;
    const start = skipWsAndComments(source, afterDot);
    const ident = parseIdentifierWithUnicodeEscapes(source, start);
    if (!ident) continue;
    const raw = source.slice(start, ident.endIdxExclusive);
    if (!raw.includes("\\u")) continue;
    if (ident.value !== propName) continue;
    let j = skipWsAndComments(source, ident.endIdxExclusive);
    if (!isOptionalCallStart(source, j)) continue;
    hits.push({ kind, index: m.index, openParenIndex: (source[j] || "") === "(" ? j : j + 2 });
  }
  return hits;
}

export function findEvalSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // Direct eval() call (not obj.eval()).
  const directEvalRe = /(^|[^.\w$])eval\s*\(/gmu;
  for (;;) {
    const m = directEvalRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "eval", index: m.index });
  }

  // Direct eval() via unicode-escaped identifier: `ev\u0061l(...)`.
  hits.push(...findEscapedDirectIdentifierCalls(masked, source, "eval", "eval"));

  // Explicit global eval references (these are still eval sinks).
  const globalEvalRe = /\b(globalThis|window|self)\s*(?:\.|\?\.)\s*eval\s*\(/gmu;
  for (;;) {
    const m = globalEvalRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "globalEval", index: m.index });
  }

  // Global eval reference-taking (including unicode-escaped identifiers).
  hits.push(...findGlobalDotProperty(masked, source, "eval", "globalEval").map(({ kind, index }) => ({ kind, index })));

  // Escaped global eval references: `globalThis.e\u0076al(...)`.
  hits.push(...findGlobalDotCallWithUnicodeEscapes(masked, source, "eval", "globalEval").map(({ kind, index }) => ({ kind, index })));

  // Bracket-notation global eval reference-taking (including escapes/no-subst templates).
  hits.push(...findGlobalBracketProperty(masked, source, "eval", "globalEvalBracket").map(({ kind, index }) => ({ kind, index })));

  // Bracket-notation global eval calls (e.g. globalThis["eval"]()).
  hits.push(...findGlobalBracketCall(masked, source, "eval", "globalEvalBracket").map(({ kind, index }) => ({ kind, index })));

  // Function constructor.
  const newFunctionRe = /\bnew\s+Function\s*\(/gmu;
  for (;;) {
    const m = newFunctionRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "newFunction", index: m.index });
  }

  // Function() call is also an eval sink (same as new Function()).
  // Avoid double-counting `new Function(` as both.
  const functionCallRe = /\bFunction\s*\(/gmu;
  for (;;) {
    const m = functionCallRe.exec(masked);
    if (!m) break;
    if (/\bnew\s+Function\s*\(/gmu.test(masked.slice(Math.max(0, m.index - 16), m.index + 16))) continue;
    hits.push({ kind: "Function", index: m.index });
  }

  // Direct Function() via unicode-escaped identifier: `Funct\u0069on(...)`.
  hits.push(...findEscapedDirectIdentifierCalls(masked, source, "Function", "Function"));

  // Global Function reference-taking via dot/bracket (including unicode escapes).
  hits.push(...findGlobalDotProperty(masked, source, "Function", "Function").map(({ kind, index }) => ({ kind, index })));
  hits.push(...findGlobalBracketProperty(masked, source, "Function", "Function").map(({ kind, index }) => ({ kind, index })));

  // Timer-string eval sinks: setTimeout("...") / setInterval("...") (including `window.*`/`globalThis.*`).
  //
  // We search in the masked source so we don't match inside strings/comments/regex, then we parse
  // the original source to see if the first argument is a literal string/template.
  hits.push(...findDirectIdentifierStringArgCall(masked, source, "setTimeout", "setTimeoutString"));
  hits.push(...findDirectIdentifierStringArgCall(masked, source, "setInterval", "setIntervalString"));

  // Timer-string eval via unicode-escaped identifier: `setTime\u006fut("...")`.
  hits.push(...findEscapedDirectIdentifierCalls(masked, source, "setTimeout", "setTimeoutString", { requireStringArg0: true }).map(({ kind, index }) => ({ kind, index })));
  hits.push(...findEscapedDirectIdentifierCalls(masked, source, "setInterval", "setIntervalString", { requireStringArg0: true }).map(({ kind, index }) => ({ kind, index })));

  hits.push(...findGlobalDotStringArgCall(masked, source, "setTimeout", "setTimeoutString"));
  hits.push(...findGlobalDotStringArgCall(masked, source, "setInterval", "setIntervalString"));

  // Escaped global timers: `window.setTime\u006fut("...")`.
  for (const which of ["setTimeout", "setInterval"]) {
    const dotCalls = findGlobalDotCallWithUnicodeEscapes(masked, source, which, `${which}DotEscaped`);
    for (const hit of dotCalls) {
      const arg0 = skipWsAndComments(source, hit.openParenIndex + 1);
      const ch = source[arg0] || "";
      if (ch === "'" || ch === '"' || ch === "`") hits.push({ kind: `${which}String`, index: hit.index });
    }
  }

  // Bracket-notation global timers with string first arg:
  // globalThis["setTimeout"]("...") / window["setInterval"](`...`)
  for (const which of ["setTimeout", "setInterval"]) {
    const bracketCalls = findGlobalBracketCall(masked, source, which, `${which}Bracket`);
    for (const hit of bracketCalls) {
      const arg0 = skipWsAndComments(source, hit.openParenIndex + 1);
      const ch = source[arg0] || "";
      if (ch === "'" || ch === '"' || ch === "`") hits.push({ kind: `${which}String`, index: hit.index });
    }
  }

  return hits;
}

