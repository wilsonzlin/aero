import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import { parseQuotedStringLiteral, skipWsAndComments } from "./js_scan_parse_helpers.js";

function findGlobalBracketCall(masked, source, propName, kind) {
  const hits = [];
  const re = /\b(globalThis|window|self)\s*\[/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    const openBracket = masked.indexOf("[", m.index);
    if (openBracket < 0) continue;

    const keyStart = skipWsAndComments(source, openBracket + 1);
    const lit = parseQuotedStringLiteral(source, keyStart);
    if (!lit) continue;
    if (lit.value !== propName) continue;

    let j = skipWsAndComments(source, lit.endIdxExclusive);
    if ((source[j] || "") !== "]") continue;
    j = skipWsAndComments(source, j + 1);
    if ((source[j] || "") !== "(") continue;

    hits.push({ kind, index: m.index, openParenIndex: j });
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

  // Explicit global eval references (these are still eval sinks).
  const globalEvalRe = /\b(globalThis|window|self)\s*\.\s*eval\s*\(/gmu;
  for (;;) {
    const m = globalEvalRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "globalEval", index: m.index });
  }

  // Bracket-notation global eval (e.g. globalThis["eval"]()).
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

  // Timer-string eval sinks: setTimeout("...") / setInterval("...") (including `window.*`/`globalThis.*`).
  //
  // We search in the masked source so we don't match inside strings/comments/regex, then we parse
  // the original source to see if the first argument is a literal string/template.
  const timerDirectRe = /(^|[^.\w$])(setTimeout|setInterval)\s*\(/gmu;
  for (;;) {
    const m = timerDirectRe.exec(masked);
    if (!m) break;
    const callIdx = m.index + (m[1] ? m[1].length : 0);
    const afterParen = m.index + m[0].length;
    const arg0 = skipWsAndComments(source, afterParen);
    const ch = source[arg0] || "";
    if (ch === "'" || ch === '"' || ch === "`") hits.push({ kind: `${m[2]}String`, index: callIdx });
  }

  const timerGlobalRe = /\b(globalThis|window|self)\s*\.\s*(setTimeout|setInterval)\s*\(/gmu;
  for (;;) {
    const m = timerGlobalRe.exec(masked);
    if (!m) break;
    const callIdx = m.index;
    const afterParen = m.index + m[0].length;
    const arg0 = skipWsAndComments(source, afterParen);
    const ch = source[arg0] || "";
    if (ch === "'" || ch === '"' || ch === "`") hits.push({ kind: `${m[2]}String`, index: callIdx });
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

