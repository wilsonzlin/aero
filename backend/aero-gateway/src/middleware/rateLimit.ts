import type { FastifyInstance } from 'fastify';
import { TokenBucketRateLimiter } from '../dns/rateLimit.js';

function getClientKey(ip: string | undefined): string {
  return ip ?? 'unknown';
}

export function setupRateLimit(app: FastifyInstance, opts: { requestsPerMinute: number }): void {
  if (opts.requestsPerMinute <= 0) return;

  const capacity = opts.requestsPerMinute;
  const limiter = new TokenBucketRateLimiter(capacity / 60, capacity);

  app.addHook('onRequest', async (request, reply) => {
    if (request.method === 'OPTIONS') return;

    const route = request.routeOptions?.url;
    // Health/metrics endpoints should remain usable even when request rate
    // limiting is enabled (liveness probes, dashboards, etc).
    //
    // When the gateway is deployed behind a reverse proxy under a base path
    // (e.g. `/aero`), the same handlers may be registered under that prefix
    // (e.g. `/aero/healthz`), so match by suffix instead of exact equality.
    if (route && (route === '/healthz' || route.endsWith('/healthz') || route === '/readyz' || route.endsWith('/readyz') || route === '/metrics' || route.endsWith('/metrics'))) {
      return;
    }

    const key = getClientKey(request.ip);
    if (!limiter.allow(key)) {
      reply.code(429).send({ error: 'too_many_requests', message: 'Rate limit exceeded' });
      return;
    }
  });
}
