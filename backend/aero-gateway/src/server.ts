import fastify, { type FastifyInstance } from 'fastify';
import fastifyStatic from '@fastify/static';
import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import type http from 'node:http';
import type { Duplex } from 'node:stream';
import type { Config } from './config.js';
import { appendSetCookieHeader, isRequestSecure, serializeCookie } from './cookies.js';
import { setupCrossOriginIsolation } from './middleware/crossOriginIsolation.js';
import { originGuard } from './middleware/originGuard.js';
import { setupRequestIdHeader } from './middleware/requestId.js';
import { setupRateLimit } from './middleware/rateLimit.js';
import { setupSecurityHeaders } from './middleware/securityHeaders.js';
import { setupMetrics } from './metrics.js';
import { setupDohRoutes } from './routes/doh.js';
import { handleTcpMuxUpgrade } from './routes/tcpMux.js';
import { handleTcpProxyUpgrade } from './routes/tcpProxy.js';
import { getVersionInfo } from './version.js';

type ServerBundle = {
  app: FastifyInstance;
  markShuttingDown: () => void;
  closeUpgradeSockets: () => void;
};

function findFrontendDistDir(): string | null {
  const candidates = [
    path.resolve(process.cwd(), '../../web/dist'),
    path.resolve(process.cwd(), '../web/dist'),
    path.resolve(process.cwd(), 'web/dist'),
    path.resolve(process.cwd(), '../../frontend/dist'),
    path.resolve(process.cwd(), '../frontend/dist'),
    path.resolve(process.cwd(), 'frontend/dist'),
  ];

  for (const dir of candidates) {
    try {
      if (!fs.statSync(dir).isDirectory()) continue;
      return dir;
    } catch {
      // ignore missing
    }
  }

  return null;
}

function respondUpgradeHttp(socket: Duplex, status: number, message: string): void {
  const body = `${message}\n`;
  socket.end(
    [
      `HTTP/1.1 ${status} ${httpStatusText(status)}`,
      'Content-Type: text/plain; charset=utf-8',
      `Content-Length: ${Buffer.byteLength(body)}`,
      'Connection: close',
      '\r\n',
      body,
    ].join('\r\n'),
  );
}

function httpStatusText(status: number): string {
  switch (status) {
    case 400:
      return 'Bad Request';
    case 403:
      return 'Forbidden';
    case 404:
      return 'Not Found';
    case 503:
      return 'Service Unavailable';
    default:
      return 'Error';
  }
}

export function buildServer(config: Config): ServerBundle {
  let shuttingDown = false;
  const upgradeSockets = new Set<Duplex>();

  const app = fastify({
    trustProxy: config.TRUST_PROXY,
    logger: { level: config.LOG_LEVEL },
    requestIdHeader: 'x-request-id',
    ...(config.TLS_ENABLED
      ? {
          https: {
            cert: fs.readFileSync(config.TLS_CERT_PATH),
            key: fs.readFileSync(config.TLS_KEY_PATH),
          },
        }
      : {}),
    genReqId: (req) => {
      const header = req.headers['x-request-id'];
      if (typeof header === 'string' && header.length > 0) return header;
      if (Array.isArray(header) && header.length > 0 && header[0]) return header[0];
      return randomUUID();
    },
  });

  setupRequestIdHeader(app);
  setupSecurityHeaders(app);
  if (config.CROSS_ORIGIN_ISOLATION) setupCrossOriginIsolation(app);

  setupRateLimit(app, { requestsPerMinute: config.RATE_LIMIT_REQUESTS_PER_MINUTE });

  app.addHook('preHandler', async (request, reply) => {
    await originGuard(request, reply, { allowedOrigins: config.ALLOWED_ORIGINS });
  });

  const metrics = setupMetrics(app);

  app.get('/healthz', async () => ({ ok: true }));

  app.get('/readyz', async (_request, reply) => {
    if (shuttingDown) return reply.code(503).send({ ok: false });
    return { ok: true };
  });

  app.get('/version', async () => getVersionInfo());

  // Helper endpoint to validate Secure cookie behaviour in local dev (TLS vs proxy TLS termination).
  app.get('/session', async (request, reply) => {
    const secure = isRequestSecure(request.raw, { trustProxy: config.TRUST_PROXY });
    appendSetCookieHeader(
      reply.raw,
      serializeCookie('aero_session', randomUUID(), {
        httpOnly: true,
        sameSite: 'Lax',
        secure,
        maxAgeSeconds: 60 * 60 * 24,
      }),
    );
    return { ok: true };
  });

  // WebSocket upgrade endpoints (/tcp and /tcp-mux) are handled at the Node HTTP
  // server layer (Fastify does not natively handle arbitrary upgrade routing).
  app.server.on('upgrade', (req: http.IncomingMessage, socket: Duplex, head: Buffer) => {
    upgradeSockets.add(socket);
    socket.once('close', () => upgradeSockets.delete(socket));

    if (shuttingDown) {
      respondUpgradeHttp(socket, 503, 'Shutting down');
      return;
    }

    let url: URL;
    try {
      url = new URL(req.url ?? '', 'http://localhost');
    } catch {
      respondUpgradeHttp(socket, 400, 'Invalid request URL');
      return;
    }

    if (url.pathname === '/tcp') {
      handleTcpProxyUpgrade(req, socket, head, {
        allowedOrigins: config.ALLOWED_ORIGINS,
        blockedClientIps: config.TCP_BLOCKED_CLIENT_IPS,
        allowedTargetHosts: config.TCP_ALLOWED_HOSTS,
        allowedTargetPorts: config.TCP_ALLOWED_PORTS,
        allowPrivateIps: config.TCP_ALLOW_PRIVATE_IPS,
        metrics: metrics.tcpProxy,
      });
      return;
    }

    if (url.pathname === '/tcp-mux') {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedOrigins: config.ALLOWED_ORIGINS,
        blockedClientIps: config.TCP_BLOCKED_CLIENT_IPS,
        allowedTargetHosts: config.TCP_ALLOWED_HOSTS,
        allowedTargetPorts: config.TCP_ALLOWED_PORTS,
        allowPrivateIps: config.TCP_ALLOW_PRIVATE_IPS,
        maxStreams: config.TCP_MUX_MAX_STREAMS,
        maxStreamBufferedBytes: config.TCP_MUX_MAX_STREAM_BUFFER_BYTES,
        maxFramePayloadBytes: config.TCP_MUX_MAX_FRAME_PAYLOAD_BYTES,
        metrics: metrics.tcpProxy,
      });
      return;
    }

    respondUpgradeHttp(socket, 404, 'Not Found');
  });
  setupDohRoutes(app, config, metrics.dns);

  if (process.env.AERO_GATEWAY_E2E === '1') {
    app.get('/e2e', async (_request, reply) => {
      reply.type('text/html; charset=utf-8');
      reply.header('cache-control', 'no-store');
      return e2ePageHtml();
    });
  }

  // Handle CORS preflight requests, even when no route matches.
  app.options('/*', async (_request, reply) => reply.code(204).send());

  const staticDir = findFrontendDistDir();
  if (staticDir) {
    app.log.info({ staticDir }, 'Serving static frontend assets');
    app.register(fastifyStatic, { root: staticDir });
  } else {
    app.log.info('No frontend/dist found; static hosting disabled');
  }

  return {
    app,
    markShuttingDown: () => {
      shuttingDown = true;
    },
    closeUpgradeSockets: () => {
      for (const socket of upgradeSockets) socket.destroy();
    },
  };
}

function e2ePageHtml(): string {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>aero-gateway e2e</title>
  </head>
  <body>
    <pre id="out">running…</pre>
    <script>
      const outEl = document.getElementById('out');
      function render(obj) {
        outEl.textContent = JSON.stringify(obj, null, 2);
      }
      function withTimeout(promise, ms, label) {
        return Promise.race([
          promise,
          new Promise((_, reject) => setTimeout(() => reject(new Error(label + ' timed out after ' + ms + 'ms')), ms)),
        ]);
      }
      function requireParam(name) {
        const url = new URL(location.href);
        const value = url.searchParams.get(name);
        if (!value) throw new Error('Missing required query parameter: ' + name);
        return value;
      }
      function buildDnsQueryA(name) {
        const id = Math.floor(Math.random() * 65536);
        const bytes = [];
        bytes.push((id >> 8) & 0xff, id & 0xff); // ID
        bytes.push(0x01, 0x00); // flags: RD
        bytes.push(0x00, 0x01); // QDCOUNT
        bytes.push(0x00, 0x00); // ANCOUNT
        bytes.push(0x00, 0x00); // NSCOUNT
        bytes.push(0x00, 0x00); // ARCOUNT

        for (const label of name.split('.')) {
          const encoded = new TextEncoder().encode(label);
          if (encoded.length === 0 || encoded.length > 63) throw new Error('Invalid DNS label');
          bytes.push(encoded.length);
          for (const b of encoded) bytes.push(b);
        }
        bytes.push(0x00); // end of QNAME

        bytes.push(0x00, 0x01); // QTYPE=A
        bytes.push(0x00, 0x01); // QCLASS=IN

        return { id, bytes: new Uint8Array(bytes) };
      }
      function base64Url(bytes) {
        let binary = '';
        for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
        return btoa(binary).replace(/\\+/g, '-').replace(/\\//g, '_').replace(/=+$/g, '');
      }
      function readU16BE(u8, off) {
        return (u8[off] << 8) | u8[off + 1];
      }

      (async () => {
        const results = {
          crossOriginIsolated: window.crossOriginIsolated,
          sharedArrayBuffer: { ok: false, error: null },
          websocket: { ok: false, echo: null, error: null },
          dnsQuery: { ok: false, meta: null, error: null },
          dnsJson: { ok: false, answer: null, error: null },
        };

        try {
          const buf = new SharedArrayBuffer(16);
          results.sharedArrayBuffer.ok = buf.byteLength === 16;
        } catch (err) {
          results.sharedArrayBuffer.error = String(err && err.message ? err.message : err);
        }

        try {
          const echoPort = Number(requireParam('echoPort'));
          if (!Number.isInteger(echoPort) || echoPort < 1 || echoPort > 65535) {
            throw new Error('Invalid echoPort');
          }

          const wsBase = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host;
          const wsUrl = new URL('/tcp', wsBase);
          wsUrl.searchParams.set('v', '1');
          wsUrl.searchParams.set('host', '127.0.0.1');
          wsUrl.searchParams.set('port', String(echoPort));
          const echo = await withTimeout(new Promise((resolve, reject) => {
            const ws = new WebSocket(wsUrl.toString());
            ws.binaryType = 'arraybuffer';
            ws.onopen = () => {
              const data = new TextEncoder().encode('ping');
              ws.send(data);
            };
            ws.onerror = () => reject(new Error('WebSocket error'));
            ws.onmessage = (event) => {
              resolve(event.data);
              ws.close();
            };
          }), 5000, 'WebSocket');

          if (!(echo instanceof ArrayBuffer)) {
            throw new Error('Expected ArrayBuffer echo, got ' + Object.prototype.toString.call(echo));
          }
          const decoded = new TextDecoder().decode(new Uint8Array(echo));
          results.websocket.ok = decoded === 'ping';
          results.websocket.echo = decoded;
        } catch (err) {
          results.websocket.error = String(err && err.message ? err.message : err);
        }

        try {
          const query = buildDnsQueryA('example.com');
          const dns = base64Url(query.bytes);
          const res = await withTimeout(fetch('/dns-query?dns=' + encodeURIComponent(dns)), 5000, 'dns-query fetch');
          const buf = new Uint8Array(await res.arrayBuffer());

          if (buf.length < 12) throw new Error('DNS response too short');
          const id = readU16BE(buf, 0);
          const flags = readU16BE(buf, 2);
          const qdcount = readU16BE(buf, 4);
          const ancount = readU16BE(buf, 6);
          const rcode = flags & 0x000f;
          const qr = (flags & 0x8000) !== 0;

          results.dnsQuery.meta = { id, rcode, qdcount, ancount };
          results.dnsQuery.ok = id === query.id && qr && rcode === 0 && qdcount === 1 && ancount >= 1;
        } catch (err) {
          results.dnsQuery.error = String(err && err.message ? err.message : err);
        }

        try {
          const res = await withTimeout(fetch('/dns-json?name=example.com&type=A'), 5000, 'dns-json fetch');
          const json = await res.json();
          results.dnsJson.answer = json?.Answer?.[0]?.data ?? null;
          results.dnsJson.ok = Boolean(res.ok && json && json.Status === 0 && typeof results.dnsJson.answer === 'string');
        } catch (err) {
          results.dnsJson.error = String(err && err.message ? err.message : err);
        }

        window.__aeroGatewayE2E = results;
        render(results);
      })();
    </script>
  </body>
</html>`;
}
