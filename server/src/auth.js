import { tryGetProp } from "../../src/safe_props.js";

const MAX_ORIGIN_LEN = 4 * 1024;
const MAX_AUTH_HEADER_LEN = 4 * 1024;
const MAX_TOKEN_LEN = 4 * 1024;

export function extractBearerToken(headerValue) {
  if (!headerValue) return null;
  if (typeof headerValue !== "string") return null;
  if (headerValue.length > MAX_AUTH_HEADER_LEN) return null;
  const match = /^Bearer (.+)$/i.exec(headerValue.trim());
  const token = match ? match[1] : null;
  if (typeof token !== "string") return null;
  if (token.length === 0 || token.length > MAX_TOKEN_LEN) return null;
  return token;
}

export function getAuthTokenFromRequest(req, urlSearchParams) {
  const fromQuery = urlSearchParams?.get?.("token");
  if (fromQuery && fromQuery.length <= MAX_TOKEN_LEN) return fromQuery;
  const fromAuthz = extractBearerToken(tryGetProp(tryGetProp(req, "headers"), "authorization"));
  if (fromAuthz) return fromAuthz;
  return null;
}

export function isTokenAllowed(token, allowedTokens) {
  if (!token) return false;
  return allowedTokens.includes(token);
}

export function isOriginAllowed(origin, allowedOrigins) {
  if (!allowedOrigins || allowedOrigins.length === 0) return true;
  if (!origin) return false;
  if (typeof origin !== "string") return false;
  if (origin.length > MAX_ORIGIN_LEN) return false;
  return allowedOrigins.includes(origin);
}

