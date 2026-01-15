import type { FastifyInstance } from 'fastify';

import type { Config } from '../config.js';
import { encodeDnsQuery, normalizeDnsName } from '../dns/codec.js';
import type { DnsResolver } from '../dns/resolver.js';
import type { TokenBucketRateLimiter } from '../dns/rateLimit.js';
import { dnsResponseToJson } from '../dns/dnsJson.js';
import { parseDnsRecordType } from '../dns/recordTypes.js';
import type { SessionManager } from '../session.js';

type DnsJsonQuery = { name?: string; type?: string };

export function setupDnsJsonRoutes(
  app: FastifyInstance,
  config: Config,
  opts: { resolver: DnsResolver; rateLimiter: TokenBucketRateLimiter; sessions: SessionManager },
): void {
  app.get('/dns-json', async (request, reply) => {
    const session = opts.sessions.verifySessionRequest(request.raw);
    if (!session) {
      reply.header('cache-control', 'no-store');
      return reply.code(401).send({ error: 'unauthorized', message: 'Missing or invalid session' });
    }

    const ip = request.ip ?? 'unknown';
    if (!opts.rateLimiter.allow(ip)) {
      return reply.code(429).send({ error: 'too_many_requests', message: 'Rate limit exceeded' });
    }

    const query = request.query as DnsJsonQuery;
    const name = query.name ? normalizeDnsName(query.name) : '';
    if (!name) {
      return reply.code(400).send({ error: 'bad_request', message: "Missing 'name' query parameter" });
    }

    if (!query.type) {
      return reply.code(400).send({ error: 'bad_request', message: "Missing 'type' query parameter" });
    }

    let type: number;
    try {
      type = parseDnsRecordType(query.type);
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Invalid DNS record type';
      return reply.code(400).send({ error: 'bad_request', message });
    }

    const dnsQuery = encodeDnsQuery({ id: 0, name, type });
    if (dnsQuery.length > config.DNS_MAX_QUERY_BYTES) {
      return reply.code(413).send({ error: 'payload_too_large', message: 'DNS query too large' });
    }

    try {
      const { response, rcode, cacheHit } = await opts.resolver.resolve(dnsQuery);
      request.log.info({ qname: name, qtype: type, rcode, cacheHit }, 'dns_json');

      reply.header('content-type', 'application/dns-json');
      reply.header('cache-control', 'no-store');
      return reply.send(dnsResponseToJson(response, { name, type }));
    } catch (err) {
      const message = err instanceof Error ? err.message : 'DNS resolution failed';

      if (message.includes('ANY queries are disabled') || message.includes('PTR queries to private ranges are disabled')) {
        return reply.code(403).send({ error: 'forbidden', message });
      }

      if (message.startsWith('DNS ')) {
        return reply.code(400).send({ error: 'bad_request', message });
      }

      if (message.includes('Upstream response too large')) {
        return reply.code(502).send({ error: 'bad_gateway', message });
      }

      return reply.code(502).send({ error: 'bad_gateway', message });
    }
  });
}
