import type { IncomingMessage, ServerResponse } from "node:http";

import type { ProxyConfig } from "./config";
import { asciiLowerEqualsSpan } from "./ascii";

const MAX_CORS_REQUEST_HEADERS_LEN = 4096;

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

export function setDohCorsHeaders(
  req: IncomingMessage,
  res: ServerResponse,
  config: ProxyConfig,
  opts: { allowMethods?: string } = {}
): void {
  if (config.dohCorsAllowOrigins.length === 0) return;

  const requestOrigin = req.headers.origin;
  if (config.dohCorsAllowOrigins.includes("*")) {
    res.setHeader("Access-Control-Allow-Origin", "*");
  } else if (typeof requestOrigin === "string" && config.dohCorsAllowOrigins.includes(requestOrigin)) {
    res.setHeader("Access-Control-Allow-Origin", requestOrigin);
    res.setHeader("Vary", "Origin");
  } else {
    return;
  }

  if (opts.allowMethods) {
    res.setHeader("Access-Control-Allow-Methods", opts.allowMethods);
  }

  const requestedHeadersValue = sanitizeCorsRequestHeaders(req.headers["access-control-request-headers"]);
  // Always allow Content-Type for RFC8484 POST, even if the client didn't send a preflight header.
  const allowHeaders = requestedHeadersValue
    ? corsHeaderListHasToken(requestedHeadersValue, "content-type")
      ? requestedHeadersValue
      : `Content-Type, ${requestedHeadersValue}`
    : "Content-Type";
  res.setHeader("Access-Control-Allow-Headers", allowHeaders);

  // Allow browsers to read Content-Length cross-origin (useful for client-side size enforcement).
  res.setHeader("Access-Control-Expose-Headers", "Content-Length");

  // Cache preflight results in the browser to avoid an extra roundtrip for each DNS query during
  // local development.
  res.setHeader("Access-Control-Max-Age", "600");

  // Private Network Access (PNA) support: some browsers require an explicit opt-in response when a
  // secure context fetches a private-network target (e.g. localhost).
  const reqPrivateNetwork = req.headers["access-control-request-private-network"];
  const privateNetworkValue = typeof reqPrivateNetwork === "string" ? reqPrivateNetwork : "";
  if (privateNetworkValue.trim().toLowerCase() === "true") {
    res.setHeader("Access-Control-Allow-Private-Network", "true");
  }
}

