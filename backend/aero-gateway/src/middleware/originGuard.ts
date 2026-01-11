import type { FastifyReply, FastifyRequest } from 'fastify';

import { normalizeOriginString } from '../security/origin.js';

function normalizeRequestHost(requestHost: string, scheme: 'http' | 'https'): string | null {
  const trimmed = requestHost.trim();
  if (trimmed === '') return null;
  if (trimmed.includes('@')) return null;
  // Reject empty port specs like `example.com:`. Node's URL parser may otherwise
  // treat this as if the port were absent, which would diverge from the stricter
  // behavior in our shared protocol vectors.
  if (trimmed.endsWith(':')) return null;

  let url: URL;
  try {
    url = new URL(`${scheme}://${trimmed}`);
  } catch {
    return null;
  }

  if (url.username !== '' || url.password !== '') return null;
  if (url.search !== '' || url.hash !== '') return null;
  if (url.pathname !== '/' && url.pathname !== '') return null;

  return url.host;
}

export function isNormalizedOriginAllowed(
  normalizedOrigin: string,
  allowedOrigins: readonly string[],
  requestHost: string = '',
): boolean {
  if (allowedOrigins.includes('*')) return true;

  if (allowedOrigins.length > 0) {
    return allowedOrigins.includes(normalizedOrigin);
  }

  // Default allowlist policy (when unset/empty): same host[:port].
  if (normalizedOrigin === 'null') return false;

  const scheme = normalizedOrigin.startsWith('http://') ? 'http' : normalizedOrigin.startsWith('https://') ? 'https' : null;
  if (!scheme) return false;

  const originHost = normalizedOrigin.slice(`${scheme}://`.length);
  const normalizedRequestHost = normalizeRequestHost(requestHost, scheme);
  if (!normalizedRequestHost) return false;
  return originHost === normalizedRequestHost;
}

export function isOriginAllowed(
  originHeader: string,
  allowedOrigins: readonly string[],
  requestHost: string = '',
): boolean {
  const normalized = normalizeOriginString(originHeader);
  if (!normalized) return false;

  return isNormalizedOriginAllowed(normalized, allowedOrigins, requestHost);
}

export async function originGuard(
  request: FastifyRequest,
  reply: FastifyReply,
  opts: { allowedOrigins: readonly string[] },
): Promise<void> {
  const originHeader = request.headers.origin;
  let origin: string | undefined;
  if (Array.isArray(originHeader)) {
    if (originHeader.length === 0) return;
    // Origin is a single-value header. Be strict: reject repeated headers to avoid
    // ambiguous join/parse behaviour across HTTP stacks.
    if (originHeader.length > 1) {
      reply.code(403).send({ error: 'forbidden', message: 'Origin not allowed' });
      return;
    }
    origin = originHeader[0];
  } else {
    origin = originHeader;
  }
  if (!origin) return;

  const normalizedOrigin = normalizeOriginString(origin);
  const hostHeader = request.headers.host;
  const requestHost = Array.isArray(hostHeader) ? hostHeader[0] : hostHeader ?? '';

  if (!normalizedOrigin || !isNormalizedOriginAllowed(normalizedOrigin, opts.allowedOrigins, requestHost)) {
    reply.code(403).send({ error: 'forbidden', message: 'Origin not allowed' });
    return;
  }

  reply.header('access-control-allow-origin', normalizedOrigin);
  reply.header('access-control-allow-credentials', 'true');
  reply.header('access-control-expose-headers', 'x-request-id');
  reply.header('vary', 'Origin');

  // Basic CORS preflight support for browser clients.
  if (request.method === 'OPTIONS' && request.headers['access-control-request-method']) {
    const requestHeaders = request.headers['access-control-request-headers'];
    reply.header('access-control-allow-methods', 'GET,POST,PUT,PATCH,DELETE,OPTIONS');
    if (requestHeaders) {
      reply.header('access-control-allow-headers', Array.isArray(requestHeaders) ? requestHeaders.join(',') : requestHeaders);
    }
    reply.header('access-control-allow-credentials', 'true');
    reply.header('access-control-max-age', '600');
    reply.code(204).send();
    return;
  }
}
