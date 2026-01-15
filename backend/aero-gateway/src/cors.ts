import { asciiLowerEqualsSpan } from "./ascii.js";

export const MAX_CORS_REQUEST_HEADERS_LEN = 4096;

function isAsciiWhitespace(code: number): boolean {
  // Treat all ASCII control chars + space as “trim”.
  return code <= 0x20;
}

function isHttpTokenChar(code: number): boolean {
  // RFC7230 token = 1*tchar, where:
  // tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
  //         "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
  if (code >= 0x30 /* '0' */ && code <= 0x39 /* '9' */) return true;
  if (code >= 0x41 /* 'A' */ && code <= 0x5a /* 'Z' */) return true;
  if (code >= 0x61 /* 'a' */ && code <= 0x7a /* 'z' */) return true;
  return (
    code === 0x21 /* '!' */ ||
    code === 0x23 /* '#' */ ||
    code === 0x24 /* '$' */ ||
    code === 0x25 /* '%' */ ||
    code === 0x26 /* '&' */ ||
    code === 0x27 /* ''' */ ||
    code === 0x2a /* '*' */ ||
    code === 0x2b /* '+' */ ||
    code === 0x2d /* '-' */ ||
    code === 0x2e /* '.' */ ||
    code === 0x5e /* '^' */ ||
    code === 0x5f /* '_' */ ||
    code === 0x60 /* '`' */ ||
    code === 0x7c /* '|' */ ||
    code === 0x7e /* '~' */
  );
}

function isValidCorsHeaderNameList(s: string): boolean {
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charCodeAt(i);
    if (c === 0x2c /* ',' */ || isAsciiWhitespace(c) || isHttpTokenChar(c)) continue;
    return false;
  }
  return true;
}

function corsHeaderListHasToken(s: string, lowerToken: string): boolean {
  let i = 0;
  while (i < s.length) {
    while (i < s.length && (isAsciiWhitespace(s.charCodeAt(i)) || s.charCodeAt(i) === 0x2c /* ',' */)) i += 1;
    if (i >= s.length) break;
    const start = i;
    while (i < s.length && isHttpTokenChar(s.charCodeAt(i))) i += 1;
    const end = i;
    if (asciiLowerEqualsSpan(s, start, end, lowerToken)) return true;
    while (i < s.length && s.charCodeAt(i) !== 0x2c /* ',' */) i += 1;
  }
  return false;
}

function sanitizeCorsRequestHeaders(value: unknown): string | undefined {
  let raw: string;
  if (typeof value === "string") raw = value;
  else if (Array.isArray(value) && value.length === 1 && typeof value[0] === "string") raw = value[0];
  else return undefined;

  const trimmed = raw.trim();
  if (trimmed === "") return undefined;
  if (trimmed.length > MAX_CORS_REQUEST_HEADERS_LEN) return undefined;
  if (!isValidCorsHeaderNameList(trimmed)) return undefined;
  return trimmed;
}

export function corsAllowHeadersValue(requestHeaders: unknown): string {
  const requestedHeadersValue = sanitizeCorsRequestHeaders(requestHeaders);
  if (!requestedHeadersValue) return "Content-Type";
  return corsHeaderListHasToken(requestedHeadersValue, "content-type")
    ? requestedHeadersValue
    : `Content-Type, ${requestedHeadersValue}`;
}

