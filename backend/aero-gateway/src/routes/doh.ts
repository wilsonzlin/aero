import type { FastifyInstance } from 'fastify';

import type { Config } from '../config.js';
import type { DnsMetrics } from '../metrics.js';
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsErrorResponse } from '../dns/codec.js';
import { DnsPolicyError, DnsResolver, qtypeToString, rcodeToString } from '../dns/resolver.js';
import { TokenBucketRateLimiter } from '../dns/rateLimit.js';
import { base64UrlPrefixForHeader, decodeBase64UrlToBuffer, maxBase64UrlLenForBytes } from '../base64url.js';
import { headerHasMimeType } from '../contentType.js';
import type { SessionManager } from '../session.js';
import { setupDnsJsonRoutes } from './dnsJson.js';
import { formatOneLineUtf8 } from '../util/text.js';

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

  const MAX_CONTENT_TYPE_LEN = 256;
  const MAX_DNS_ERROR_LOG_BYTES = 512;

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
      const session = sessions.verifySessionRequest(request.raw);
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
          // Avoid decoding arbitrarily large `dns` query params into buffers. For valid base64url,
          // the encoded length is strictly monotonic with decoded byte length, so we can enforce
          // DNS_MAX_QUERY_BYTES before decoding the full message.
          const maxEncodedLen = maxBase64UrlLenForBytes(config.DNS_MAX_QUERY_BYTES);
          if (dns.length > maxEncodedLen) {
            // Best-effort decode of the DNS header (first 12 bytes) so we can preserve query ID
            // in the 413 response without allocating the entire message.
            const prefix = base64UrlPrefixForHeader(dns, 16);
            let id = 0;
            let queryFlags = 0;
            if (prefix) {
              try {
                const headerBytes = decodeBase64UrlToBuffer(prefix);
                try {
                  const header = decodeDnsHeader(headerBytes);
                  id = header.id;
                  queryFlags = header.flags;
                } catch {
                  if (headerBytes.length >= 2) id = headerBytes.readUInt16BE(0);
                  if (headerBytes.length >= 4) queryFlags = headerBytes.readUInt16BE(2);
                }
              } catch {
                // ignore
              }
            }
            return sendDnsError(reply, 413, { id, queryFlags, rcode: 1 });
          }

          query = decodeBase64UrlToBuffer(dns);
        } else {
          if (!headerHasMimeType(request.headers["content-type"], "application/dns-message", MAX_CONTENT_TYPE_LEN)) {
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
        let queryFlags = 0;
        try {
          const header = decodeDnsHeader(query);
          id = header.id;
          queryFlags = header.flags;
        } catch {
          if (query.length >= 2) id = query.readUInt16BE(0);
          if (query.length >= 4) queryFlags = query.readUInt16BE(2);
        }
        return sendDnsError(reply, 413, { id, queryFlags, rcode: 1 });
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
        if (err instanceof DnsPolicyError) {
          metrics.dnsQueriesTotal.inc({
            qtype: qtypeToString(question.type),
            rcode: rcodeToString(5),
            source: 'policy',
          });
          request.log.info({ qname: question.name, qtype: question.type, rcode: 5 }, 'dns_response');
          return sendDnsError(reply, 200, { id, queryFlags, question, rcode: 5 });
        }

        const rawMessage = err instanceof Error ? err.message : '';

        // All resolver failures should map to a standard DNS error response (SERVFAIL) so
        // DoH clients always receive a valid DNS message payload.
        metrics.dnsQueriesTotal.inc({
          qtype: qtypeToString(question.type),
          rcode: rcodeToString(2),
          source: 'error',
        });
        const errForLog = formatOneLineUtf8(rawMessage, MAX_DNS_ERROR_LOG_BYTES) || 'DNS resolution failed';
        request.log.warn({ qname: question.name, qtype: question.type, err: errForLog }, 'dns_error');
        return sendDnsError(reply, 200, { id, queryFlags, question, rcode: 2 });
      }
    },
  });

  return { resolver, rateLimiter };
}
