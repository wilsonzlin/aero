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

function isPrivateIpv4(a: number, b: number): boolean {
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
  let allZero = true;
  for (let i = 0; i < bytes.length; i += 1) {
    if (bytes[i] !== 0) {
      allZero = false;
      break;
    }
  }
  if (allZero) return true;

  let loopback = true;
  for (let i = 0; i < 15; i += 1) {
    if (bytes[i] !== 0) {
      loopback = false;
      break;
    }
  }
  if (loopback && bytes[15] === 1) return true;
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
    const parsed = parseInAddrArpaIPv4(base);
    if (!parsed) return false;
    return isPrivateIpv4(parsed.a, parsed.b);
  }

  if (asciiEndsWithIgnoreCase(qname, '.ip6.arpa')) {
    const base = qname.slice(0, -'.ip6.arpa'.length);
    const bytes = parseIp6ArpaBytes(base);
    if (!bytes) return false;
    return isPrivateIpv6(bytes);
  }

  return false;
}

function parseDecimalByteFromAscii(input: string, start: number, end: number): number | null {
  if (end <= start) return null;
  let out = 0;
  for (let i = start; i < end; i += 1) {
    const c = input.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return null;
    out = out * 10 + (c - 0x30);
    if (out > 255) return null;
  }
  return out;
}

function parseInAddrArpaIPv4(base: string): { a: number; b: number } | null {
  // Parse the last 4 dot-delimited labels (ignoring empties) from right to left.
  let i = base.length;
  let labels = 0;
  let a = 0;
  let b = 0;

  while (i > 0) {
    while (i > 0 && base.charCodeAt(i - 1) === 0x2e /* '.' */) i -= 1;
    if (i === 0) break;

    let start = i;
    while (start > 0 && base.charCodeAt(start - 1) !== 0x2e /* '.' */) start -= 1;

    const value = parseDecimalByteFromAscii(base, start, i);
    if (value === null) return null;

    if (labels === 0) a = value;
    else if (labels === 1) b = value;
    labels += 1;
    if (labels > 4) return null;

    i = start - 1;
  }

  if (labels !== 4) return null;
  return { a, b };
}

function hexNibbleValue(code: number): number | null {
  if (code >= 0x30 /* '0' */ && code <= 0x39 /* '9' */) return code - 0x30;
  if (code >= 0x41 /* 'A' */ && code <= 0x46 /* 'F' */) return 10 + (code - 0x41);
  if (code >= 0x61 /* 'a' */ && code <= 0x66 /* 'f' */) return 10 + (code - 0x61);
  return null;
}

function parseIp6ArpaBytes(base: string): Uint8Array | null {
  // Parse 32 dot-delimited nibbles from right to left (ignoring empty labels).
  const bytes = new Uint8Array(16);
  let nibbleIndex = 0;
  let i = base.length;

  while (i > 0) {
    while (i > 0 && base.charCodeAt(i - 1) === 0x2e /* '.' */) i -= 1;
    if (i === 0) break;

    let start = i;
    while (start > 0 && base.charCodeAt(start - 1) !== 0x2e /* '.' */) start -= 1;

    if (i - start !== 1) return null;
    const value = hexNibbleValue(base.charCodeAt(start));
    if (value === null) return null;
    if (nibbleIndex >= 32) return null;

    const byteIndex = nibbleIndex >> 1;
    if ((nibbleIndex & 1) === 0) bytes[byteIndex] = value << 4;
    else bytes[byteIndex] |= value;
    nibbleIndex += 1;

    i = start - 1;
  }

  if (nibbleIndex !== 32) return null;
  return bytes;
}

export type DnsPolicyErrorKind = "any-disabled" | "private-ptr-disabled";

export class DnsPolicyError extends Error {
  readonly kind: DnsPolicyErrorKind;

  constructor(kind: DnsPolicyErrorKind) {
    super(kind === "any-disabled" ? "ANY queries are disabled" : "PTR queries to private ranges are disabled");
    this.kind = kind;
  }
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

  static policyErrorAnyDisabled(): DnsPolicyError {
    return new DnsPolicyError("any-disabled");
  }

  static policyErrorPrivatePtrDisabled(): DnsPolicyError {
    return new DnsPolicyError("private-ptr-disabled");
  }

  async resolve(query: Buffer): Promise<{ response: Buffer; qname: string; qtype: number; rcode: number; cacheHit: boolean }> {
    const header = decodeDnsHeader(query);
    const question = decodeFirstQuestion(query);
    const cacheKey = makeCacheKey({ name: question.name, type: question.type, class: question.class });
    const qtypeLabel = qtypeToString(question.type);

    if (!this.config.DNS_ALLOW_ANY && question.type === 255) {
      throw DnsResolver.policyErrorAnyDisabled();
    }

    if (!this.config.DNS_ALLOW_PRIVATE_PTR && question.type === 12 && isPrivatePtrQname(question.name)) {
      throw DnsResolver.policyErrorPrivatePtrDisabled();
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
