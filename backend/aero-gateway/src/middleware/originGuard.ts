import type { FastifyReply, FastifyRequest } from 'fastify';

import { normalizeOriginString } from '../security/origin.js';

export function isNormalizedOriginAllowed(normalizedOrigin: string, allowedOrigins: readonly string[]): boolean {
  if (allowedOrigins.includes('*')) return true;
  return allowedOrigins.includes(normalizedOrigin);
}

export function isOriginAllowed(originHeader: string, allowedOrigins: readonly string[]): boolean {
  const normalized = normalizeOriginString(originHeader);
  if (!normalized) return false;

  return isNormalizedOriginAllowed(normalized, allowedOrigins);
}

export async function originGuard(
  request: FastifyRequest,
  reply: FastifyReply,
  opts: { allowedOrigins: readonly string[] },
): Promise<void> {
  const origin = request.headers.origin;
  if (!origin) return;

  const normalizedOrigin = normalizeOriginString(origin);
  if (!normalizedOrigin || !isNormalizedOriginAllowed(normalizedOrigin, opts.allowedOrigins)) {
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
