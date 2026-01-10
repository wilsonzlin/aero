import type { FastifyInstance } from 'fastify';

import type { Config } from '../config.js';
import type { DnsMetrics } from '../metrics.js';
import { decodeFirstQuestion } from '../dns/codec.js';
import { DnsResolver } from '../dns/resolver.js';
import { TokenBucketRateLimiter } from '../dns/rateLimit.js';

export function decodeBase64UrlToBuffer(base64url: string): Buffer {
  if (!/^[A-Za-z0-9_-]+$/.test(base64url)) throw new Error('Invalid base64url');
  let base64 = base64url.replaceAll('-', '+').replaceAll('_', '/');
  const mod = base64.length % 4;
  if (mod === 2) base64 += '==';
  else if (mod === 3) base64 += '=';
  else if (mod !== 0) throw new Error('Invalid base64url length');
  return Buffer.from(base64, 'base64');
}

type DohQuery = { dns?: string };

export function setupDohRoutes(app: FastifyInstance, config: Config, metrics: DnsMetrics): void {
  const resolver = new DnsResolver(config, metrics);
  const rateLimiter = new TokenBucketRateLimiter(config.DNS_QPS_PER_IP, config.DNS_BURST_PER_IP);

  app.addContentTypeParser(
    'application/dns-message',
    { parseAs: 'buffer' },
    (_request, body, done) => done(null, body),
  );

  app.route({
    method: ['GET', 'POST'],
    url: '/dns-query',
    handler: async (request, reply) => {
      const ip = request.ip ?? 'unknown';
      if (!rateLimiter.allow(ip)) {
        return reply.code(429).send({ error: 'too_many_requests', message: 'Rate limit exceeded' });
      }

      let query: Buffer;
      try {
        if (request.method === 'GET') {
          const dns = (request.query as DohQuery).dns;
          if (!dns) {
            return reply.code(400).send({ error: 'bad_request', message: "Missing 'dns' query parameter" });
          }
          query = decodeBase64UrlToBuffer(dns);
        } else {
          const contentType = request.headers['content-type']?.split(';')[0]?.trim().toLowerCase();
          if (contentType !== 'application/dns-message') {
            return reply.code(415).send({ error: 'unsupported_media_type', message: 'Expected application/dns-message' });
          }

          const body = request.body;
          if (!Buffer.isBuffer(body)) {
            return reply.code(400).send({ error: 'bad_request', message: 'Expected binary DNS message body' });
          }
          query = body;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : 'Invalid request';
        return reply.code(400).send({ error: 'bad_request', message });
      }

      if (query.length > config.DNS_MAX_QUERY_BYTES) {
        return reply.code(413).send({ error: 'payload_too_large', message: 'DNS query too large' });
      }

      try {
        const question = decodeFirstQuestion(query);
        request.log.info({ qname: question.name, qtype: question.type }, 'dns_query');

        const { response, rcode, cacheHit } = await resolver.resolve(query);
        request.log.info(
          { qname: question.name, qtype: question.type, rcode, cacheHit, bytes: response.length },
          'dns_response',
        );

        reply.header('content-type', 'application/dns-message');
        reply.header('cache-control', 'no-store');
        return reply.send(response);
      } catch (err) {
        const message = err instanceof Error ? err.message : 'DNS resolution failed';

        if (message.includes('ANY queries are disabled') || message.includes('PTR queries to private ranges are disabled')) {
          return reply.code(403).send({ error: 'forbidden', message });
        }

        if (message.startsWith('DNS ') || message.startsWith('Invalid base64url')) {
          return reply.code(400).send({ error: 'bad_request', message });
        }

        if (message.includes('Upstream response too large')) {
          return reply.code(502).send({ error: 'bad_gateway', message });
        }

        return reply.code(502).send({ error: 'bad_gateway', message });
      }
    },
  });
}
