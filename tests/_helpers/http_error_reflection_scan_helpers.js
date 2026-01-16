import { stripStringsAndComments } from "./js_source_scan_helpers.js";
import { parseBracketStringProperty, parseIdentifierWithUnicodeEscapes, skipWsAndComments } from "./js_scan_parse_helpers.js";

function escapeRegex(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function scanToMatchingParen(masked, openIdx) {
  if (masked[openIdx] !== "(") return null;
  let depth = 0;
  for (let i = openIdx; i < masked.length; i++) {
    const ch = masked[i];
    if (ch === "(") depth++;
    else if (ch === ")") {
      depth--;
      if (depth === 0) return i;
    }
  }
  return null;
}

function parseDirectErrArg(source, openParenIdx) {
  let i = skipWsAndComments(source, openParenIdx + 1);

  // Allow harmless grouping: `end((err))`, `send(((error)))`.
  let depth = 0;
  while ((source[i] || "") === "(") {
    depth++;
    i = skipWsAndComments(source, i + 1);
  }

  const ident = parseIdentifierWithUnicodeEscapes(source, i);
  if (!ident) return null;
  if (ident.value !== "err" && ident.value !== "error") return null;
  i = skipWsAndComments(source, ident.endIdxExclusive);

  for (let k = 0; k < depth; k++) {
    if ((source[i] || "") !== ")") return null;
    i = skipWsAndComments(source, i + 1);
  }

  const next = source[i] || "";
  if (next !== "," && next !== ")") return null;
  return ident.value;
}

function startsWithOptionalChain(source, idx) {
  return (source[idx] || "") === "?" && (source[idx + 1] || "") === ".";
}

function isAsciiIdentContinue(ch) {
  return (
    (ch >= "A" && ch <= "Z") ||
    (ch >= "a" && ch <= "z") ||
    (ch >= "0" && ch <= "9") ||
    ch === "_" ||
    ch === "$"
  );
}

function hasErrMessageBracketAccess(source, masked, startIdx, endIdxExclusive) {
  // Strings/comments are masked to spaces in `masked`, so any bracket-string property
  // checks must parse against `source` and validate the property value.
  const identRe = /\b(?:err|error)\b/gmu;
  identRe.lastIndex = startIdx;
  for (;;) {
    const m = identRe.exec(masked);
    if (!m) break;
    const idx = m.index;
    if (idx >= endIdxExclusive) break;

    let i = skipWsAndComments(source, idx + m[0].length);
    if ((source[i] || "") === ")" || (source[i] || "") === "]") continue;
    if (startsWithOptionalChain(source, i)) i = skipWsAndComments(source, i + 2);
    if ((source[i] || "") !== "[") continue;
    const prop = parseBracketStringProperty(source, i);
    if (!prop) continue;
    if (prop.property === "message") return true;
  }

  // Parenthesized form: `(err)["message"]` / `((error))?.["message"]`.
  const parenRe = /\(/gmu;
  parenRe.lastIndex = startIdx;
  for (;;) {
    const m = parenRe.exec(masked);
    if (!m) break;
    const openIdx = m.index;
    if (openIdx >= endIdxExclusive) break;

    const prev = openIdx > 0 ? (masked[openIdx - 1] || "") : "";
    if (isAsciiIdentContinue(prev) || prev === ")") continue;

    let i = openIdx;
    let depth = 0;
    while ((source[i] || "") === "(") {
      depth++;
      i = skipWsAndComments(source, i + 1);
    }
    const ident = parseIdentifierWithUnicodeEscapes(source, i);
    if (!ident) continue;
    if (ident.value !== "err" && ident.value !== "error") continue;

    i = skipWsAndComments(source, ident.endIdxExclusive);
    let ok = true;
    for (let k = 0; k < depth; k++) {
      if ((source[i] || "") !== ")") {
        ok = false;
        break;
      }
      i = skipWsAndComments(source, i + 1);
    }
    if (!ok) continue;

    if (startsWithOptionalChain(source, i)) i = skipWsAndComments(source, i + 2);
    if ((source[i] || "") !== "[") continue;
    const prop = parseBracketStringProperty(source, i);
    if (!prop) continue;
    if (prop.property === "message") return true;
  }

  return false;
}

function hasErrMessageDotAccessWithUnicodeEscapes(source, masked, startIdx, endIdxExclusive) {
  const identRe = /\b(?:err|error)\b/gmu;
  identRe.lastIndex = startIdx;
  for (;;) {
    const m = identRe.exec(masked);
    if (!m) break;
    const idx = m.index;
    if (idx >= endIdxExclusive) break;

    let i = skipWsAndComments(source, idx + m[0].length);
    if (startsWithOptionalChain(source, i)) {
      // `err?.prop` form: optional chain includes the dot.
      i = skipWsAndComments(source, i + 2);
    } else {
      // `err.prop` form.
      if ((source[i] || "") !== ".") continue;
      i = skipWsAndComments(source, i + 1);
    }

    const propStart = i;
    const ident = parseIdentifierWithUnicodeEscapes(source, i);
    if (!ident) continue;
    if (ident.value !== "message") continue;
    if (!source.slice(propStart, ident.endIdxExclusive).includes("\\")) continue;
    if (ident.endIdxExclusive > endIdxExclusive) continue;
    return true;
  }

  // Parenthesized error identifier: `(err).m\u0065ssage`, `((error))?.m\u0065ssage`.
  const parenRe = /\(/gmu;
  parenRe.lastIndex = startIdx;
  for (;;) {
    const m = parenRe.exec(masked);
    if (!m) break;
    const openIdx = m.index;
    if (openIdx >= endIdxExclusive) break;

    const prev = openIdx > 0 ? (masked[openIdx - 1] || "") : "";
    if (isAsciiIdentContinue(prev) || prev === ")") continue;

    let i = openIdx;
    let depth = 0;
    while ((source[i] || "") === "(") {
      depth++;
      i = skipWsAndComments(source, i + 1);
    }
    const ident = parseIdentifierWithUnicodeEscapes(source, i);
    if (!ident) continue;
    if (ident.value !== "err" && ident.value !== "error") continue;

    i = skipWsAndComments(source, ident.endIdxExclusive);
    let ok = true;
    for (let k = 0; k < depth; k++) {
      if ((source[i] || "") !== ")") {
        ok = false;
        break;
      }
      i = skipWsAndComments(source, i + 1);
    }
    if (!ok) continue;

    if (startsWithOptionalChain(source, i)) {
      i = skipWsAndComments(source, i + 2);
    } else {
      if ((source[i] || "") !== ".") continue;
      i = skipWsAndComments(source, i + 1);
    }

    const propStart = i;
    const prop = parseIdentifierWithUnicodeEscapes(source, i);
    if (!prop) continue;
    if (prop.value !== "message") continue;
    if (!source.slice(propStart, prop.endIdxExclusive).includes("\\")) continue;
    if (prop.endIdxExclusive > endIdxExclusive) continue;
    return true;
  }

  return false;
}

function scanChainedReceiverCalls(source, masked, receivers, terminalMethods, patternsByMethod) {
  const hits = [];
  const receiverRe = new RegExp(`\\b(${[...receivers].map(escapeRegex).join("|")})\\b`, "gmu");
  for (;;) {
    const m = receiverRe.exec(masked);
    if (!m) break;
    const recv = m[1] || "";
    if (!recv) continue;

    let i = m.index + recv.length;
    for (let steps = 0; steps < 50; steps++) {
      i = skipWsAndComments(source, i);
      const hasOptChain = startsWithOptionalChain(source, i);
      if (hasOptChain) {
        i = skipWsAndComments(source, i + 2);
      } else if ((source[i] || "") === ".") {
        i = skipWsAndComments(source, i + 1);
      } else if ((source[i] || "") === "[") {
        // bracket property without a leading dot is valid: res["send"](...)
        // (and optional-chaining bracket properties are handled via the `?.` branch above)
      } else {
        break;
      }

      let name = "";
      if ((source[i] || "") === "[") {
        const prop = parseBracketStringProperty(source, i);
        if (!prop) break;
        name = prop.property;
        i = prop.endIdxExclusive;
      } else {
        const ident = parseIdentifierWithUnicodeEscapes(source, i);
        if (!ident) break;
        name = ident.value;
        i = ident.endIdxExclusive;
      }

      let j = skipWsAndComments(source, i);
      const isOptCall = (source[j] || "") === "?" && (source[j + 1] || "") === "." && (source[j + 2] || "") === "(";
      const isCall = (source[j] || "") === "(" || isOptCall;

      if (terminalMethods.has(name) && isCall) {
        const openParen = isOptCall ? j + 2 : j;
        const closeParen = scanToMatchingParen(masked, openParen);
        if (closeParen === null) break;
        const argsMasked = masked.slice(openParen + 1, closeParen);
        const patterns = patternsByMethod.get(name) ?? [];
        for (const { re, kind } of patterns) {
          if (re.test(argsMasked)) hits.push({ kind, index: m.index });
        }
        if (hasErrMessageBracketAccess(source, masked, openParen + 1, closeParen)) {
          hits.push({ kind: `http ${name} err.message`, index: m.index });
        }
        if (hasErrMessageDotAccessWithUnicodeEscapes(source, masked, openParen + 1, closeParen)) {
          hits.push({ kind: `http ${name} err.message`, index: m.index });
        }
        const direct = parseDirectErrArg(source, openParen);
        if (direct) hits.push({ kind: "direct err arg", index: m.index });
        break;
      }

      // Skip call arguments for fluent helpers (e.g. reply.code(500).send(...)).
      if (isCall) {
        const openParen = isOptCall ? j + 2 : j;
        const closeParen = scanToMatchingParen(masked, openParen);
        if (closeParen === null) break;
        i = closeParen + 1;
        continue;
      }

      // Property access in chain (e.g. reply.raw.end(...)).
      i = j;
    }
  }
  return hits;
}

function findCallArgHits(source, masked, callRe, patterns, bracketKind = "") {
  const hits = [];
  for (;;) {
    const m = callRe.exec(masked);
    if (!m) break;
    const callIdx = m.index;
    const openParen = m.index + m[0].length - 1;
    const closeParen = scanToMatchingParen(masked, openParen);
    if (closeParen === null) continue;
    const argsMasked = masked.slice(openParen + 1, closeParen);

    for (const { re, kind } of patterns) {
      if (re.test(argsMasked)) hits.push({ kind, index: callIdx });
    }

    if (bracketKind && hasErrMessageBracketAccess(source, masked, openParen + 1, closeParen)) {
      hits.push({ kind: bracketKind, index: callIdx });
    }
    if (bracketKind && hasErrMessageDotAccessWithUnicodeEscapes(source, masked, openParen + 1, closeParen)) {
      hits.push({ kind: bracketKind, index: callIdx });
    }

    const direct = parseDirectErrArg(source, openParen);
    if (direct) hits.push({ kind: "direct err arg", index: callIdx });
  }
  return hits;
}

export function findHttpErrorReflectionSinksInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // Guardrails for common HTTP frameworks / handlers:
  // - `res.send(err.message)` / `reply.send(err.message)`
  // - `res.end(String(err))` / `reply.end(String(error))`
  // - `reply.code(500).send(err.message)` / `res.status(500).end(String(err))`
  // - `reply.raw.end(err)` (implicit reflection)
  //
  // We keep this conservative and only look for `err`/`error` variables to minimize false positives.
  const terminalMethods = new Set(["send", "end", "json", "writeHead"]);
  const errMessageRe = /\b(?:err|error)\s*(?:\?\.\s*|\.\s*)message\b/u;
  const stringErrRe = /\bString\s*\(\s*(?:err|error)\s*\)/u;
  const stringErrParenRe = /\bString\s*\(\s*\(+\s*(?:err|error)\s*\)+\s*\)/u;
  const jsonStringifyErrRe = /\bJSON\s*\.\s*stringify\s*\(\s*(?:err|error)\b/u;
  const jsonStringifyErrParenRe = /\bJSON\s*\.\s*stringify\s*\(\s*\(+\s*(?:err|error)\b/u;
  // NOTE: bracket-string properties are parsed via `hasErrMessageBracketAccess()` because
  // string literal contents are masked out by `stripStringsAndComments()`.
  // Parenthesized error identifiers: `(err).message`, `((error))?.message`, `(err)["message"]`.
  //
  // We require the leading `(` not be preceded by an identifier char to avoid
  // false positives on call-argument patterns like `foo(err).message`.
  const errMessageParenRe = /(?<![\w$])\(+\s*(?:err|error)\s*\)+\s*(?:\?\.\s*|\.\s*)message\b/u;
  const patternsByMethod = new Map([
    [
      "send",
      [
        { re: errMessageRe, kind: "http send err.message" },
        { re: errMessageParenRe, kind: "http send err.message" },
        { re: stringErrRe, kind: "http send String(err)" },
        { re: stringErrParenRe, kind: "http send String(err)" },
        { re: jsonStringifyErrRe, kind: "http send JSON.stringify(err)" },
        { re: jsonStringifyErrParenRe, kind: "http send JSON.stringify(err)" },
      ],
    ],
    [
      "end",
      [
        { re: errMessageRe, kind: "http end err.message" },
        { re: errMessageParenRe, kind: "http end err.message" },
        { re: stringErrRe, kind: "http end String(err)" },
        { re: stringErrParenRe, kind: "http end String(err)" },
        { re: jsonStringifyErrRe, kind: "http end JSON.stringify(err)" },
        { re: jsonStringifyErrParenRe, kind: "http end JSON.stringify(err)" },
      ],
    ],
    [
      "json",
      [
        { re: errMessageRe, kind: "http json err.message" },
        { re: errMessageParenRe, kind: "http json err.message" },
        { re: stringErrRe, kind: "http json String(err)" },
        { re: stringErrParenRe, kind: "http json String(err)" },
        { re: jsonStringifyErrRe, kind: "http json JSON.stringify(err)" },
        { re: jsonStringifyErrParenRe, kind: "http json JSON.stringify(err)" },
        {
          re: /\b(?:message|error)\s*:\s*(?:err|error)\s*(?:\?\.\s*|\.\s*)message\b/u,
          kind: "http json {error: err.message}",
        },
        {
          re: /\b(?:message|error)\s*:\s*(?<![\w$])\(+\s*(?:err|error)\s*\)+\s*(?:\?\.\s*|\.\s*)message\b/u,
          kind: "http json {error: err.message}",
        },
      ],
    ],
    [
      "writeHead",
      [
        { re: errMessageRe, kind: "http writeHead err.message" },
        { re: errMessageParenRe, kind: "http writeHead err.message" },
        { re: stringErrRe, kind: "http writeHead String(err)" },
        { re: stringErrParenRe, kind: "http writeHead String(err)" },
      ],
    ],
  ]);

  hits.push(
    ...scanChainedReceiverCalls(source, masked, new Set(["res", "reply"]), terminalMethods, patternsByMethod),
  );

  // Backward-compatible direct-call scans (cheap, redundant safety net).
  hits.push(
    ...findCallArgHits(source, masked, /\b(?:res|reply)\s*\.\s*(?:send|end)\s*\(/gmu, [
      { re: errMessageRe, kind: "http send/end err.message" },
      { re: errMessageParenRe, kind: "http send/end err.message" },
      { re: stringErrRe, kind: "http send/end String(err)" },
      { re: stringErrParenRe, kind: "http send/end String(err)" },
      { re: jsonStringifyErrRe, kind: "http send/end JSON.stringify(err)" },
      { re: jsonStringifyErrParenRe, kind: "http send/end JSON.stringify(err)" },
    ], "http send/end err.message"),
  );

  hits.push(
    ...findCallArgHits(source, masked, /\b(?:res|reply)\s*\.\s*json\s*\(/gmu, [
      { re: errMessageRe, kind: "http json err.message" },
      { re: errMessageParenRe, kind: "http json err.message" },
      { re: stringErrRe, kind: "http json String(err)" },
      { re: stringErrParenRe, kind: "http json String(err)" },
      { re: jsonStringifyErrRe, kind: "http json JSON.stringify(err)" },
      { re: jsonStringifyErrParenRe, kind: "http json JSON.stringify(err)" },
      {
        re: /\b(?:message|error)\s*:\s*(?:err|error)\s*(?:\?\.\s*|\.\s*)message\b/u,
        kind: "http json {error: err.message}",
      },
      {
        re: /\b(?:message|error)\s*:\s*(?<![\w$])\(+\s*(?:err|error)\s*\)+\s*(?:\?\.\s*|\.\s*)message\b/u,
        kind: "http json {error: err.message}",
      },
    ], "http json err.message"),
  );

  // Status-line reflection: `res.writeHead(status, err.message)` etc.
  hits.push(
    ...findCallArgHits(source, masked, /\b(?:res|reply)\s*\.\s*writeHead\s*\(/gmu, [
      { re: errMessageRe, kind: "http writeHead err.message" },
      { re: errMessageParenRe, kind: "http writeHead err.message" },
      { re: stringErrRe, kind: "http writeHead String(err)" },
      { re: stringErrParenRe, kind: "http writeHead String(err)" },
    ], "http writeHead err.message"),
  );

  // Node streams/sockets occasionally serve HTTP error responses directly.
  hits.push(
    ...findCallArgHits(source, masked, /\bsocket\s*\.\s*end\s*\(/gmu, [
      { re: errMessageRe, kind: "socket.end err.message" },
      { re: errMessageParenRe, kind: "socket.end err.message" },
      { re: stringErrRe, kind: "socket.end String(err)" },
      { re: stringErrParenRe, kind: "socket.end String(err)" },
      { re: jsonStringifyErrRe, kind: "socket.end JSON.stringify(err)" },
      { re: jsonStringifyErrParenRe, kind: "socket.end JSON.stringify(err)" },
    ], "socket.end err.message"),
  );

  hits.push(
    ...findCallArgHits(source, masked, /\bsocket\s*\.\s*write\s*\(/gmu, [
      { re: errMessageRe, kind: "socket.write err.message" },
      { re: errMessageParenRe, kind: "socket.write err.message" },
      { re: stringErrRe, kind: "socket.write String(err)" },
      { re: stringErrParenRe, kind: "socket.write String(err)" },
      { re: jsonStringifyErrRe, kind: "socket.write JSON.stringify(err)" },
      { re: jsonStringifyErrParenRe, kind: "socket.write JSON.stringify(err)" },
    ], "socket.write err.message"),
  );

  // Normalize the generic "direct err arg" marker so callers see where it happened.
  for (const h of hits) {
    if (h.kind !== "direct err arg") continue;
    h.kind = "http direct err arg";
  }

  return hits;
}

