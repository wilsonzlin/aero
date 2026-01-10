#!/usr/bin/env node
import { randomInt } from 'node:crypto';

/**
 * Range Harness
 *
 * A small CLI to probe/benchmark HTTP Range requests against large disk-image
 * URLs (S3/CloudFront/etc). Designed to be dependency-free and run on Node 18+.
 *
 * Usage:
 *   node tools/range-harness/index.js --url <URL> [--chunk-size 8388608] [--count 32]
 *     [--concurrency 4] [--random|--sequential]
 */

if (typeof fetch !== 'function') {
  // eslint-disable-next-line no-console
  console.error('This tool requires Node.js 18+ (global fetch is missing).');
  process.exit(1);
}

function printUsage(exitCode = 0) {
  const lines = [
    'Usage:',
    '  node tools/range-harness/index.js --url <URL> [--chunk-size 8388608] [--count 32] [--concurrency 4] [--random|--sequential]',
    '',
    'Options:',
    '  --url <URL>            (required) HTTP/HTTPS URL to the disk image',
    '  --chunk-size <bytes>   Size of each Range request (default: 8388608 = 8MiB)',
    '  --count <N>            Number of range requests to perform (default: 32)',
    '  --concurrency <N>      Number of in-flight requests (default: 4)',
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

function parseArgs(argv) {
  /** @type {Record<string, any>} */
  const opts = {
    url: null,
    chunkSize: 8 * 1024 * 1024,
    count: 32,
    concurrency: 4,
    mode: 'sequential',
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
  const lower = value.toLowerCase();
  if (lower.includes('miss')) return 'miss';
  if (lower.includes('hit')) return 'hit';
  return 'other';
}

function parseContentRange(value) {
  if (!value) return null;
  const trimmed = value.trim();
  // Examples:
  //   bytes 0-1023/1048576
  //   bytes 0-1023/*
  //   bytes */1048576 (for 416)
  let m = /^bytes\s+(\d+)-(\d+)\/(\d+|\*)$/i.exec(trimmed);
  if (m) {
    return {
      unit: 'bytes',
      start: Number(m[1]),
      end: Number(m[2]),
      total: m[3] === '*' ? null : Number(m[3]),
      isUnsatisfied: false,
    };
  }
  m = /^bytes\s+\*\/(\d+)$/i.exec(trimmed);
  if (m) {
    return {
      unit: 'bytes',
      start: null,
      end: null,
      total: Number(m[1]),
      isUnsatisfied: true,
    };
  }
  return null;
}

async function readBodyAndCount(body, { byteLimit, abortController }) {
  if (!body) return { bytes: 0, abortedEarly: false };
  let bytes = 0;
  let abortedEarly = false;
  try {
    for await (const chunk of body) {
      bytes += chunk.length;
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

async function getResourceInfo(url) {
  const headers = { 'Accept-Encoding': 'identity' };

  let headRes;
  try {
    headRes = await fetch(url, { method: 'HEAD', headers });
  } catch (err) {
    headRes = null;
  }

  let etag = headRes?.headers?.get('etag') ?? null;
  let contentLength = headRes?.headers?.get('content-length') ?? null;
  let acceptRanges = headRes?.headers?.get('accept-ranges') ?? null;

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
  etag = res.headers.get('etag') ?? etag;
  acceptRanges = res.headers.get('accept-ranges') ?? acceptRanges;
  const contentRange = res.headers.get('content-range');
  const parsed = parseContentRange(contentRange);
  const resContentLength = res.headers.get('content-length');

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
      `content-length=${contentLength ?? 'n/a'}; range probe status=${res.status} ` +
      `content-range=${contentRange ?? 'n/a'} content-length=${resContentLength ?? 'n/a'}`,
  );
}

function buildPlan({ size, chunkSize, count, mode }) {
  const chunks = Math.max(1, Math.ceil(size / chunkSize));
  const plan = [];
  for (let i = 0; i < count; i++) {
    const chunkIndex = mode === 'random' ? randomInt(0, chunks) : i % chunks;
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

async function main() {
  const opts = parseArgs(process.argv.slice(2));

  // eslint-disable-next-line no-console
  console.log(`URL: ${opts.url}`);
  // eslint-disable-next-line no-console
  console.log(
    `Config: chunkSize=${formatBytes(opts.chunkSize)} count=${opts.count} concurrency=${opts.concurrency} mode=${opts.mode}`,
  );

  const info = await getResourceInfo(opts.url);
  // eslint-disable-next-line no-console
  console.log(
    `HEAD: status=${info.headStatus ?? 'n/a'} ok=${info.headOk} usedFallback=${info.usedFallback}`,
  );
  // eslint-disable-next-line no-console
  console.log(
    `Resource: size=${formatBytes(info.size)} (${info.size} bytes) etag=${info.etag ?? '(missing)'} accept-ranges=${
      info.acceptRanges ?? '(missing)'
    }`,
  );

  const plan = buildPlan({
    size: info.size,
    chunkSize: opts.chunkSize,
    count: opts.count,
    mode: opts.mode,
  });

  let startedNs = null;
  let finishedNs = null;

  let warned200 = false;

  const results = await runPool(plan, opts.concurrency, async (task) => {
    const expectedLen = task.end - task.start + 1;
    const rangeValue = `bytes=${task.start}-${task.end}`;
    const controller = new AbortController();

    const startNs = nowNs();
    startedNs = startedNs ?? startNs;
    /** @type {any} */
    let response;
    /** @type {string|null} */
    let fetchError = null;
    try {
      response = await fetch(opts.url, {
        method: 'GET',
        headers: {
          Range: rangeValue,
          'Accept-Encoding': 'identity',
        },
        signal: controller.signal,
      });
    } catch (err) {
      fetchError = err && typeof err === 'object' && 'message' in err ? String(err.message) : String(err);
      const endNs = nowNs();
      const latencyMs = nsToMs(endNs - startNs);
      // eslint-disable-next-line no-console
      console.log(
        `[${padLeft(task.index + 1, 2)}] ${rangeValue} status=ERR bytes=0 time=${formatMs(latencyMs)} error=${fetchError}`,
      );
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
    const contentRangeHeader = response.headers.get('content-range');
    const xCache = response.headers.get('x-cache');
    const resContentLength = response.headers.get('content-length');

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
    finishedNs = endNs;
    const latencyMs = nsToMs(endNs - startNs);

    const warnings = [];
    let ok = true;

    if (status === 206) {
      const parsed = parseContentRange(contentRangeHeader);
      if (!parsed || parsed.isUnsatisfied || parsed.start == null || parsed.end == null) {
        ok = false;
        warnings.push(`invalid Content-Range: ${contentRangeHeader ?? '(missing)'}`);
      } else {
        if (parsed.start !== task.start || parsed.end !== task.end) {
          ok = false;
          warnings.push(
            `Content-Range mismatch (got ${parsed.start}-${parsed.end}, expected ${task.start}-${task.end})`,
          );
        }
        if (parsed.total != null && parsed.total !== info.size) {
          warnings.push(`Content-Range total differs from HEAD (${parsed.total} vs ${info.size})`);
        }
      }
      if (bytes !== expectedLen) {
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

    // eslint-disable-next-line no-console
    console.log(
      `[${padLeft(task.index + 1, 2)}] ${rangeValue} status=${status} bytes=${bytes} time=${formatMs(latencyMs)} rate=${formatRate(
        perReqRate,
      )} content-range=${contentRangeHeader ?? '(missing)'} x-cache=${xCache ?? '(missing)'}${
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
  });

  finishedNs = finishedNs ?? nowNs();

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

  // eslint-disable-next-line no-console
  console.log('\nSummary');
  // eslint-disable-next-line no-console
  console.log('-------');
  // eslint-disable-next-line no-console
  console.log(`Requests: ${results.length} ok=${okCount} withWarnings=${warnCount}`);
  // eslint-disable-next-line no-console
  console.log(`Latency: avg=${formatMs(avgLatency)} median=${formatMs(medLatency)}`);
  // eslint-disable-next-line no-console
  console.log(
    `Throughput: bytes=${formatBytes(totalBytes)} wall=${wallTimeSec.toFixed(2)}s aggregate=${formatRate(aggRate)}`,
  );

  const statusParts = [...statusCounts.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([k, v]) => `${k}:${v}`);
  // eslint-disable-next-line no-console
  console.log(`Status codes: ${statusParts.join(' ')}`);

  const hit = xCacheClassCounts.get('hit') ?? 0;
  const miss = xCacheClassCounts.get('miss') ?? 0;
  const other = xCacheClassCounts.get('other') ?? 0;
  const missing = xCacheClassCounts.get('missing') ?? 0;
  // eslint-disable-next-line no-console
  console.log(`X-Cache: hit=${hit} miss=${miss} other=${other} missing=${missing}`);

  if (exactXCacheCounts.size > 0) {
    // eslint-disable-next-line no-console
    console.log('X-Cache breakdown:');
    for (const [k, v] of [...exactXCacheCounts.entries()].sort((a, b) => b[1] - a[1])) {
      // eslint-disable-next-line no-console
      console.log(`  ${padLeft(v, 3)}  ${k}`);
    }
  }
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err && typeof err === 'object' && 'stack' in err ? err.stack : err);
  process.exit(1);
});
