import type { FastifyReply, FastifyRequest } from 'fastify';

function normalizeOriginHeader(origin: string): string | null {
  const trimmed = origin.trim();
  if (trimmed === 'null') return 'null';

  try {
    return new URL(trimmed).origin;
  } catch {
    return null;
  }
}

export function isOriginAllowed(originHeader: string, allowedOrigins: readonly string[]): boolean {
  if (allowedOrigins.includes('*')) return true;

  const normalized = normalizeOriginHeader(originHeader);
  if (!normalized) return false;

  return allowedOrigins.includes(normalized);
}

export async function originGuard(
  request: FastifyRequest,
  reply: FastifyReply,
  opts: { allowedOrigins: readonly string[] },
): Promise<void> {
  const origin = request.headers.origin;
  if (!origin) return;

  if (!isOriginAllowed(origin, opts.allowedOrigins)) {
    reply.code(403).send({ error: 'forbidden', message: 'Origin not allowed' });
    return;
  }

  reply.header('access-control-allow-origin', normalizeOriginHeader(origin) ?? origin);
  reply.header('access-control-expose-headers', 'x-request-id');
  reply.header('vary', 'Origin');

  // Basic CORS preflight support for browser clients.
  if (request.method === 'OPTIONS' && request.headers['access-control-request-method']) {
    const requestHeaders = request.headers['access-control-request-headers'];
    reply.header('access-control-allow-methods', 'GET,POST,PUT,PATCH,DELETE,OPTIONS');
    if (requestHeaders) {
      reply.header('access-control-allow-headers', Array.isArray(requestHeaders) ? requestHeaders.join(',') : requestHeaders);
    }
    reply.header('access-control-max-age', '600');
    reply.code(204).send();
    return;
  }
}
