import fs from 'node:fs/promises';
import net from 'node:net';
import path from 'node:path';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';

import autocannon from 'autocannon';
import dnsPacket from 'dns-packet';
import WebSocket from 'ws';

import { startGateway } from '../src/gateway.mjs';

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

  return {
    n: samplesMs.length,
    min: sorted[0] ?? null,
    p50: percentile(sorted, 50),
    p90: percentile(sorted, 90),
    p99: percentile(sorted, 99),
    max: sorted.at(-1) ?? null,
    mean,
  };
}

function base64UrlEncode(buf) {
  return buf
    .toString('base64')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

async function startEchoServer({ host = '127.0.0.1' } = {}) {
  const server = net.createServer((socket) => {
    socket.on('data', (data) => {
      socket.write(data);
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
    let headerBuf = Buffer.alloc(0);

    socket.on('data', (chunk) => {
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
        socket.write(Buffer.from('OK'));
        socket.end();
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
      if (ws.readyState !== ws.OPEN) {
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
  if (ws.readyState !== ws.OPEN) throw new Error('WebSocket not open');
  await new Promise((resolve, reject) => {
    ws.send(buf, { binary: true }, (err) => (err ? reject(err) : resolve()));
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

  ws.close();
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

    // Avoid unbounded memory usage if the socket can't keep up.
    while (ws.bufferedAmount > 8 * 1024 * 1024) {
      await delay(1);
    }
  }

  const ack = await reader.readExact(2);
  const end = process.hrtime.bigint();

  if (ack.toString('utf8') !== 'OK') {
    throw new Error(`Unexpected throughput ACK: ${ack.toString('utf8')}`);
  }

  ws.close();
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

async function benchDoh({ gatewayPort, durationSeconds, connections }) {
  const query = dnsPacket.encode({
    type: 'query',
    id: 0x1234,
    flags: dnsPacket.RECURSION_DESIRED,
    questions: [{ type: 'A', name: 'bench.test' }],
  });

  const url = `http://127.0.0.1:${gatewayPort}/dns-query?dns=${base64UrlEncode(query)}`;

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
  const metrics = await metricsRes.json();

  const hitRatio = metrics.doh.cacheHitRatio ?? null;
  return {
    qps: result.requests.average,
    latencyMs: {
      p50: result.latency.p50,
      p90: result.latency.p90,
      p99: result.latency.p99,
    },
    cache: {
      hits: metrics.doh.cacheHits,
      misses: metrics.doh.cacheMisses,
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
  lines.push(`TCP proxy throughput (upload ${Math.round(thr.bytes / (1024 * 1024))} MiB)`);
  lines.push(`  ${thr.mibPerSecond.toFixed(1)} MiB/s (${thr.seconds.toFixed(3)}s)`);

  const doh = results.doh;
  lines.push('');
  lines.push(`DoH QPS (connections=${results.meta.doh.connections}, duration=${results.meta.doh.durationSeconds}s)`);
  lines.push(`  ${doh.qps.toFixed(0)} req/s   p50 ${formatMs(doh.latencyMs.p50)}   p99 ${formatMs(doh.latencyMs.p99)}`);
  lines.push(
    `  cache hit ratio ${(doh.cache.hitRatio * 100).toFixed(1)}% (hits=${doh.cache.hits}, misses=${doh.cache.misses})`,
  );

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

  if (results.doh.cache.hitRatio < thresholds.dohCacheHitRatioMin) {
    failures.push(
      `doh cache hit ratio ${results.doh.cache.hitRatio.toFixed(3)} < ${thresholds.dohCacheHitRatioMin}`,
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
  const mode = args.mode === 'smoke' ? 'smoke' : 'local';
  const outputJson = args.json ? path.resolve(process.cwd(), args.json) : path.resolve('bench', 'results.json');
  const shouldAssert = Boolean(args.assert);

  const config =
    mode === 'smoke'
      ? {
          tcpRtt: { payloadBytes: 32, warmup: 10, iterations: 100 },
          tcpThroughput: { totalBytes: 5 * 1024 * 1024, chunkBytes: 64 * 1024 },
          doh: { durationSeconds: 3, connections: 25 },
        }
      : {
          tcpRtt: { payloadBytes: 32, warmup: 20, iterations: 200 },
          tcpThroughput: { totalBytes: 10 * 1024 * 1024, chunkBytes: 64 * 1024 },
          doh: { durationSeconds: 10, connections: 50 },
        };

  const echoServer = await startEchoServer();
  const sinkServer = await startSinkServer();
  const gateway = await startGateway();

  const results = {
    meta: {
      mode,
      outputJson,
      gateway: { url: gateway.url },
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

    results.tcpProxy.throughput = await benchTcpThroughputMiBps({
      gatewayPort: gateway.port,
      targetHost: sinkServer.host,
      targetPort: sinkServer.port,
      ...config.tcpThroughput,
    });

    results.doh = await benchDoh({ gatewayPort: gateway.port, ...config.doh });

    await fs.mkdir(path.dirname(outputJson), { recursive: true });
    await fs.writeFile(outputJson, `${JSON.stringify(results, null, 2)}\n`);

    printResultsTable(results);

    if (shouldAssert) assertThresholds(results);
  } finally {
    await Promise.all([gateway.close(), echoServer.close(), sinkServer.close()]);
  }
}

await main();
