import type { FastifyInstance } from 'fastify';

import type { Config } from '../config.js';
import type { DnsMetrics } from '../metrics.js';
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsErrorResponse } from '../dns/codec.js';
import { DnsResolver, qtypeToString, rcodeToString } from '../dns/resolver.js';
import { TokenBucketRateLimiter } from '../dns/rateLimit.js';
import type { SessionManager } from '../session.js';
import { setupDnsJsonRoutes } from './dnsJson.js';

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

export type DohRouteDeps = Readonly<{
  resolver: DnsResolver;
  rateLimiter: TokenBucketRateLimiter;
}>;

export function setupDohRoutes(
  app: FastifyInstance,
  config: Config,
  metrics: DnsMetrics,
  sessions: SessionManager,
  deps: Partial<DohRouteDeps> = {},
): DohRouteDeps {
  const resolver = deps.resolver ?? new DnsResolver(config, metrics);
  const rateLimiter = deps.rateLimiter ?? new TokenBucketRateLimiter(config.DNS_QPS_PER_IP, config.DNS_BURST_PER_IP);
  setupDnsJsonRoutes(app, config, { resolver, rateLimiter, sessions });

  function sendDnsMessage(reply: import('fastify').FastifyReply, statusCode: number, message: Buffer) {
    reply.code(statusCode);
    reply.header('content-type', 'application/dns-message');
    reply.header('cache-control', 'no-store');
    return reply.send(message);
  }

  function sendDnsError(
    reply: import('fastify').FastifyReply,
    statusCode: number,
    opts: Parameters<typeof encodeDnsErrorResponse>[0],
  ) {
    return sendDnsMessage(reply, statusCode, encodeDnsErrorResponse(opts));
  }

  // `addContentTypeParser` is global across the Fastify instance (it is inherited
  // by registered plugins/children). If we install DoH routes under multiple
  // prefixes (e.g. base-path aliases), avoid re-registering the parser.
  if (!app.hasContentTypeParser('application/dns-message')) {
    app.addContentTypeParser(
      'application/dns-message',
      { parseAs: 'buffer' },
      (_request, body, done) => done(null, body),
    );
  }

  app.route({
    method: ['GET', 'POST'],
    url: '/dns-query',
    handler: async (request, reply) => {
      const session = sessions.verifySessionCookie(request.headers.cookie);
      if (!session) {
        reply.header('cache-control', 'no-store');
        return reply.code(401).send({ error: 'unauthorized', message: 'Missing or invalid session' });
      }

      const ip = request.ip ?? 'unknown';
      if (!rateLimiter.allow(ip)) {
        return sendDnsError(reply, 429, { id: 0, rcode: 2 });
      }

      let query: Buffer;
      try {
        if (request.method === 'GET') {
          const dns = (request.query as DohQuery).dns;
          if (!dns) {
            return sendDnsError(reply, 400, { id: 0, rcode: 1 });
          }
          query = decodeBase64UrlToBuffer(dns);
        } else {
          const contentType = request.headers['content-type']?.split(';')[0]?.trim().toLowerCase();
          if (contentType !== 'application/dns-message') {
            return sendDnsError(reply, 415, { id: 0, rcode: 1 });
          }

          const body = request.body;
          if (!Buffer.isBuffer(body)) {
            return sendDnsError(reply, 400, { id: 0, rcode: 1 });
          }
          query = body;
        }
      } catch (err) {
        return sendDnsError(reply, 400, { id: 0, rcode: 1 });
      }

      if (query.length > config.DNS_MAX_QUERY_BYTES) {
        // If the client sent a huge payload, avoid parsing more than the header.
        let id = 0;
        try {
          id = decodeDnsHeader(query).id;
        } catch {
          if (query.length >= 2) id = query.readUInt16BE(0);
        }
        return sendDnsError(reply, 413, { id, rcode: 1 });
      }

      let id = 0;
      let queryFlags = 0;
      try {
        const header = decodeDnsHeader(query);
        id = header.id;
        queryFlags = header.flags;
      } catch {
        if (query.length >= 2) id = query.readUInt16BE(0);
        if (query.length >= 4) queryFlags = query.readUInt16BE(2);
      }

      let question;
      try {
        question = decodeFirstQuestion(query);
      } catch (err) {
        request.log.warn({ err }, 'dns_parse_error');
        return sendDnsError(reply, 400, { id, queryFlags, rcode: 1 });
      }

      try {
        request.log.info({ qname: question.name, qtype: question.type }, 'dns_query');

        const { response, rcode, cacheHit } = await resolver.resolve(query);
        request.log.info(
          { qname: question.name, qtype: question.type, rcode, cacheHit, bytes: response.length },
          'dns_response',
        );

        return sendDnsMessage(reply, 200, response);
      } catch (err) {
        const message = err instanceof Error ? err.message : 'DNS resolution failed';

        if (message.includes('ANY queries are disabled') || message.includes('PTR queries to private ranges are disabled')) {
          metrics.dnsQueriesTotal.inc({
            qtype: qtypeToString(question.type),
            rcode: rcodeToString(5),
            source: 'policy',
          });
          request.log.info({ qname: question.name, qtype: question.type, rcode: 5 }, 'dns_response');
          return sendDnsError(reply, 200, { id, queryFlags, question, rcode: 5 });
        }

        // All resolver failures should map to a standard DNS error response (SERVFAIL) so
        // DoH clients always receive a valid DNS message payload.
        metrics.dnsQueriesTotal.inc({
          qtype: qtypeToString(question.type),
          rcode: rcodeToString(2),
          source: 'error',
        });
        request.log.warn({ qname: question.name, qtype: question.type, err: message }, 'dns_error');
        return sendDnsError(reply, 200, { id, queryFlags, question, rcode: 2 });
      }
    },
  });

  return { resolver, rateLimiter };
}
