import http from 'node:http';
import net from 'node:net';

import dnsPacket from 'dns-packet';
import { WebSocketServer } from 'ws';

function base64UrlDecode(value) {
  const normalized = value.replace(/-/g, '+').replace(/_/g, '/');
  const padding = normalized.length % 4 === 0 ? '' : '='.repeat(4 - (normalized.length % 4));
  return Buffer.from(normalized + padding, 'base64');
}

async function readRequestBody(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function parseTargetParam(req) {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const target = url.searchParams.get('target');
  if (!target) return null;

  const [host, portStr, ...extra] = target.split(':');
  if (!host || !portStr || extra.length > 0) return null;

  const port = Number.parseInt(portStr, 10);
  if (!Number.isFinite(port) || port <= 0 || port > 65535) return null;

  return { host, port };
}

function buildDnsResponseTemplate({ query, answers, flags }) {
  // `dns-packet` includes the query ID in the first two bytes. We store a
  // template with `id = 0` so cache hits can cheaply patch the ID.
  return dnsPacket.encode({
    type: 'response',
    id: 0,
    flags,
    questions: query.questions,
    answers,
  });
}

function patchDnsId(buf, id) {
  const out = Buffer.from(buf);
  out.writeUInt16BE(id, 0);
  return out;
}

function buildStaticAnswers(question) {
  const name = question.name.toLowerCase();

  // Benchmark-specific deterministic "authoritative" data so we never touch
  // external DNS in CI.
  if (question.type === 'A' && name.endsWith('.test')) {
    return [
      {
        type: 'A',
        name: question.name,
        ttl: 60,
        data: '127.0.0.1',
      },
    ];
  }

  if (question.type === 'AAAA' && name.endsWith('.test')) {
    return [
      {
        type: 'AAAA',
        name: question.name,
        ttl: 60,
        data: '::1',
      },
    ];
  }

  return [];
}

export async function startGateway({
  host = '127.0.0.1',
  port = 0,
  dohCacheTtlMs = 60_000,
} = {}) {
  const dohCache = new Map();
  const metrics = {
    startedAt: new Date().toISOString(),
    tcpProxy: {
      connectionsAccepted: 0,
    },
    doh: {
      requests: 0,
      cacheHits: 0,
      cacheMisses: 0,
    },
  };

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`);

      if (url.pathname === '/health') {
        res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' });
        res.end('ok');
        return;
      }

      if (url.pathname === '/metrics') {
        const snapshot = {
          ...metrics,
          doh: {
            ...metrics.doh,
            cacheSize: dohCache.size,
            cacheHitRatio:
              metrics.doh.cacheHits + metrics.doh.cacheMisses === 0
                ? null
                : metrics.doh.cacheHits / (metrics.doh.cacheHits + metrics.doh.cacheMisses),
          },
        };

        res.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
        res.end(`${JSON.stringify(snapshot)}\n`);
        return;
      }

      if (url.pathname !== '/dns-query') {
        res.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
        res.end('not found');
        return;
      }

      let queryBuf;
      if (req.method === 'GET') {
        const dnsParam = url.searchParams.get('dns');
        if (!dnsParam) {
          res.writeHead(400, { 'content-type': 'text/plain; charset=utf-8' });
          res.end('missing dns query parameter');
          return;
        }
        queryBuf = base64UrlDecode(dnsParam);
      } else if (req.method === 'POST') {
        queryBuf = await readRequestBody(req);
      } else {
        res.writeHead(405, { 'allow': 'GET, POST' });
        res.end();
        return;
      }

      let query;
      try {
        query = dnsPacket.decode(queryBuf);
      } catch {
        res.writeHead(400, { 'content-type': 'text/plain; charset=utf-8' });
        res.end('invalid dns query');
        return;
      }

      metrics.doh.requests += 1;

      const question = query.questions?.[0];
      if (!question) {
        res.writeHead(400, { 'content-type': 'text/plain; charset=utf-8' });
        res.end('dns query missing question');
        return;
      }

      const key = `${question.name.toLowerCase()}\0${question.type}`;
      const now = Date.now();

      const cached = dohCache.get(key);
      if (cached && cached.expiresAt > now) {
        metrics.doh.cacheHits += 1;
        const body = patchDnsId(cached.template, query.id);
        res.writeHead(200, {
          'content-type': 'application/dns-message',
          'x-aero-cache': 'HIT',
        });
        res.end(body);
        return;
      }

      metrics.doh.cacheMisses += 1;
      const answers = buildStaticAnswers(question);
      const flags = dnsPacket.RECURSION_DESIRED | dnsPacket.RECURSION_AVAILABLE;

      const template = buildDnsResponseTemplate({ query, answers, flags });
      dohCache.set(key, { template, expiresAt: now + dohCacheTtlMs });

      res.writeHead(200, {
        'content-type': 'application/dns-message',
        'x-aero-cache': 'MISS',
      });
      res.end(patchDnsId(template, query.id));
    } catch (err) {
      res.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
      res.end(`internal error: ${err instanceof Error ? err.message : String(err)}`);
    }
  });

  const wss = new WebSocketServer({ server, path: '/tcp' });

  wss.on('connection', (ws, req) => {
    const target = parseTargetParam(req);
    if (!target) {
      ws.close(1008, 'invalid target');
      return;
    }

    metrics.tcpProxy.connectionsAccepted += 1;

    const socket = net.createConnection({ host: target.host, port: target.port });

    const closeBoth = () => {
      if (ws.readyState === ws.OPEN || ws.readyState === ws.CONNECTING) ws.close();
      socket.destroy();
    };

    socket.on('data', (data) => {
      if (ws.readyState !== ws.OPEN) return;
      ws.send(data, { binary: true }, (err) => {
        if (err) closeBoth();
      });
    });

    socket.on('error', () => closeBoth());
    socket.on('close', () => {
      if (ws.readyState === ws.OPEN) ws.close();
    });

    ws.on('message', (data) => {
      if (socket.destroyed) return;
      const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
      socket.write(buf);
    });

    ws.on('close', () => socket.destroy());
    ws.on('error', () => socket.destroy());
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => resolve());
  });

  const address = server.address();
  if (!address || typeof address === 'string') {
    throw new Error(`Unexpected server address: ${String(address)}`);
  }

  return {
    host,
    port: address.port,
    url: `http://${host}:${address.port}`,
    close: async () => {
      await Promise.all([
        new Promise((resolve) => wss.close(() => resolve())),
        new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
      ]);
    },
  };
}

