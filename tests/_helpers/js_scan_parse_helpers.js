export function isSpace(ch) {
  return ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === "\u2028" || ch === "\u2029";
}

export function skipWs(text, i) {
  while (i < text.length && isSpace(text[i])) i++;
  return i;
}

function isLineTerminator(ch) {
  return ch === "\n" || ch === "\r" || ch === "\u2028" || ch === "\u2029";
}

function isHexDigit(ch) {
  const code = ch.charCodeAt(0);
  return (code >= 48 && code <= 57) || (code >= 65 && code <= 70) || (code >= 97 && code <= 102);
}

function parseUnicodeEscapeIdentifierChar(source, startIdx) {
  if ((source[startIdx] || "") !== "\\" || (source[startIdx + 1] || "") !== "u") return null;
  const next = source[startIdx + 2] || "";

  if (next === "{") {
    let j = startIdx + 3;
    let hex = "";
    for (;;) {
      if (j >= source.length) return null;
      const c = source[j] || "";
      if (c === "}") break;
      if (!isHexDigit(c)) return null;
      hex += c;
      if (hex.length > 6) return null;
      j++;
    }
    if (hex.length === 0) return null;
    const codePoint = Number.parseInt(hex, 16);
    if (!Number.isFinite(codePoint) || codePoint < 0 || codePoint > 0x10ffff) return null;
    if (codePoint > 0x7f) return null; // keep parsing conservative: ASCII only
    return { ch: String.fromCharCode(codePoint), endIdxExclusive: j + 1 };
  }

  const a = source[startIdx + 2] || "";
  const b = source[startIdx + 3] || "";
  const c = source[startIdx + 4] || "";
  const d = source[startIdx + 5] || "";
  if (!a || !b || !c || !d) return null;
  if (!isHexDigit(a) || !isHexDigit(b) || !isHexDigit(c) || !isHexDigit(d)) return null;
  const codePoint = Number.parseInt(a + b + c + d, 16);
  if (!Number.isFinite(codePoint) || codePoint < 0 || codePoint > 0x7f) return null;
  return { ch: String.fromCharCode(codePoint), endIdxExclusive: startIdx + 6 };
}

export function skipWsAndComments(source, i) {
  while (i < source.length) {
    const ch = source[i];
    const next = i + 1 < source.length ? source[i + 1] : "";
    if (isSpace(ch)) {
      i++;
      continue;
    }
    if (ch === "/" && next === "/") {
      i += 2;
      while (i < source.length && !isLineTerminator(source[i] || "")) i++;
      continue;
    }
    if (ch === "/" && next === "*") {
      i += 2;
      while (i + 1 < source.length && !(source[i] === "*" && source[i + 1] === "/")) i++;
      if (i + 1 < source.length) i += 2;
      continue;
    }
    return i;
  }
  return i;
}

function parseJsStringLiteralBody(source, startIdx, endQuote, allowNewlines, disallowTemplateExpr) {
  const out = [];
  let i = startIdx;
  for (;;) {
    if (i >= source.length) return null;
    const ch = source[i] || "";
    const next = source[i + 1] || "";
    if (ch === endQuote) return { value: out.join(""), endIdxExclusive: i + 1 };

    if (!allowNewlines && (ch === "\n" || ch === "\r" || ch === "\u2028" || ch === "\u2029")) return null;
    if (disallowTemplateExpr && ch === "$" && next === "{") return null;

    if (ch !== "\\") {
      out.push(ch);
      i++;
      continue;
    }

    if (i + 1 >= source.length) return null;
    const esc = source[i + 1] || "";

    // Line continuation: `\` + line terminator.
    if (esc === "\n") {
      i += 2;
      continue;
    }
    if (esc === "\r") {
      i += 2;
      if ((source[i] || "") === "\n") i++;
      continue;
    }
    if (esc === "\u2028" || esc === "\u2029") {
      i += 2;
      continue;
    }

    // Legacy octal escapes: `\123` → char code 0o123.
    if (esc >= "0" && esc <= "7") {
      let j = i + 1;
      let digits = "";
      for (let k = 0; k < 3 && j < source.length; k++) {
        const d = source[j] || "";
        if (d < "0" || d > "7") break;
        digits += d;
        j++;
      }
      if (digits === "0" && ((source[j] || "") >= "0" && (source[j] || "") <= "9")) return null;
      const code = Number.parseInt(digits, 8);
      out.push(String.fromCharCode(code));
      i = j;
      continue;
    }

    const SIMPLE = {
      "'": "'",
      '"': '"',
      "`": "`",
      "\\": "\\",
      b: "\b",
      f: "\f",
      n: "\n",
      r: "\r",
      t: "\t",
      v: "\v",
      0: "\0",
    };
    if (Object.prototype.hasOwnProperty.call(SIMPLE, esc)) {
      // `\0` is only valid when not followed by a digit.
      if (esc === "0") {
        const after = source[i + 2] || "";
        if (after >= "0" && after <= "9") return null;
      }
      out.push(SIMPLE[esc]);
      i += 2;
      continue;
    }

    if (esc === "x") {
      const a = source[i + 2] || "";
      const b = source[i + 3] || "";
      if (!a || !b) return null;
      if (!isHexDigit(a) || !isHexDigit(b)) return null;
      out.push(String.fromCharCode(Number.parseInt(a + b, 16)));
      i += 4;
      continue;
    }

    if (esc === "u") {
      const next2 = source[i + 2] || "";
      if (next2 === "{") {
        let j = i + 3;
        let hex = "";
        for (;;) {
          if (j >= source.length) return null;
          const c = source[j] || "";
          if (c === "}") break;
          if (!isHexDigit(c)) return null;
          hex += c;
          if (hex.length > 6) return null;
          j++;
        }
        if (hex.length === 0) return null;
        const codePoint = Number.parseInt(hex, 16);
        if (codePoint > 0x10ffff) return null;
        out.push(String.fromCodePoint(codePoint));
        i = j + 1;
        continue;
      }

      const a = source[i + 2] || "";
      const b = source[i + 3] || "";
      const c = source[i + 4] || "";
      const d = source[i + 5] || "";
      if (!a || !b || !c || !d) return null;
      if (!isHexDigit(a) || !isHexDigit(b) || !isHexDigit(c) || !isHexDigit(d)) return null;
      out.push(String.fromCharCode(Number.parseInt(a + b + c + d, 16)));
      i += 6;
      continue;
    }

    // Unknown escapes (e.g. `\z`) are treated as the escaped char in non-strict JS.
    out.push(esc);
    i += 2;
  }
}

export function parseQuotedStringLiteral(source, quoteIdx) {
  const quote = source[quoteIdx];
  if (quote !== "'" && quote !== '"') return null;
  return parseJsStringLiteralBody(source, quoteIdx + 1, quote, false, false);
}

export function parseNoSubstTemplateLiteral(source, quoteIdx) {
  const quote = source[quoteIdx];
  if (quote !== "`") return null;
  return parseJsStringLiteralBody(source, quoteIdx + 1, "`", true, true);
}

export function parseStringLiteralOrNoSubstTemplate(source, startIdx) {
  const ch = source[startIdx] || "";
  if (ch === "'" || ch === '"') return parseQuotedStringLiteral(source, startIdx);
  if (ch === "`") return parseNoSubstTemplateLiteral(source, startIdx);
  return null;
}

export function parseBracketStringProperty(source, openBracketIdx) {
  if ((source[openBracketIdx] || "") !== "[") return null;
  let i = skipWsAndComments(source, openBracketIdx + 1);
  const lit = parseStringLiteralOrNoSubstTemplate(source, i);
  if (!lit) return null;
  i = skipWsAndComments(source, lit.endIdxExclusive);
  if ((source[i] || "") !== "]") return null;
  return { property: lit.value, closeBracketIdx: i, endIdxExclusive: i + 1 };
}

export function isIdentStart(ch) {
  return (
    (ch >= "A" && ch <= "Z") ||
    (ch >= "a" && ch <= "z") ||
    ch === "_" ||
    ch === "$"
  );
}

export function isIdentContinue(ch) {
  return isIdentStart(ch) || (ch >= "0" && ch <= "9");
}

export function parseIdentifier(source, startIdx) {
  const first = source[startIdx] || "";
  if (!isIdentStart(first)) return null;
  let i = startIdx + 1;
  while (i < source.length && isIdentContinue(source[i] || "")) i++;
  return { value: source.slice(startIdx, i), endIdxExclusive: i };
}

export function parseIdentifierWithUnicodeEscapes(source, startIdx) {
  let i = startIdx;
  let first = true;
  const out = [];
  for (;;) {
    if (i >= source.length) break;
    const ch = source[i] || "";
    if (ch === "\\") {
      const esc = parseUnicodeEscapeIdentifierChar(source, i);
      if (!esc) break;
      if (first ? !isIdentStart(esc.ch) : !isIdentContinue(esc.ch)) return null;
      out.push(esc.ch);
      i = esc.endIdxExclusive;
      first = false;
      continue;
    }

    if (first) {
      if (!isIdentStart(ch)) return null;
      out.push(ch);
      i++;
      first = false;
      continue;
    }
    if (!isIdentContinue(ch)) break;
    out.push(ch);
    i++;
  }
  if (first) return null;
  return { value: out.join(""), endIdxExclusive: i };
}

export function matchKeyword(source, idx, keyword) {
  if (source.slice(idx, idx + keyword.length) !== keyword) return false;
  const before = idx > 0 ? source[idx - 1] : "";
  const after = source[idx + keyword.length] || "";
  if (before && isIdentContinue(before)) return false;
  if (after && isIdentContinue(after)) return false;
  return true;
}

export function isOptionalCallStart(source, idx) {
  if (source[idx] === "(") return true;
  return source[idx] === "?" && source[idx + 1] === "." && source[idx + 2] === "(";
}

