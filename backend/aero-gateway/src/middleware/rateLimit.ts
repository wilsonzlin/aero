import type { FastifyInstance } from 'fastify';

type Bucket = {
  tokens: number;
  updatedAtMs: number;
};

function getClientKey(ip: string | undefined): string {
  return ip ?? 'unknown';
}

export function setupRateLimit(app: FastifyInstance, opts: { requestsPerMinute: number }): void {
  if (opts.requestsPerMinute <= 0) return;

  const capacity = opts.requestsPerMinute;
  const refillPerMs = capacity / 60_000;
  const buckets = new Map<string, Bucket>();

  app.addHook('onRequest', async (request, reply) => {
    if (request.method === 'OPTIONS') return;

    const route = request.routeOptions?.url;
    if (route === '/healthz' || route === '/readyz' || route === '/metrics') return;

    const key = getClientKey(request.ip);
    const now = Date.now();
    const bucket = buckets.get(key) ?? { tokens: capacity, updatedAtMs: now };

    const elapsedMs = now - bucket.updatedAtMs;
    bucket.tokens = Math.min(capacity, bucket.tokens + elapsedMs * refillPerMs);
    bucket.updatedAtMs = now;

    if (bucket.tokens < 1) {
      reply.code(429).send({ error: 'too_many_requests', message: 'Rate limit exceeded' });
      return;
    }

    bucket.tokens -= 1;
    buckets.set(key, bucket);
  });
}
