#!/usr/bin/env node
import { randomInt } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { formatOneLineError, formatOneLineUtf8 } from '../../src/text.js';

/**
 * Range Harness
 *
 * A small CLI to probe/benchmark HTTP Range requests against large disk-image
 * URLs (S3/CloudFront/etc). Designed to be dependency-free and run on the repo's pinned Node version.
 *
 * Usage:
 *   node tools/range-harness/index.js --url <URL> [--chunk-size 1048576] [--count 32]
 *     [--concurrency 4] [--random|--sequential]
 */

if (typeof fetch !== 'function') {
  // eslint-disable-next-line no-console
  console.error('This tool requires Node.js with global fetch (Node 18+).');
  process.exit(1);
}

const DEFAULT_CHUNK_SIZE_BYTES = 1024 * 1024;
const DEFAULT_COUNT = 32;
const DEFAULT_CONCURRENCY = 4;
const DEFAULT_PASSES = 1;
const DEFAULT_ACCEPT_ENCODING = 'identity';
// Browsers automatically send a non-identity Accept-Encoding (and scripts cannot override it).
// Use this when you want to detect CDN/object-store compression behavior that would affect real
// browser disk streaming clients.
const BROWSER_ACCEPT_ENCODING = 'gzip, deflate, br, zstd';

const MAX_LOG_ERROR_MESSAGE_BYTES = 512;

function printUsage(exitCode = 0) {
  const lines = [
    'Usage:',
    `  node tools/range-harness/index.js --url <URL> [--chunk-size ${DEFAULT_CHUNK_SIZE_BYTES}] [--count ${DEFAULT_COUNT}] [--concurrency ${DEFAULT_CONCURRENCY}] [--random|--sequential]`,
    '',
    'Options:',
    '  --url <URL>            (required) HTTP/HTTPS URL to the disk image',
    `  --chunk-size <bytes>   Size of each Range request (default: ${DEFAULT_CHUNK_SIZE_BYTES} = 1MiB)`,
    `  --count <N>            Number of range requests to perform (default: ${DEFAULT_COUNT})`,
    `  --concurrency <N>      Number of in-flight requests (default: ${DEFAULT_CONCURRENCY})`,
    `  --passes <N>           Repeat the same range plan N times (default: ${DEFAULT_PASSES}; useful for cache hit verification)`,
    '  --seed <N>             Seed for deterministic random ranges (only affects --random)',
    '  --unique               Avoid requesting the same chunk multiple times per pass (only affects --random)',
    `  --accept-encoding <v>  Set Accept-Encoding (default: ${DEFAULT_ACCEPT_ENCODING}; use "browser" for ${BROWSER_ACCEPT_ENCODING})`,
    '  --browser-accept-encoding  Shorthand for --accept-encoding browser',
    '  --header <k:v>         Extra request header (repeatable), e.g. --header \"Authorization: Bearer ...\"',
    '  --json                 Emit machine-readable JSON (suppresses human-readable logs)',
    '  --strict               Exit non-zero if any request fails correctness checks',
    '  --random               Pick random aligned chunks',
    '  --sequential           Walk aligned chunks from the start (wraps around)',
    '  --help                 Show this help',
  ];
  // eslint-disable-next-line no-console
  console.log(lines.join('\n'));
  process.exit(exitCode);
}

function parsePositiveInt(name, value) {
  if (value == null) {
    throw new Error(`Missing value for ${name}`);
  }
  if (!/^\d+$/.test(value)) {
    throw new Error(`Invalid ${name}: ${value} (expected a positive integer)`);
  }
  const n = Number(value);
  if (!Number.isSafeInteger(n) || n <= 0) {
    throw new Error(`Invalid ${name}: ${value} (expected a positive integer)`);
  }
  return n;
}

function parseNonNegativeInt(name, value) {
  if (value == null) {
    throw new Error(`Missing value for ${name}`);
  }
  if (!/^\d+$/.test(value)) {
    throw new Error(`Invalid ${name}: ${value} (expected a non-negative integer)`);
  }
  const n = Number(value);
  if (!Number.isSafeInteger(n) || n < 0) {
    throw new Error(`Invalid ${name}: ${value} (expected a non-negative integer)`);
  }
  return n;
}

function parseArgs(argv) {
  /** @type {Record<string, any>} */
  const opts = {
    url: null,
    chunkSize: DEFAULT_CHUNK_SIZE_BYTES,
    count: DEFAULT_COUNT,
    concurrency: DEFAULT_CONCURRENCY,
    mode: 'sequential',
    headers: {},
    passes: DEFAULT_PASSES,
    seed: null,
    unique: false,
    json: false,
    strict: false,
    acceptEncoding: DEFAULT_ACCEPT_ENCODING,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--help' || arg === '-h') {
      printUsage(0);
    } else if (arg === '--url') {
      opts.url = argv[++i] ?? null;
    } else if (arg === '--chunk-size') {
      opts.chunkSize = parsePositiveInt('--chunk-size', argv[++i]);
    } else if (arg === '--count') {
      opts.count = parsePositiveInt('--count', argv[++i]);
    } else if (arg === '--concurrency') {
      opts.concurrency = parsePositiveInt('--concurrency', argv[++i]);
    } else if (arg === '--passes') {
      opts.passes = parsePositiveInt('--passes', argv[++i]);
    } else if (arg === '--seed') {
      opts.seed = parseNonNegativeInt('--seed', argv[++i]);
    } else if (arg === '--unique') {
      opts.unique = true;
    } else if (arg === '--accept-encoding') {
      const raw = argv[++i];
      if (raw == null) {
        throw new Error('Missing value for --accept-encoding');
      }
      const trimmed = String(raw).trim();
      if (!trimmed) {
        throw new Error('Invalid --accept-encoding (empty)');
      }
      opts.acceptEncoding = trimmed.toLowerCase() === 'browser' ? BROWSER_ACCEPT_ENCODING : trimmed;
    } else if (arg === '--browser-accept-encoding') {
      opts.acceptEncoding = BROWSER_ACCEPT_ENCODING;
    } else if (arg === '--header') {
      const raw = argv[++i];
      if (raw == null) {
        throw new Error('Missing value for --header');
      }
      const idx = raw.indexOf(':');
      if (idx === -1) {
        throw new Error(`Invalid --header ${JSON.stringify(raw)} (expected \"Name: value\")`);
      }
      const name = raw.slice(0, idx).trim();
      const value = raw.slice(idx + 1).trim();
      if (!name) {
        throw new Error(`Invalid --header ${JSON.stringify(raw)} (empty header name)`);
      }
      opts.headers[name] = value;
    } else if (arg === '--json') {
      opts.json = true;
    } else if (arg === '--strict') {
      opts.strict = true;
    } else if (arg === '--random') {
      opts.mode = 'random';
    } else if (arg === '--sequential') {
      opts.mode = 'sequential';
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  if (!opts.url) {
    throw new Error('--url is required');
  }

  opts.concurrency = Math.min(opts.concurrency, opts.count);
  return opts;
}

function nowNs() {
  return process.hrtime.bigint();
}

function nsToMs(ns) {
  return Number(ns) / 1e6;
}

function formatMs(ms) {
  if (!Number.isFinite(ms)) return 'n/a';
  if (ms < 1) return `${ms.toFixed(2)}ms`;
  if (ms < 100) return `${ms.toFixed(1)}ms`;
  return `${ms.toFixed(0)}ms`;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes)) return 'n/a';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const digits = v < 10 && i > 0 ? 2 : v < 100 && i > 0 ? 1 : 0;
  return `${v.toFixed(digits)}${units[i]}`;
}

function formatRate(bytesPerSecond) {
  if (!Number.isFinite(bytesPerSecond)) return 'n/a';
  return `${formatBytes(bytesPerSecond)}/s`;
}

function median(values) {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) return sorted[mid];
  return (sorted[mid - 1] + sorted[mid]) / 2;
}

function classifyXCache(value) {
  if (!value) return 'missing';
  if (value.length > 1024) return 'other';
  const lower = value.toLowerCase();
  if (lower.includes('miss')) return 'miss';
  if (lower.includes('hit')) return 'hit';
  return 'other';
}

const MAX_CONTENT_RANGE_HEADER_VALUE_LEN = 256;
const MAX_CONTENT_ENCODING_HEADER_VALUE_LEN = 256;
const MAX_CONTENT_LENGTH_HEADER_VALUE_LEN = 32;
const MAX_ETAG_HEADER_VALUE_LEN = 4 * 1024;
const MAX_ACCEPT_RANGES_HEADER_VALUE_LEN = 256;
const MAX_X_CACHE_HEADER_VALUE_LEN = 1024;

function formatHeaderValueForLog(value, maxLen, missing = '(missing)') {
  if (!value) return missing;
  if (value.length <= maxLen) return value;
  return `${value.slice(0, maxLen)}...(truncated)`;
}

function getHeaderBounded(headers, name, maxLen) {
  const value = headers.get(name);
  if (!value) return null;
  if (value.length > maxLen) return null;
  return value;
}

function parseSafeDecimal(s) {
  // `\d+` from regex ensures ASCII digits only, but still bound size and safe-int range.
  if (!s || s.length > 16) return null;
  const n = Number(s);
  if (!Number.isSafeInteger(n) || n < 0) return null;
  return n;
}

export function parseContentRange(value) {
  if (!value) return null;
  if (value.length > MAX_CONTENT_RANGE_HEADER_VALUE_LEN) return null;
  const trimmed = value.trim();
  if (trimmed.length > MAX_CONTENT_RANGE_HEADER_VALUE_LEN) return null;
  // Examples:
  //   bytes 0-1023/1048576
  //   bytes 0-1023/*
  //   bytes */1048576 (for 416)
  let m = /^bytes\s+(\d+)-(\d+)\/(\d+|\*)$/i.exec(trimmed);
  if (m) {
    const start = parseSafeDecimal(m[1]);
    const end = parseSafeDecimal(m[2]);
    const total = m[3] === '*' ? null : parseSafeDecimal(m[3]);
    if (start == null || end == null) return null;
    if (m[3] !== '*' && total == null) return null;
    return {
      unit: 'bytes',
      start,
      end,
      total,
      isUnsatisfied: false,
    };
  }
  m = /^bytes\s+\*\/(\d+)$/i.exec(trimmed);
  if (m) {
    const total = parseSafeDecimal(m[1]);
    if (total == null) return null;
    return {
      unit: 'bytes',
      start: null,
      end: null,
      total,
      isUnsatisfied: true,
    };
  }
  return null;
}

function makeMulberry32(seed) {
  // Deterministic PRNG for reproducible random range plans across runs.
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function buildBaseHeaders(extraHeaders, acceptEncoding) {
  return {
    ...(extraHeaders ?? {}),
    // Accept-Encoding is controlled by the UA in browsers. Default to identity for more stable
    // throughput measurements, but allow browser-like values to detect transforms.
    'Accept-Encoding': acceptEncoding,
  };
}

async function readBodyAndCount(body, { byteLimit, abortController }) {
  if (!body) return { bytes: 0, abortedEarly: false };
  let bytes = 0;
  let abortedEarly = false;

  // Node's fetch() (Undici) returns a WHATWG ReadableStream. Prefer the reader
  // API (available in Node 18+) over `for await` in case async-iterability
  // differs across Node versions.
  if (typeof body.getReader === 'function') {
    const reader = body.getReader();
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) bytes += value.byteLength ?? value.length ?? 0;
        if (byteLimit != null && bytes >= byteLimit) {
          abortedEarly = true;
          abortController?.abort();
          try {
            await reader.cancel();
          } catch {
            // ignore
          }
          break;
        }
      }
    } catch (err) {
      // If we abort a response to avoid downloading the full object (e.g. server
      // ignored Range and returned 200), the stream will typically error. That is
      // expected; we still want to report what we read.
      if (abortController?.signal?.aborted) {
        abortedEarly = true;
      } else {
        throw err;
      }
    } finally {
      try {
        reader.releaseLock();
      } catch {
        // ignore
      }
    }
    return { bytes, abortedEarly };
  }

  try {
    for await (const chunk of body) {
      bytes += chunk?.byteLength ?? chunk?.length ?? 0;
      if (byteLimit != null && bytes >= byteLimit) {
        abortedEarly = true;
        abortController?.abort();
        break;
      }
    }
  } catch (err) {
    // If we abort a response to avoid downloading the full object (e.g. server
    // ignored Range and returned 200), the stream will typically error. That is
    // expected; we still want to report what we read.
    if (abortController?.signal?.aborted) {
      abortedEarly = true;
    } else {
      throw err;
    }
  }
  return { bytes, abortedEarly };
}

async function getResourceInfo(url, extraHeaders, acceptEncoding) {
  const headers = buildBaseHeaders(extraHeaders, acceptEncoding);

  let headRes;
  try {
    headRes = await fetch(url, { method: 'HEAD', headers });
  } catch (err) {
    headRes = null;
  }

  let etag = headRes?.headers ? getHeaderBounded(headRes.headers, 'etag', MAX_ETAG_HEADER_VALUE_LEN) : null;
  const contentLengthRaw = headRes?.headers?.get('content-length') ?? null;
  const contentLength =
    contentLengthRaw && contentLengthRaw.length <= MAX_CONTENT_LENGTH_HEADER_VALUE_LEN ? contentLengthRaw : null;
  let acceptRanges = headRes?.headers ? getHeaderBounded(headRes.headers, 'accept-ranges', MAX_ACCEPT_RANGES_HEADER_VALUE_LEN) : null;

  if (headRes && headRes.ok && contentLength && /^\d+$/.test(contentLength)) {
    return {
      size: Number(contentLength),
      etag,
      acceptRanges,
      headStatus: headRes.status,
      headOk: true,
      usedFallback: false,
    };
  }

  // Fallback: ask for 1 byte so we can parse Content-Range: bytes 0-0/total.
  const controller = new AbortController();
  const rangeHeaders = {
    ...headers,
    Range: 'bytes=0-0',
  };

  const res = await fetch(url, { method: 'GET', headers: rangeHeaders, signal: controller.signal });
  etag = getHeaderBounded(res.headers, 'etag', MAX_ETAG_HEADER_VALUE_LEN) ?? etag;
  acceptRanges = getHeaderBounded(res.headers, 'accept-ranges', MAX_ACCEPT_RANGES_HEADER_VALUE_LEN) ?? acceptRanges;
  const contentRange = res.headers.get('content-range');
  const parsed = parseContentRange(contentRange);
  const resContentLengthRaw = res.headers.get('content-length');
  const resContentLength =
    resContentLengthRaw && resContentLengthRaw.length <= MAX_CONTENT_LENGTH_HEADER_VALUE_LEN ? resContentLengthRaw : null;

  // Avoid downloading the full object if the server ignores our range.
  await readBodyAndCount(res.body, { byteLimit: 1, abortController: controller });

  if (parsed && parsed.total != null) {
    return {
      size: parsed.total,
      etag,
      acceptRanges,
      headStatus: headRes?.status ?? null,
      headOk: Boolean(headRes?.ok),
      usedFallback: true,
    };
  }

  if (resContentLength && /^\d+$/.test(resContentLength)) {
    return {
      size: Number(resContentLength),
      etag,
      acceptRanges,
      headStatus: headRes?.status ?? null,
      headOk: Boolean(headRes?.ok),
      usedFallback: true,
    };
  }

  const headStatusStr = headRes ? `${headRes.status}` : 'n/a';
  throw new Error(
    `Unable to determine Content-Length. HEAD status=${headStatusStr} ` +
      `content-length=${formatHeaderValueForLog(contentLengthRaw, MAX_CONTENT_LENGTH_HEADER_VALUE_LEN, 'n/a')}; range probe status=${res.status} ` +
      `content-range=${formatHeaderValueForLog(contentRange, MAX_CONTENT_RANGE_HEADER_VALUE_LEN, 'n/a')} content-length=${
        formatHeaderValueForLog(resContentLengthRaw, MAX_CONTENT_LENGTH_HEADER_VALUE_LEN, 'n/a')
      }`,
  );
}

export function buildPlan({ size, chunkSize, count, mode, seed, unique }) {
  const chunks = Math.max(1, Math.ceil(size / chunkSize));
  const rng = seed == null ? null : makeMulberry32(seed);
  const plan = [];
  const pickIndex = () => (rng ? Math.floor(rng() * chunks) : randomInt(0, chunks));

  const used = mode === 'random' && unique ? new Set() : null;

  for (let i = 0; i < count; i++) {
    let chunkIndex;
    if (mode === 'random') {
      if (used) {
        // Keep ranges unique within a pass. If we exhaust the chunk space (very
        // small objects or very large counts), start a new cycle.
        if (used.size === chunks) used.clear();

        let attempts = 0;
        do {
          chunkIndex = pickIndex();
          attempts++;
        } while (used.has(chunkIndex) && attempts < 10000);

        if (used.has(chunkIndex)) {
          // Extremely unlikely unless count≈chunks and RNG keeps colliding.
          // Fall back to a linear scan for the next unused index.
          for (let j = 0; j < chunks; j++) {
            if (!used.has(j)) {
              chunkIndex = j;
              break;
            }
          }
        }

        used.add(chunkIndex);
      } else {
        chunkIndex = pickIndex();
      }
    } else {
      chunkIndex = i % chunks;
    }

    const start = chunkIndex * chunkSize;
    const end = Math.min(start + chunkSize - 1, size - 1);
    plan.push({ index: i, start, end });
  }
  return plan;
}

async function runPool(items, concurrency, workerFn) {
  const results = new Array(items.length);
  let next = 0;
  const workerCount = Math.min(concurrency, items.length);
  const workers = Array.from({ length: workerCount }, async () => {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const idx = next++;
      if (idx >= items.length) return;
      results[idx] = await workerFn(items[idx]);
    }
  });
  await Promise.all(workers);
  return results;
}

function padLeft(str, width) {
  const s = String(str);
  if (s.length >= width) return s;
  return ' '.repeat(width - s.length) + s;
}

function computeStats(results, startedNs, finishedNs) {
  const latencies = results.map((r) => r.latencyMs).filter((v) => Number.isFinite(v));
  const avgLatency = latencies.reduce((a, b) => a + b, 0) / Math.max(1, latencies.length);
  const medLatency = median(latencies);

  const totalBytes = results.reduce((sum, r) => sum + (r.bytes ?? 0), 0);
  const wallTimeSec = startedNs ? Number(finishedNs - startedNs) / 1e9 : NaN;
  const aggRate = wallTimeSec > 0 ? totalBytes / wallTimeSec : NaN;

  const statusCounts = new Map();
  const exactXCacheCounts = new Map();
  const xCacheClassCounts = new Map();
  let okCount = 0;
  let warnCount = 0;

  for (const r of results) {
    const statusKey = r.status == null ? 'ERR' : String(r.status);
    statusCounts.set(statusKey, (statusCounts.get(statusKey) ?? 0) + 1);

    const xCacheKey = r.xCache ?? '(missing)';
    exactXCacheCounts.set(xCacheKey, (exactXCacheCounts.get(xCacheKey) ?? 0) + 1);

    const cls = classifyXCache(r.xCache);
    xCacheClassCounts.set(cls, (xCacheClassCounts.get(cls) ?? 0) + 1);

    if (r.ok) okCount++;
    if (r.warnings && r.warnings.length) warnCount++;
  }

  const statusParts = [...statusCounts.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([k, v]) => `${k}:${v}`);

  const hit = xCacheClassCounts.get('hit') ?? 0;
  const miss = xCacheClassCounts.get('miss') ?? 0;
  const other = xCacheClassCounts.get('other') ?? 0;
  const missing = xCacheClassCounts.get('missing') ?? 0;

  return {
    okCount,
    warnCount,
    avgLatency,
    medLatency,
    totalBytes,
    wallTimeSec,
    aggRate,
    statusCounts,
    statusParts,
    xCache: {
      hit,
      miss,
      other,
      missing,
      exactXCacheCounts,
    },
  };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));

  const log = (...args) => {
    if (!opts.json) {
      // eslint-disable-next-line no-console
      console.log(...args);
    }
  };

  log(`URL: ${opts.url}`);
  log(
    `Config: chunkSize=${formatBytes(opts.chunkSize)} count=${opts.count} concurrency=${opts.concurrency} passes=${opts.passes} seed=${
      opts.seed ?? '(random)'
    } unique=${opts.unique} mode=${opts.mode} acceptEncoding=${JSON.stringify(opts.acceptEncoding)}`,
  );

  const info = await getResourceInfo(opts.url, opts.headers, opts.acceptEncoding);
  log(`HEAD: status=${info.headStatus ?? 'n/a'} ok=${info.headOk} usedFallback=${info.usedFallback}`);
  log(
    `Resource: size=${formatBytes(info.size)} (${info.size} bytes) etag=${formatHeaderValueForLog(
      info.etag,
      MAX_ETAG_HEADER_VALUE_LEN,
    )} accept-ranges=${formatHeaderValueForLog(info.acceptRanges, MAX_ACCEPT_RANGES_HEADER_VALUE_LEN)}`,
  );

  const plan = buildPlan({
    size: info.size,
    chunkSize: opts.chunkSize,
    count: opts.count,
    mode: opts.mode,
    seed: opts.seed,
    unique: opts.unique,
  });

  let warned200 = false;

  let startedNs = null;
  let finishedNs = null;

  /** @type {any[]} */
  const allResults = [];
  /** @type {any[]} */
  const passSummaries = [];

  for (let pass = 0; pass < opts.passes; pass++) {
    let passStartedNs = null;
    let passFinishedNs = null;

    const passResults = await runPool(
      plan.map((t) => ({ ...t, pass: pass + 1 })),
      opts.concurrency,
      async (task) => {
        const expectedLen = task.end - task.start + 1;
        const rangeValue = `bytes=${task.start}-${task.end}`;
        const controller = new AbortController();

        const label =
          opts.passes === 1 ? `[${padLeft(task.index + 1, 2)}]` : `[p${task.pass}/${opts.passes} ${padLeft(task.index + 1, 2)}]`;

        const startNs = nowNs();
        startedNs = startedNs ?? startNs;
        passStartedNs = passStartedNs ?? startNs;
        /** @type {any} */
        let response;
        /** @type {string|null} */
        let fetchError = null;
        try {
          response = await fetch(opts.url, {
            method: 'GET',
            headers: {
              ...buildBaseHeaders(opts.headers, opts.acceptEncoding),
              Range: rangeValue,
            },
            signal: controller.signal,
          });
        } catch (err) {
          fetchError = formatOneLineError(err, MAX_LOG_ERROR_MESSAGE_BYTES, 'fetch failed');
          const endNs = nowNs();
          passFinishedNs = endNs;
          finishedNs = endNs;
          const latencyMs = nsToMs(endNs - startNs);
          log(`${label} ${rangeValue} status=ERR bytes=0 time=${formatMs(latencyMs)} error=${fetchError}`);
          return {
            ...task,
            expectedLen,
            status: null,
            bytes: 0,
            latencyMs,
            contentRange: null,
            xCache: null,
            ok: false,
            warnings: [`fetch error: ${fetchError}`],
          };
        }

        const status = response.status;
        const contentRangeHeaderRaw = response.headers.get('content-range');
        const contentRangeHeader =
          contentRangeHeaderRaw && contentRangeHeaderRaw.length <= MAX_CONTENT_RANGE_HEADER_VALUE_LEN ? contentRangeHeaderRaw : null;
        const xCacheRaw = response.headers.get('x-cache');
        const xCache = xCacheRaw && xCacheRaw.length <= MAX_X_CACHE_HEADER_VALUE_LEN ? xCacheRaw : null;
        const resContentLength = response.headers.get('content-length');
        const contentEncodingRaw = response.headers.get('content-encoding');
        const contentEncoding =
          contentEncodingRaw && contentEncodingRaw.length <= MAX_CONTENT_ENCODING_HEADER_VALUE_LEN ? contentEncodingRaw : null;

        // If the server ignored our Range and returns 200, avoid pulling an entire
        // disk image into memory by aborting after expectedLen bytes. This still
        // provides useful signal (it will likely be "slow" and show no Content-Range).
        let byteLimit = null;
        const contentLengthNum = resContentLength && /^\d+$/.test(resContentLength) ? Number(resContentLength) : null;
        if (status === 200 && expectedLen < info.size && (contentLengthNum == null || contentLengthNum > expectedLen)) {
          byteLimit = expectedLen;
        }

        const { bytes, abortedEarly } = await readBodyAndCount(response.body, { byteLimit, abortController: controller });

        const endNs = nowNs();
        passFinishedNs = endNs;
        finishedNs = endNs;
        const latencyMs = nsToMs(endNs - startNs);

        const warnings = [];
        let ok = true;

        const contentEncodingTrimmed = contentEncoding ? contentEncoding.trim() : null;
        if (contentEncodingTrimmed && contentEncodingTrimmed.toLowerCase() !== 'identity') {
          ok = false;
          warnings.push(
            `unexpected Content-Encoding: ${formatHeaderValueForLog(contentEncodingTrimmed, MAX_CONTENT_ENCODING_HEADER_VALUE_LEN)}`,
          );
        }

        if (status === 206) {
          const parsed = parseContentRange(contentRangeHeader);

          if (!parsed) {
            ok = false;
            warnings.push(
              `invalid Content-Range: ${formatHeaderValueForLog(contentRangeHeaderRaw, MAX_CONTENT_RANGE_HEADER_VALUE_LEN)}`,
            );
          } else {
            if (parsed.isUnsatisfied || parsed.start == null || parsed.end == null) {
              ok = false;
              warnings.push(
                `unexpected Content-Range for 206: ${formatHeaderValueForLog(
                  contentRangeHeaderRaw,
                  MAX_CONTENT_RANGE_HEADER_VALUE_LEN,
                )}`,
              );
            }

            if (
              parsed.start != null &&
              parsed.end != null &&
              (parsed.start !== task.start || parsed.end !== task.end)
            ) {
              ok = false;
              warnings.push(
                `Content-Range mismatch (got ${parsed.start}-${parsed.end}, expected ${task.start}-${task.end})`,
              );
            }

            if (parsed.total != null && parsed.total !== info.size) {
              ok = false;
              warnings.push(`Content-Range total differs from HEAD (${parsed.total} vs ${info.size})`);
            }
          }

          if (bytes !== expectedLen) {
            ok = false;
            warnings.push(`body bytes (${bytes}) != expected (${expectedLen})`);
          }
        } else if (status === 200) {
          ok = false;
          if (!warned200) {
            warned200 = true;
            warnings.push(
              'server returned 200 (Range ignored?). Results are still shown, but throughput/latency may not be meaningful.',
            );
          } else {
            warnings.push('server returned 200 (Range ignored?)');
          }
          if (abortedEarly) {
            warnings.push(`aborted after ${bytes} bytes to avoid downloading the full object`);
          }
        } else if (status === 416) {
          ok = false;
          const parsed = parseContentRange(contentRangeHeader);
          if (parsed?.isUnsatisfied && parsed.total != null) {
            warnings.push(`416 Range Not Satisfiable (server reports size=${parsed.total})`);
          } else {
            warnings.push(`416 Range Not Satisfiable`);
          }
        } else {
          ok = false;
          warnings.push(`unexpected status ${status}`);
        }

        const perReqRate = latencyMs > 0 ? (bytes / (latencyMs / 1000)) : NaN;

        log(
          `${label} ${rangeValue} status=${status} bytes=${bytes} time=${formatMs(latencyMs)} rate=${formatRate(
            perReqRate,
          )} content-range=${formatHeaderValueForLog(
            contentRangeHeaderRaw,
            MAX_CONTENT_RANGE_HEADER_VALUE_LEN,
          )} x-cache=${formatHeaderValueForLog(xCacheRaw, MAX_X_CACHE_HEADER_VALUE_LEN)}${
            warnings.length ? ` WARN=${warnings[0]}` : ''
          }`,
        );

        return {
          ...task,
          expectedLen,
          status,
          bytes,
          latencyMs,
          contentRange: contentRangeHeader,
          xCache,
          ok,
          warnings,
        };
      },
    );

    passFinishedNs = passFinishedNs ?? nowNs();
    passStartedNs = passStartedNs ?? passFinishedNs;

    allResults.push(...passResults);

    const passStats = computeStats(passResults, passStartedNs, passFinishedNs);
    passSummaries.push({
      pass: pass + 1,
      ...passStats,
    });

    if (opts.passes > 1) {
      log(`\nPass ${pass + 1}/${opts.passes}`);
      log('--------------');
      log(`Requests: ${passResults.length} ok=${passStats.okCount} withWarnings=${passStats.warnCount}`);
      log(`Latency: avg=${formatMs(passStats.avgLatency)} median=${formatMs(passStats.medLatency)}`);
      log(
        `Throughput: bytes=${formatBytes(passStats.totalBytes)} wall=${passStats.wallTimeSec.toFixed(
          2,
        )}s aggregate=${formatRate(passStats.aggRate)}`,
      );
      log(`Status codes: ${passStats.statusParts.join(' ')}`);
      log(
        `X-Cache: hit=${passStats.xCache.hit} miss=${passStats.xCache.miss} other=${passStats.xCache.other} missing=${passStats.xCache.missing}`,
      );
    }
  }

  finishedNs = finishedNs ?? nowNs();

  const overallStats = computeStats(allResults, startedNs, finishedNs);

  log(`\nSummary`);
  log('-------');
  log(`Requests: ${allResults.length} ok=${overallStats.okCount} withWarnings=${overallStats.warnCount}`);
  log(`Latency: avg=${formatMs(overallStats.avgLatency)} median=${formatMs(overallStats.medLatency)}`);
  log(
    `Throughput: bytes=${formatBytes(overallStats.totalBytes)} wall=${overallStats.wallTimeSec.toFixed(2)}s aggregate=${formatRate(
      overallStats.aggRate,
    )}`,
  );
  log(`Status codes: ${overallStats.statusParts.join(' ')}`);
  log(
    `X-Cache: hit=${overallStats.xCache.hit} miss=${overallStats.xCache.miss} other=${overallStats.xCache.other} missing=${overallStats.xCache.missing}`,
  );

  if (!opts.json && overallStats.xCache.exactXCacheCounts.size > 0) {
    log('X-Cache breakdown:');
    for (const [k, v] of [...overallStats.xCache.exactXCacheCounts.entries()].sort((a, b) => b[1] - a[1])) {
      log(`  ${padLeft(v, 3)}  ${k}`);
    }
  }

  if (opts.json) {
    // eslint-disable-next-line no-console
    console.log(
      JSON.stringify(
        {
          url: opts.url,
          config: {
            chunkSize: opts.chunkSize,
            count: opts.count,
            concurrency: opts.concurrency,
            passes: opts.passes,
            seed: opts.seed,
            unique: opts.unique,
            mode: opts.mode,
            headers: opts.headers,
            acceptEncoding: opts.acceptEncoding,
          },
          head: {
            status: info.headStatus,
            ok: info.headOk,
            usedFallback: info.usedFallback,
          },
          resource: {
            size: info.size,
            etag: info.etag,
            acceptRanges: info.acceptRanges,
          },
          passes: passSummaries.map((p) => ({
            pass: p.pass,
            requests: opts.count,
            ok: p.okCount,
            withWarnings: p.warnCount,
            latencyMs: {
              avg: p.avgLatency,
              median: p.medLatency,
            },
            throughput: {
              bytes: p.totalBytes,
              wallTimeSec: p.wallTimeSec,
              aggregateBytesPerSec: p.aggRate,
            },
            statusCounts: Object.fromEntries(p.statusCounts.entries()),
            xCache: {
              hit: p.xCache.hit,
              miss: p.xCache.miss,
              other: p.xCache.other,
              missing: p.xCache.missing,
              exact: Object.fromEntries(p.xCache.exactXCacheCounts.entries()),
            },
          })),
          summary: {
            requests: allResults.length,
            ok: overallStats.okCount,
            withWarnings: overallStats.warnCount,
            latencyMs: {
              avg: overallStats.avgLatency,
              median: overallStats.medLatency,
            },
            throughput: {
              bytes: overallStats.totalBytes,
              wallTimeSec: overallStats.wallTimeSec,
              aggregateBytesPerSec: overallStats.aggRate,
            },
            statusCounts: Object.fromEntries(overallStats.statusCounts.entries()),
            xCache: {
              hit: overallStats.xCache.hit,
              miss: overallStats.xCache.miss,
              other: overallStats.xCache.other,
              missing: overallStats.xCache.missing,
              exact: Object.fromEntries(overallStats.xCache.exactXCacheCounts.entries()),
            },
          },
          results: allResults,
        },
        null,
        2,
      ),
    );
  }

  if (opts.strict && overallStats.okCount !== allResults.length) {
    process.exitCode = 1;
  }
}

if (fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? '')) {
  main().catch((err) => {
    // eslint-disable-next-line no-console
    console.error(err && typeof err === 'object' && 'stack' in err ? err.stack : err);
    process.exit(1);
  });
}
