export function isSpace(ch) {
  return ch === " " || ch === "\t" || ch === "\n" || ch === "\r";
}

export function skipWs(text, i) {
  while (i < text.length && isSpace(text[i])) i++;
  return i;
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
      while (i < source.length && source[i] !== "\n") i++;
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

export function parseQuotedStringLiteral(source, quoteIdx) {
  const quote = source[quoteIdx];
  if (quote !== "'" && quote !== '"') return null;

  let i = quoteIdx + 1;
  for (;;) {
    if (i >= source.length) return null;
    const ch = source[i];
    if (ch === "\\") {
      i += 2;
      continue;
    }
    if (ch === quote) {
      return { value: source.slice(quoteIdx + 1, i), endIdxExclusive: i + 1 };
    }
    i++;
  }
}

