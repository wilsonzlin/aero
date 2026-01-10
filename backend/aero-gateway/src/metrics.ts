import type { FastifyInstance } from 'fastify';
import { Counter, Histogram, collectDefaultMetrics, register } from 'prom-client';

const kStartTime = Symbol('metricsStartTime');

export function setupMetrics(app: FastifyInstance): void {
  collectDefaultMetrics({ register });

  const requestsTotal = new Counter({
    name: 'http_requests_total',
    help: 'Total number of HTTP requests',
    labelNames: ['method', 'route', 'status_code'],
    registers: [register],
  });

  const requestDurationSeconds = new Histogram({
    name: 'http_request_duration_seconds',
    help: 'HTTP request latency in seconds',
    labelNames: ['method', 'route', 'status_code'],
    buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10],
    registers: [register],
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
    reply.header('content-type', register.contentType);
    return register.metrics();
  });
}

