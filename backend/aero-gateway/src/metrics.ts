import type { FastifyInstance } from 'fastify';
import { Counter, Histogram, Registry, collectDefaultMetrics } from 'prom-client';

const kStartTime = Symbol('metricsStartTime');

export type DnsMetrics = Readonly<{
  dnsQueriesTotal: Counter<'qtype' | 'rcode' | 'source'>;
  dnsCacheHitsTotal: Counter<'qtype'>;
  dnsCacheMissesTotal: Counter<'qtype'>;
  dnsUpstreamErrorsTotal: Counter<'upstream' | 'kind'>;
  dnsUpstreamLatencySeconds: Histogram<'upstream' | 'kind'>;
}>;

export type TcpProxyMetrics = Readonly<{
  blockedByHostPolicyTotal: Counter<string>;
  blockedByIpPolicyTotal: Counter<string>;
}>;

export type MetricsBundle = Readonly<{
  registry: Registry;
  dns: DnsMetrics;
  tcpProxy: TcpProxyMetrics;
}>;

export function setupMetrics(app: FastifyInstance): MetricsBundle {
  // Use a per-server registry so tests (and other embedders) can spin up multiple
  // Fastify instances in the same process without metric name collisions.
  const registry = new Registry();
  collectDefaultMetrics({ register: registry });

  const httpRequestsTotal = new Counter({
    name: 'http_requests_total',
    help: 'Total number of HTTP requests',
    labelNames: ['method', 'route', 'status_code'] as const,
    registers: [registry],
  });

  const httpRequestDurationSeconds = new Histogram({
    name: 'http_request_duration_seconds',
    help: 'HTTP request latency in seconds',
    labelNames: ['method', 'route', 'status_code'] as const,
    buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10],
    registers: [registry],
  });

  const dnsQueriesTotal = new Counter({
    name: 'dns_queries_total',
    help: 'Total number of DNS-over-HTTPS queries handled',
    labelNames: ['qtype', 'rcode', 'source'] as const,
    registers: [registry],
  });

  const dnsCacheHitsTotal = new Counter({
    name: 'dns_cache_hits_total',
    help: 'Total number of DNS cache hits',
    labelNames: ['qtype'] as const,
    registers: [registry],
  });

  const dnsCacheMissesTotal = new Counter({
    name: 'dns_cache_misses_total',
    help: 'Total number of DNS cache misses',
    labelNames: ['qtype'] as const,
    registers: [registry],
  });

  const dnsUpstreamErrorsTotal = new Counter({
    name: 'dns_upstream_errors_total',
    help: 'Total number of DNS upstream errors',
    labelNames: ['upstream', 'kind'] as const,
    registers: [registry],
  });

  const dnsUpstreamLatencySeconds = new Histogram({
    name: 'dns_upstream_latency_seconds',
    help: 'DNS upstream resolution latency in seconds',
    labelNames: ['upstream', 'kind'] as const,
    buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2, 5],
    registers: [registry],
  });

  const tcpProxyBlockedByHostPolicyTotal = new Counter({
    name: 'tcp_proxy_blocked_by_host_policy_total',
    help: 'Total number of TCP proxy dials blocked by hostname egress policy',
    registers: [registry],
  });

  const tcpProxyBlockedByIpPolicyTotal = new Counter({
    name: 'tcp_proxy_blocked_by_ip_policy_total',
    help: 'Total number of TCP proxy dials blocked by IP egress policy',
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

    httpRequestsTotal.inc(labels);
    httpRequestDurationSeconds.observe(labels, durationSeconds);
  });

  app.get('/metrics', async (_request, reply) => {
    reply.header('content-type', registry.contentType);
    return registry.metrics();
  });

  return {
    registry,
    dns: { dnsQueriesTotal, dnsCacheHitsTotal, dnsCacheMissesTotal, dnsUpstreamErrorsTotal, dnsUpstreamLatencySeconds },
    tcpProxy: { blockedByHostPolicyTotal: tcpProxyBlockedByHostPolicyTotal, blockedByIpPolicyTotal: tcpProxyBlockedByIpPolicyTotal },
  };
}
