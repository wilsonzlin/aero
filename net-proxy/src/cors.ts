import type { IncomingMessage, ServerResponse } from "node:http";

import type { ProxyConfig } from "./config";
import { asciiLowerEqualsSpan } from "./ascii";
import { isTchar } from "./httpTokens";
import { tryGetProp } from "./safeProps";

const MAX_CORS_REQUEST_HEADERS_LEN = 4096;

function trySetHeader(res: ServerResponse, name: string, value: string): boolean {
  try {
    res.setHeader(name, value);
    return true;
  } catch {
    return false;
  }
}

function isAsciiWhitespace(code: number): boolean {
  // Treat all ASCII control chars + space as “trim”.
  return code <= 0x20;
}

function isValidCorsHeaderNameList(s: string): boolean {
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charCodeAt(i);
    if (c === 0x2c /* ',' */ || isAsciiWhitespace(c) || isTchar(c)) continue;
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
    while (i < s.length && isTchar(s.charCodeAt(i))) i += 1;
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

  const requestOrigin = tryGetProp(tryGetProp(req, "headers"), "origin");
  if (config.dohCorsAllowOrigins.includes("*")) {
    if (!trySetHeader(res, "Access-Control-Allow-Origin", "*")) return;
  } else if (typeof requestOrigin === "string" && config.dohCorsAllowOrigins.includes(requestOrigin)) {
    if (!trySetHeader(res, "Access-Control-Allow-Origin", requestOrigin)) return;
    trySetHeader(res, "Vary", "Origin");
  } else {
    return;
  }

  if (opts.allowMethods) {
    trySetHeader(res, "Access-Control-Allow-Methods", opts.allowMethods);
  }

  const requestedHeadersValue = sanitizeCorsRequestHeaders(
    tryGetProp(tryGetProp(req, "headers"), "access-control-request-headers")
  );
  // Always allow Content-Type for RFC8484 POST, even if the client didn't send a preflight header.
  const allowHeaders = requestedHeadersValue
    ? corsHeaderListHasToken(requestedHeadersValue, "content-type")
      ? requestedHeadersValue
      : `Content-Type, ${requestedHeadersValue}`
    : "Content-Type";
  trySetHeader(res, "Access-Control-Allow-Headers", allowHeaders);

  // Allow browsers to read Content-Length cross-origin (useful for client-side size enforcement).
  trySetHeader(res, "Access-Control-Expose-Headers", "Content-Length");

  // Cache preflight results in the browser to avoid an extra roundtrip for each DNS query during
  // local development.
  trySetHeader(res, "Access-Control-Max-Age", "600");

  // Private Network Access (PNA) support: some browsers require an explicit opt-in response when a
  // secure context fetches a private-network target (e.g. localhost).
  const reqPrivateNetwork = tryGetProp(tryGetProp(req, "headers"), "access-control-request-private-network");
  const privateNetworkValue = typeof reqPrivateNetwork === "string" ? reqPrivateNetwork : "";
  if (privateNetworkValue.trim().toLowerCase() === "true") {
    trySetHeader(res, "Access-Control-Allow-Private-Network", "true");
  }
}

