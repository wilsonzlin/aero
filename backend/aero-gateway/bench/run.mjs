import fs from 'node:fs/promises';
import dgram from 'node:dgram';
import net from 'node:net';
import path from 'node:path';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';

import autocannon from 'autocannon';
import dnsPacket from 'dns-packet';
import WebSocket from 'ws';
import { wsCloseSafe, wsIsOpenSafe, wsSendSafe } from '../../../scripts/_shared/ws_safe.js';

// Avoid unbounded memory usage if the socket can't keep up.
const TCP_BENCH_MAX_WS_BUFFERED_AMOUNT_BYTES = 8 * 1024 * 1024;

function socketWriteSafe(socket, data) {
  try {
    return socket.write(data);
  } catch {
    try {
      socket.destroy();
    } catch {
      // ignore
    }
    return false;
  }
}

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      out[key] = next;
      i += 1;
    } else {
      out[key] = true;
    }
  }
  return out;
}

function formatMs(ms) {
  if (ms < 1) return `${(ms * 1000).toFixed(0)}µs`;
  if (ms < 100) return `${ms.toFixed(2)}ms`;
  return `${ms.toFixed(0)}ms`;
}

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.round((p / 100) * (sorted.length - 1))));
  return sorted[idx];
}

function statsFromSamplesMs(samplesMs) {
  const sorted = [...samplesMs].sort((a, b) => a - b);
  const sum = samplesMs.reduce((a, b) => a + b, 0);
  const mean = samplesMs.length === 0 ? null : sum / samplesMs.length;

  // Sample standard deviation (unbiased estimator) + coefficient of variation.
  // We keep this lightweight (simple second pass) since the benchmark already
  // stores the full samples array in memory.
  let stdev = 0;
  if (samplesMs.length > 1 && mean !== null) {
    let varianceSum = 0;
    for (const sample of samplesMs) {
      const diff = sample - mean;
      varianceSum += diff * diff;
    }
    stdev = Math.sqrt(varianceSum / (samplesMs.length - 1));
  }
  const cv = mean === null || mean === 0 ? 0 : stdev / Math.abs(mean);

  return {
    n: samplesMs.length,
    min: sorted[0] ?? null,
    p50: percentile(sorted, 50),
    p90: percentile(sorted, 90),
    p99: percentile(sorted, 99),
    max: sorted.at(-1) ?? null,
    mean,
    stdev,
    cv,
  };
}

function statsFromSamples(samples) {
  const finite = samples.filter((v) => typeof v === 'number' && Number.isFinite(v));
  const sorted = [...finite].sort((a, b) => a - b);
  const sum = finite.reduce((a, b) => a + b, 0);
  const mean = finite.length === 0 ? null : sum / finite.length;

  let stdev = 0;
  if (finite.length > 1 && mean !== null) {
    let varianceSum = 0;
    for (const sample of finite) {
      const diff = sample - mean;
      varianceSum += diff * diff;
    }
    stdev = Math.sqrt(varianceSum / (finite.length - 1));
  }
  const cv = mean === null || mean === 0 ? 0 : stdev / Math.abs(mean);

  return {
    n: finite.length,
    min: sorted[0] ?? null,
    max: sorted.at(-1) ?? null,
    mean,
    stdev,
    cv,
  };
}

function base64UrlEncode(buf) {
  return buf
    .toString('base64')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

async function startUdpDnsServer({ host = '127.0.0.1', answerIp = '127.0.0.1', ttlSeconds = 60 } = {}) {
  const socket = dgram.createSocket('udp4');

  socket.on('message', (msg, rinfo) => {
    let query;
    try {
      query = dnsPacket.decode(msg);
    } catch {
      return;
    }

    const question = query.questions?.[0];
    if (!question) return;

    const response = dnsPacket.encode({
      type: 'response',
      id: query.id,
      flags: dnsPacket.RECURSION_DESIRED | dnsPacket.RECURSION_AVAILABLE,
      questions: query.questions,
      answers:
        question.type === 'A'
          ? [
              {
                type: 'A',
                name: question.name,
                ttl: ttlSeconds,
                data: answerIp,
              },
            ]
          : [],
    });

    try {
      socket.send(response, rinfo.port, rinfo.address);
    } catch {
      // ignore
    }
  });

  await new Promise((resolve, reject) => {
    socket.once('error', reject);
    socket.bind(0, host, resolve);
  });

  const address = socket.address();
  if (typeof address === 'string') throw new Error('Unexpected UDP DNS server address');

  return {
    host,
    port: address.port,
    close: async () => {
      socket.close();
      await once(socket, 'close');
    },
  };
}

async function startGateway({ dnsUpstreamHost, dnsUpstreamPort } = {}) {
  let buildServer;
  let loadConfig;
  try {
    ({ buildServer } = await import('../dist/server.js'));
    ({ loadConfig } = await import('../dist/config.js'));
  } catch (err) {
    const message =
      'Failed to import aero-gateway build artifacts from dist/. ' +
      'Run `npm run build` first (or use `npm run bench`, which does this automatically).';
    const wrapped = new Error(message);
    wrapped.cause = err;
    throw wrapped;
  }

  if (!dnsUpstreamHost || !dnsUpstreamPort) {
    throw new Error('startGateway requires dnsUpstreamHost + dnsUpstreamPort');
  }

  const config = loadConfig({
    HOST: '127.0.0.1',
    PORT: '8080',
    LOG_LEVEL: 'silent',
    ALLOWED_ORIGINS: '*',
    RATE_LIMIT_REQUESTS_PER_MINUTE: '0',
    TRUST_PROXY: '0',
    CROSS_ORIGIN_ISOLATION: '0',

    DNS_UPSTREAMS: `${dnsUpstreamHost}:${dnsUpstreamPort}`,
    DNS_UPSTREAM_TIMEOUT_MS: '1000',
    DNS_QPS_PER_IP: '0',
    DNS_BURST_PER_IP: '0',
    DNS_CACHE_MAX_ENTRIES: '10000',
    DNS_CACHE_MAX_TTL_SECONDS: '300',
    DNS_CACHE_NEGATIVE_TTL_SECONDS: '60',
    DNS_MAX_QUERY_BYTES: '4096',
    DNS_MAX_RESPONSE_BYTES: '4096',
  });

  const { app } = buildServer(config);
  await app.listen({ host: '127.0.0.1', port: 0 });

  const addr = app.server.address();
  if (!addr || typeof addr === 'string') {
    await app.close();
    throw new Error(`Unexpected gateway address: ${String(addr)}`);
  }

  return {
    host: '127.0.0.1',
    port: addr.port,
    url: `http://127.0.0.1:${addr.port}`,
    close: async () => {
      await app.close();
    },
  };
}

async function startEchoServer({ host = '127.0.0.1' } = {}) {
  const server = net.createServer((socket) => {
    socket.on('data', (data) => {
      socketWriteSafe(socket, data);
    });
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, host, resolve);
  });

  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Unexpected echo server address');

  return {
    host,
    port: address.port,
    close: async () => new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
  };
}

async function startSinkServer({ host = '127.0.0.1' } = {}) {
  const server = net.createServer((socket) => {
    let expected = null;
    let seen = 0;
    let done = false;
    let headerBuf = Buffer.alloc(0);

    socket.on('data', (chunk) => {
      if (done) return;
      if (expected === null) {
        headerBuf = Buffer.concat([headerBuf, chunk]);
        if (headerBuf.length < 8) return;

        expected = Number(headerBuf.readBigUInt64BE(0));
        const rest = headerBuf.subarray(8);
        headerBuf = Buffer.alloc(0);

        if (rest.length) socket.emit('data', rest);
        return;
      }

      seen += chunk.length;
      if (seen >= expected) {
        done = true;
        if (!socketWriteSafe(socket, Buffer.from('OK'))) return;
        try {
          socket.end();
        } catch {
          try {
            socket.destroy();
          } catch {
            // ignore
          }
        }
      }
    });
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, host, resolve);
  });

  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Unexpected sink server address');

  return {
    host,
    port: address.port,
    close: async () => new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
  };
}

function createWsByteReader(ws) {
  let buffered = Buffer.alloc(0);
  let pending = [];

  const onMessage = (data) => {
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
    buffered = Buffer.concat([buffered, buf]);
    for (const resolve of pending) resolve();
    pending = [];
  };

  ws.on('message', onMessage);

  const readExact = async (n) => {
    while (buffered.length < n) {
      if (!wsIsOpenSafe(ws)) {
        throw new Error('WebSocket closed while waiting for data');
      }
      await new Promise((resolve) => pending.push(resolve));
    }
    const out = buffered.subarray(0, n);
    buffered = buffered.subarray(n);
    return out;
  };

  const dispose = () => ws.off('message', onMessage);

  return { readExact, dispose };
}

async function wsSend(ws, buf) {
  if (!wsIsOpenSafe(ws)) throw new Error('WebSocket not open');
  await new Promise((resolve, reject) => {
    let settled = false;
    const finish = (fn) => {
      if (settled) return;
      settled = true;
      fn();
    };

    const ok = wsSendSafe(ws, buf, (err) => {
      finish(() => (err ? reject(err) : resolve()));
    });
    if (!ok) finish(() => reject(new Error('WebSocket send failed')));
  });
}

async function benchTcpRttMs({ gatewayPort, targetHost, targetPort, payloadBytes, iterations, warmup }) {
  const wsUrl = new URL(`ws://127.0.0.1:${gatewayPort}/tcp`);
  wsUrl.searchParams.set('v', '1');
  wsUrl.searchParams.set('host', targetHost);
  wsUrl.searchParams.set('port', String(targetPort));
  const ws = new WebSocket(wsUrl);
  await once(ws, 'open');

  const reader = createWsByteReader(ws);
  const payload = Buffer.alloc(payloadBytes, 0x61);

  const roundTripOnce = async () => {
    const start = process.hrtime.bigint();
    await wsSend(ws, payload);
    await reader.readExact(payload.length);
    const end = process.hrtime.bigint();
    return Number(end - start) / 1e6;
  };

  for (let i = 0; i < warmup; i += 1) await roundTripOnce();

  const samples = [];
  for (let i = 0; i < iterations; i += 1) {
    samples.push(await roundTripOnce());
  }

  wsCloseSafe(ws);
  await once(ws, 'close');
  reader.dispose();

  return statsFromSamplesMs(samples);
}

async function benchTcpThroughputMiBps({
  gatewayPort,
  targetHost,
  targetPort,
  totalBytes,
  chunkBytes,
}) {
  const wsUrl = new URL(`ws://127.0.0.1:${gatewayPort}/tcp`);
  wsUrl.searchParams.set('v', '1');
  wsUrl.searchParams.set('host', targetHost);
  wsUrl.searchParams.set('port', String(targetPort));
  const ws = new WebSocket(wsUrl);
  await once(ws, 'open');

  const reader = createWsByteReader(ws);

  const header = Buffer.alloc(8);
  header.writeBigUInt64BE(BigInt(totalBytes), 0);

  const start = process.hrtime.bigint();
  await wsSend(ws, header);

  let sent = 0;
  while (sent < totalBytes) {
    const remaining = totalBytes - sent;
    const size = Math.min(remaining, chunkBytes);
    await wsSend(ws, Buffer.alloc(size));
    sent += size;

    while (ws.bufferedAmount > TCP_BENCH_MAX_WS_BUFFERED_AMOUNT_BYTES) {
      await delay(1);
    }
  }

  const ack = await reader.readExact(2);
  const end = process.hrtime.bigint();

  if (ack.toString('utf8') !== 'OK') {
    throw new Error(`Unexpected throughput ACK: ${ack.toString('utf8')}`);
  }

  wsCloseSafe(ws);
  await once(ws, 'close');
  reader.dispose();

  const seconds = Number(end - start) / 1e9;
  const mib = totalBytes / (1024 * 1024);
  return {
    bytes: totalBytes,
    seconds,
    mibPerSecond: mib / seconds,
  };
}

async function benchTcpThroughputMiBpsMulti({ runs, warmupRuns, ...opts }) {
  const warmups = warmupRuns ?? 0;
  const iterations = runs ?? 1;
  if (!Number.isFinite(iterations) || iterations < 1) throw new Error('benchTcpThroughputMiBpsMulti requires runs >= 1');
  if (!Number.isFinite(warmups) || warmups < 0) throw new Error('benchTcpThroughputMiBpsMulti requires warmupRuns >= 0');

  for (let i = 0; i < warmups; i += 1) {
    await benchTcpThroughputMiBps(opts);
  }

  const results = [];
  for (let i = 0; i < iterations; i += 1) {
    results.push(await benchTcpThroughputMiBps(opts));
  }

  const throughputStats = statsFromSamples(results.map((r) => r.mibPerSecond));
  const secondsStats = statsFromSamples(results.map((r) => r.seconds));

  return {
    bytes: opts.totalBytes,
    seconds: secondsStats.mean,
    mibPerSecond: throughputStats.mean,
    runs: results,
    stats: throughputStats,
  };
}

async function benchDoh({ gatewayPort, durationSeconds, connections }) {
  const query = dnsPacket.encode({
    type: 'query',
    id: 0x1234,
    flags: dnsPacket.RECURSION_DESIRED,
    questions: [{ type: 'A', name: 'bench.test' }],
  });

  const url = `http://127.0.0.1:${gatewayPort}/dns-query?dns=${base64UrlEncode(query)}`;

  // Warm the cache first so the measured run mostly exercises the cache-hit path
  // (avoids a high-concurrency stampede causing many upstream misses).
  const warmupRes = await fetch(url, { headers: { accept: 'application/dns-message' } });
  if (!warmupRes.ok) throw new Error(`DoH warmup failed: HTTP ${warmupRes.status}`);

  const result = await autocannon({
    url,
    method: 'GET',
    connections,
    duration: durationSeconds,
    headers: {
      accept: 'application/dns-message',
    },
  });

  const metricsRes = await fetch(`http://127.0.0.1:${gatewayPort}/metrics`);
  if (!metricsRes.ok) throw new Error(`metrics fetch failed: ${metricsRes.status}`);
  const metricsText = await metricsRes.text();

  const parseCounter = (metricName, matchLabels = {}) => {
    let found = false;
    let sum = 0;
    for (const line of metricsText.split('\n')) {
      if (!line || line.startsWith('#')) continue;
      if (!line.startsWith(metricName)) continue;

      let rest = line.slice(metricName.length);
      let labels = {};
      if (rest.startsWith('{')) {
        const end = rest.indexOf('}');
        if (end === -1) continue;
        const rawLabels = rest.slice(1, end);
        rest = rest.slice(end + 1);

        for (const entry of rawLabels.split(',')) {
          if (!entry) continue;
          const eq = entry.indexOf('=');
          if (eq === -1) continue;
          const key = entry.slice(0, eq).trim();
          let value = entry.slice(eq + 1).trim();
          if (value.startsWith('"') && value.endsWith('"')) value = value.slice(1, -1);
          labels[key] = value;
        }
      }

      const matches = Object.entries(matchLabels).every(([key, value]) => labels[key] === value);
      if (!matches) continue;

      const numeric = Number.parseFloat(rest.trim().split(' ')[0]);
      if (!Number.isFinite(numeric)) continue;
      sum += numeric;
      found = true;
    }

    return found ? sum : null;
  };

  const hits = parseCounter('dns_cache_hits_total', { qtype: 'A' }) ?? 0;
  const misses = parseCounter('dns_cache_misses_total', { qtype: 'A' }) ?? 0;
  const hitRatio = hits + misses === 0 ? null : hits / (hits + misses);

  const getFiniteNumber = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
  const summariseAutocannonStats = (stats, { fallbackN } = {}) => {
    if (!stats || typeof stats !== 'object') return null;
    const mean = getFiniteNumber(stats.mean) ?? getFiniteNumber(stats.average);
    if (mean === null) return null;
    const stdev = getFiniteNumber(stats.stddev) ?? getFiniteNumber(stats.stdev) ?? 0;
    const cv = mean === 0 ? 0 : stdev / Math.abs(mean);
    const min = getFiniteNumber(stats.min) ?? mean;
    const max = getFiniteNumber(stats.max) ?? mean;
    const n = Number.isFinite(stats.n) ? stats.n : fallbackN ?? 1;
    return { n, min, max, mean, stdev, cv };
  };

  const totalRequests = getFiniteNumber(result.requests?.total) ?? getFiniteNumber(result.requests?.count);
  const qpsStats = summariseAutocannonStats(result.requests, { fallbackN: durationSeconds });
  // For latency, use the total request count as the most meaningful sample count.
  const latencyStats = summariseAutocannonStats(result.latency, { fallbackN: totalRequests ?? durationSeconds });
  return {
    qps: result.requests.average,
    qpsStats,
    latencyMs: {
      p50: result.latency.p50,
      p90: result.latency.p90,
      p99: result.latency.p99,
      ...(latencyStats
        ? {
            n: latencyStats.n,
            min: latencyStats.min,
            max: latencyStats.max,
            mean: latencyStats.mean,
            stdev: latencyStats.stdev,
            cv: latencyStats.cv,
          }
        : {}),
    },
    cache: {
      hits,
      misses,
      hitRatio,
    },
    raw: {
      requests: result.requests,
      latency: result.latency,
      throughput: result.throughput,
      errors: result.errors,
      timeouts: result.timeouts,
      non2xx: result.non2xx,
    },
  };
}

function printResultsTable(results) {
  const lines = [];
  lines.push(`Aero Gateway Benchmarks (mode=${results.meta.mode})`);
  lines.push('-'.repeat(lines[0].length));

  const rtt = results.tcpProxy.rttMs;
  lines.push('');
  lines.push(`TCP proxy RTT (${results.meta.tcpRtt.payloadBytes}B payload, n=${rtt.n})`);
  lines.push(`  p50  ${formatMs(rtt.p50)}   p90  ${formatMs(rtt.p90)}   p99  ${formatMs(rtt.p99)}`);
  lines.push(`  min  ${formatMs(rtt.min)}   mean ${formatMs(rtt.mean)}   max  ${formatMs(rtt.max)}`);

  const thr = results.tcpProxy.throughput;
  lines.push('');
  const thrCv = thr.stats?.cv;
  const thrN = thr.stats?.n;
  const thrExtras = [];
  if (typeof thrN === 'number' && Number.isFinite(thrN) && thrN > 1) thrExtras.push(`n=${thrN}`);
  if (typeof thrCv === 'number' && Number.isFinite(thrCv) && thrCv > 0) thrExtras.push(`CV ${(thrCv * 100).toFixed(1)}%`);
  lines.push(
    `TCP proxy throughput (upload ${Math.round(thr.bytes / (1024 * 1024))} MiB${thrExtras.length ? `, ${thrExtras.join(', ')}` : ''})`,
  );
  lines.push(`  ${thr.mibPerSecond.toFixed(1)} MiB/s (${thr.seconds.toFixed(3)}s)`);

  const doh = results.doh;
  lines.push('');
  lines.push(`DoH QPS (connections=${results.meta.doh.connections}, duration=${results.meta.doh.durationSeconds}s)`);
  lines.push(`  ${doh.qps.toFixed(0)} req/s   p50 ${formatMs(doh.latencyMs.p50)}   p99 ${formatMs(doh.latencyMs.p99)}`);
  const hitRatioPct = doh.cache.hitRatio === null ? 'n/a' : `${(doh.cache.hitRatio * 100).toFixed(1)}%`;
  lines.push(`  cache hit ratio ${hitRatioPct} (hits=${doh.cache.hits}, misses=${doh.cache.misses})`);

  lines.push('');
  lines.push(`Results JSON: ${results.meta.outputJson}`);
  console.log(lines.join('\n'));
}

function assertThresholds(results) {
  const failures = [];

  // Conservative thresholds: these are loopback-only and should be stable even
  // on noisy CI runners, while still catching catastrophic regressions.
  const thresholds = {
    tcpRttP50MsMax: 25,
    tcpThroughputMiBpsMin: 1,
    dohQpsMin: 100,
    dohCacheHitRatioMin: 0.95,
  };

  if (results.tcpProxy.rttMs.p50 > thresholds.tcpRttP50MsMax) {
    failures.push(`tcp rtt p50 ${results.tcpProxy.rttMs.p50.toFixed(2)}ms > ${thresholds.tcpRttP50MsMax}ms`);
  }

  if (results.tcpProxy.throughput.mibPerSecond < thresholds.tcpThroughputMiBpsMin) {
    failures.push(
      `tcp throughput ${results.tcpProxy.throughput.mibPerSecond.toFixed(1)}MiB/s < ${thresholds.tcpThroughputMiBpsMin}MiB/s`,
    );
  }

  if (results.doh.qps < thresholds.dohQpsMin) {
    failures.push(`doh qps ${results.doh.qps.toFixed(0)} < ${thresholds.dohQpsMin}`);
  }

  if (results.doh.cache.hitRatio === null || results.doh.cache.hitRatio < thresholds.dohCacheHitRatioMin) {
    failures.push(
      `doh cache hit ratio ${results.doh.cache.hitRatio === null ? 'n/a' : results.doh.cache.hitRatio.toFixed(3)} < ${thresholds.dohCacheHitRatioMin}`,
    );
  }

  if (failures.length > 0) {
    const message = `Benchmark thresholds not met:\n- ${failures.join('\n- ')}`;
    const err = new Error(message);
    err.failures = failures;
    throw err;
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const mode = args.mode === 'smoke' ? 'smoke' : args.mode === 'nightly' ? 'nightly' : 'local';
  const outputJson = args.json ? path.resolve(process.cwd(), args.json) : path.resolve('bench', 'results.json');
  const shouldAssert = Boolean(args.assert);
  const startedAt = new Date().toISOString();

  const config =
    mode === 'smoke'
      ? {
           tcpRtt: { payloadBytes: 32, warmup: 10, iterations: 100 },
           tcpThroughput: { totalBytes: 5 * 1024 * 1024, chunkBytes: 64 * 1024, warmupRuns: 1, runs: 3 },
           doh: { durationSeconds: 3, connections: 25 },
         }
      : mode === 'nightly'
        ? {
            tcpRtt: { payloadBytes: 32, warmup: 20, iterations: 200 },
            tcpThroughput: { totalBytes: 10 * 1024 * 1024, chunkBytes: 64 * 1024, warmupRuns: 1, runs: 5 },
            doh: { durationSeconds: 10, connections: 50 },
          }
        : {
           tcpRtt: { payloadBytes: 32, warmup: 20, iterations: 200 },
           tcpThroughput: { totalBytes: 10 * 1024 * 1024, chunkBytes: 64 * 1024, warmupRuns: 1, runs: 5 },
           doh: { durationSeconds: 10, connections: 50 },
         };

  const echoServer = await startEchoServer();
  const sinkServer = await startSinkServer();
  const dnsServer = await startUdpDnsServer();
  const gateway = await startGateway({ dnsUpstreamHost: dnsServer.host, dnsUpstreamPort: dnsServer.port });

  const results = {
    tool: 'aero-gateway-bench',
    startedAt,
    meta: {
      mode,
      outputJson,
      nodeVersion: process.version,
      platform: process.platform,
      arch: process.arch,
      gateway: { url: gateway.url },
      dnsUpstream: { host: dnsServer.host, port: dnsServer.port },
      tcpRtt: config.tcpRtt,
      tcpThroughput: config.tcpThroughput,
      doh: config.doh,
    },
    tcpProxy: {},
    doh: {},
  };

  try {
    results.tcpProxy.rttMs = await benchTcpRttMs({
      gatewayPort: gateway.port,
      targetHost: echoServer.host,
      targetPort: echoServer.port,
      ...config.tcpRtt,
    });

    results.tcpProxy.throughput = await benchTcpThroughputMiBpsMulti({
      gatewayPort: gateway.port,
      targetHost: sinkServer.host,
      targetPort: sinkServer.port,
      ...config.tcpThroughput,
    });

    results.doh = await benchDoh({ gatewayPort: gateway.port, ...config.doh });

    results.finishedAt = new Date().toISOString();
    await fs.mkdir(path.dirname(outputJson), { recursive: true });
    await fs.writeFile(outputJson, `${JSON.stringify(results, null, 2)}\n`);

    printResultsTable(results);

    if (shouldAssert) assertThresholds(results);
  } finally {
    await Promise.all([gateway.close(), echoServer.close(), sinkServer.close(), dnsServer.close()]);
  }
}

await main();
