export function extractBearerToken(headerValue) {
  if (!headerValue) return null;
  const match = /^Bearer (.+)$/i.exec(headerValue.trim());
  return match ? match[1] : null;
}

export function getAuthTokenFromRequest(req, urlSearchParams) {
  const fromQuery = urlSearchParams?.get?.("token");
  if (fromQuery) return fromQuery;
  const fromAuthz = extractBearerToken(req.headers.authorization);
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
  return allowedOrigins.includes(origin);
}

