import type { FastifyInstance } from 'fastify';
import { Counter, Histogram, Registry, collectDefaultMetrics } from 'prom-client';

const kStartTime = Symbol('metricsStartTime');

export function setupMetrics(app: FastifyInstance): void {
  // Use a per-server registry so tests (and other embedders) can spin up multiple
  // Fastify instances in the same process without metric name collisions.
  const registry = new Registry();
  collectDefaultMetrics({ register: registry });

  const requestsTotal = new Counter({
    name: 'http_requests_total',
    help: 'Total number of HTTP requests',
    labelNames: ['method', 'route', 'status_code'],
    registers: [registry],
  });

  const requestDurationSeconds = new Histogram({
    name: 'http_request_duration_seconds',
    help: 'HTTP request latency in seconds',
    labelNames: ['method', 'route', 'status_code'],
    buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10],
    registers: [registry],
  });

  app.addHook('onRequest', async (request) => {
    (request as any)[kStartTime] = process.hrtime.bigint();
  });

  app.addHook('onResponse', async (request, reply) => {
    const start = (request as any)[kStartTime] as bigint | undefined;
    if (!start) return;

    const durationSeconds = Number(process.hrtime.bigint() - start) / 1e9;
    const route = request.routeOptions?.url ?? 'unknown';
    const labels = {
      method: request.method,
      route,
      status_code: String(reply.statusCode),
    };

    requestsTotal.inc(labels);
    requestDurationSeconds.observe(labels, durationSeconds);
  });

  app.get('/metrics', async (_request, reply) => {
    reply.header('content-type', registry.contentType);
    return registry.metrics();
  });
}
