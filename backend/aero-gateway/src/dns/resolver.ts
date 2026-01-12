import { performance } from 'node:perf_hooks';

import type { Config } from '../config.js';
import type { DnsMetrics } from '../metrics.js';
import { DnsCache, makeCacheKey } from './cache.js';
import { decodeDnsHeader, decodeFirstQuestion, extractCacheInfoFromResponse, getRcodeFromFlags } from './codec.js';
import { parseUpstreams, queryDohUpstream, queryUdpUpstream, type DnsUpstream } from './upstream.js';

function patchDnsId(message: Buffer, id: number): Buffer {
  const patched = Buffer.from(message);
  patched.writeUInt16BE(id & 0xffff, 0);
  return patched;
}

export function qtypeToString(qtype: number): string {
  // Minimal mapping for metrics.
  switch (qtype) {
    case 1:
      return "A";
    case 2:
      return "NS";
    case 5:
      return "CNAME";
    case 6:
      return "SOA";
    case 12:
      return "PTR";
    case 15:
      return "MX";
    case 16:
      return "TXT";
    case 28:
      return "AAAA";
    case 255:
      return 'ANY';
    default:
      return String(qtype);
  }
}

export function rcodeToString(rcode: number): string {
  switch (rcode) {
    case 0:
      return 'NOERROR';
    case 2:
      return 'SERVFAIL';
    case 3:
      return 'NXDOMAIN';
    case 5:
      return 'REFUSED';
    default:
      return String(rcode);
  }
}

function isPrivateIpv4(ip: string): boolean {
  const [a, b] = ip.split('.').map((p) => Number.parseInt(p, 10));
  if (![a, b].every((n) => Number.isFinite(n))) return false;
  if (a === 10) return true;
  if (a === 127) return true;
  if (a === 0) return true;
  if (a === 169 && b === 254) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 192 && b === 168) return true;
  if (a === 100 && b >= 64 && b <= 127) return true; // CGNAT
  return false;
}

function isPrivateIpv6(bytes: Uint8Array): boolean {
  // fc00::/7 (ULA)
  if ((bytes[0] & 0xfe) === 0xfc) return true;
  // fe80::/10 (link-local)
  if (bytes[0] === 0xfe && (bytes[1] & 0xc0) === 0x80) return true;
  // ::1 loopback, :: unspecified
  const allZero = bytes.every((b) => b === 0);
  if (allZero) return true;
  const loopback = bytes.slice(0, 15).every((b) => b === 0) && bytes[15] === 1;
  if (loopback) return true;
  return false;
}

function asciiEndsWithIgnoreCase(s: string, suffix: string): boolean {
  if (s.length < suffix.length) return false;
  const start = s.length - suffix.length;
  for (let i = 0; i < suffix.length; i += 1) {
    let c = s.charCodeAt(start + i);
    if (c >= 0x41 && c <= 0x5a) c += 0x20; // ASCII upper -> lower
    if (c !== suffix.charCodeAt(i)) return false;
  }
  return true;
}

export function isPrivatePtrQname(qname: string): boolean {
  if (asciiEndsWithIgnoreCase(qname, '.in-addr.arpa')) {
    const base = qname.slice(0, -'.in-addr.arpa'.length);
    const octets = base.split('.').filter(Boolean);
    if (octets.length !== 4) return false;
    const ip = octets.reverse().join('.');
    return isPrivateIpv4(ip);
  }

  if (asciiEndsWithIgnoreCase(qname, '.ip6.arpa')) {
    const base = qname.slice(0, -'.ip6.arpa'.length);
    const nibbles = base.split('.').filter(Boolean);
    if (nibbles.length !== 32) return false;
    const hex = nibbles.reverse().join('');
    if (!/^[0-9a-fA-F]{32}$/.test(hex)) return false;
    const bytes = new Uint8Array(16);
    for (let i = 0; i < 16; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    return isPrivateIpv6(bytes);
  }

  return false;
}

export class DnsResolver {
  private readonly cache: DnsCache;
  private readonly upstreams: DnsUpstream[];
  private readonly config: Config;
  private readonly metrics: DnsMetrics;

  constructor(config: Config, metrics: DnsMetrics) {
    this.config = config;
    this.metrics = metrics;
    this.cache = new DnsCache(config.DNS_CACHE_MAX_ENTRIES, config.DNS_CACHE_MAX_TTL_SECONDS);
    this.upstreams = parseUpstreams(config.DNS_UPSTREAMS);
  }

  async resolve(query: Buffer): Promise<{ response: Buffer; qname: string; qtype: number; rcode: number; cacheHit: boolean }> {
    const header = decodeDnsHeader(query);
    const question = decodeFirstQuestion(query);
    const cacheKey = makeCacheKey({ name: question.name, type: question.type, class: question.class });
    const qtypeLabel = qtypeToString(question.type);

    if (!this.config.DNS_ALLOW_ANY && question.type === 255) {
      throw new Error('ANY queries are disabled');
    }

    if (!this.config.DNS_ALLOW_PRIVATE_PTR && question.type === 12 && isPrivatePtrQname(question.name)) {
      throw new Error('PTR queries to private ranges are disabled');
    }

    const cached = this.cache.get(cacheKey);
    if (cached) {
      const patched = patchDnsId(cached, header.id);
      const cachedHeader = decodeDnsHeader(patched);
      const rcode = getRcodeFromFlags(cachedHeader.flags);

      this.metrics.dnsCacheHitsTotal.inc({ qtype: qtypeLabel });
      this.metrics.dnsQueriesTotal.inc({ qtype: qtypeLabel, rcode: rcodeToString(rcode), source: 'cache' });
      return { response: patched, qname: question.name, qtype: question.type, rcode, cacheHit: true };
    }

    this.metrics.dnsCacheMissesTotal.inc({ qtype: qtypeLabel });

    let lastError: unknown = null;
    for (const upstream of this.upstreams) {
      const start = performance.now();
      try {
        const response =
          upstream.kind === 'udp'
            ? await queryUdpUpstream(upstream, query, this.config.DNS_UPSTREAM_TIMEOUT_MS)
            : await queryDohUpstream(upstream, query, this.config.DNS_UPSTREAM_TIMEOUT_MS);

        const durationSeconds = (performance.now() - start) / 1000;
        this.metrics.dnsUpstreamLatencySeconds.observe({ upstream: upstream.label, kind: upstream.kind }, durationSeconds);

        if (response.length > this.config.DNS_MAX_RESPONSE_BYTES) {
          throw new Error(`Upstream response too large: ${response.length} bytes`);
        }

        const cacheInfo = extractCacheInfoFromResponse(response, this.config.DNS_CACHE_NEGATIVE_TTL_SECONDS);
        if (cacheInfo && cacheInfo.ttlSeconds > 0) {
          this.cache.set(cacheKey, response, cacheInfo.ttlSeconds);
        }

        const patched = patchDnsId(response, header.id);
        const responseHeader = decodeDnsHeader(patched);
        const rcode = getRcodeFromFlags(responseHeader.flags);
        this.metrics.dnsQueriesTotal.inc({ qtype: qtypeLabel, rcode: rcodeToString(rcode), source: 'upstream' });
        return { response: patched, qname: question.name, qtype: question.type, rcode, cacheHit: false };
      } catch (err) {
        const durationSeconds = (performance.now() - start) / 1000;
        this.metrics.dnsUpstreamLatencySeconds.observe({ upstream: upstream.label, kind: upstream.kind }, durationSeconds);
        this.metrics.dnsUpstreamErrorsTotal.inc({ upstream: upstream.label, kind: upstream.kind });
        lastError = err;
      }
    }

    throw (lastError instanceof Error ? lastError : new Error('All upstreams failed'));
  }
}
