import type { FastifyReply, FastifyRequest } from 'fastify';

import { normalizeOriginString } from '../security/origin.js';

function isValidHostHeaderString(trimmed: string): boolean {
  if (trimmed === '') return false;
  if (trimmed.charCodeAt(trimmed.length - 1) === 0x3a /* ':' */) return false;

  for (let i = 0; i < trimmed.length; i += 1) {
    const c = trimmed.charCodeAt(i);
    // Host is an ASCII serialization. Be strict about rejecting non-ASCII or
    // non-printable characters that URL parsers may normalize away.
    if (c <= 0x20 || c >= 0x7f) return false;
    // Disallow percent-encoding and IPv6 zone identifiers; different URL parsers
    // disagree on how to handle them, and browsers won't emit them in Host.
    if (c === 0x25 /* '%' */) return false;
    // Reject comma-delimited values. Some HTTP stacks may join repeated headers
    // with commas.
    if (c === 0x2c /* ',' */) return false;
    // Reject path/query/fragment delimiters. Host headers are a host[:port]
    // serialization and must not contain these.
    if (c === 0x2f /* '/' */ || c === 0x3f /* '?' */ || c === 0x23 /* '#' */) return false;
    // Reject backslashes; some URL parsers normalize them to `/`.
    if (c === 0x5c /* '\\' */) return false;
    // Reject userinfo delimiters.
    if (c === 0x40 /* '@' */) return false;
  }

  return true;
}

function normalizeRequestHost(requestHost: string, scheme: 'http' | 'https'): string | null {
  const trimmed = requestHost.trim();
  if (!isValidHostHeaderString(trimmed)) return null;

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
