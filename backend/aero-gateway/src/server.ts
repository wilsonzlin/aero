import fastify, { type FastifyInstance } from 'fastify';
import fastifyStatic from '@fastify/static';
import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import type http from 'node:http';
import type { Duplex } from 'node:stream';
import type { Config } from './config.js';
import { appendSetCookieHeader, isRequestSecure, serializeCookie } from './cookies.js';
import { TokenBucketRateLimiter } from './dns/rateLimit.js';
import { SESSION_COOKIE_NAME, SessionConnectionTracker, createSessionManager } from './session.js';
import { setupCrossOriginIsolation } from './middleware/crossOriginIsolation.js';
import { originGuard } from './middleware/originGuard.js';
import { setupRequestIdHeader } from './middleware/requestId.js';
import { setupRateLimit } from './middleware/rateLimit.js';
import { setupSecurityHeaders } from './middleware/securityHeaders.js';
import { setupMetrics } from './metrics.js';
import { setupDohRoutes } from './routes/doh.js';
import { handleTcpMuxUpgrade } from './routes/tcpMux.js';
import { handleTcpProxyUpgrade } from './routes/tcpProxy.js';
import {
  MAX_REQUEST_URL_LEN,
  enforceUpgradeRequestUrlLimit,
  parseUpgradeRequestUrl,
  respondUpgradeHttp,
} from './routes/upgradeHttp.js';
import { validateWebSocketHandshakeRequest } from './routes/wsUpgradeRequest.js';
import { normalizeOriginString } from './security/origin.js';
import { buildUdpRelaySessionInfo, mintUdpRelayToken } from './udpRelay.js';
import { getVersionInfo } from './version.js';
import { formatOneLineError } from './util/text.js';
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
} from './protocol/l2Tunnel.js';

type ServerBundle = {
  app: FastifyInstance;
  markShuttingDown: () => void;
  closeUpgradeSockets: () => void;
};

function isUpgradeSocketDestroyed(socket: Duplex): boolean {
  try {
    return (socket as unknown as { destroyed?: unknown }).destroyed === true;
  } catch {
    // Fail closed: if state is not observable, treat it as destroyed.
    return true;
  }
}

function normalizeBasePathFromPublicBaseUrl(publicBaseUrl: string): string {
  let pathname: string;
  try {
    pathname = new URL(publicBaseUrl).pathname;
  } catch {
    // `PUBLIC_BASE_URL` is validated by config loading, but tests may construct
    // configs directly. Fall back to root path.
    pathname = '/';
  }

  // `URL.pathname` is usually at least `/`, but be defensive.
  let basePath = pathname || '/';
  if (basePath === '') basePath = '/';
  if (!basePath.startsWith('/')) basePath = `/${basePath}`;

  // Remove trailing `/` except for the root path.
  if (basePath !== '/') basePath = basePath.replace(/\/+$/, '');
  if (basePath === '') basePath = '/';
  return basePath;
}

function joinBasePath(basePath: string, endpointPath: string): string {
  const suffix = endpointPath.startsWith('/') ? endpointPath : `/${endpointPath}`;
  if (basePath === '' || basePath === '/') return suffix;
  return `${basePath}${suffix}`;
}

function findFrontendDistDir(): string | null {
  const candidates = [
    path.resolve(process.cwd(), '../../dist'),
    path.resolve(process.cwd(), '../dist'),
    path.resolve(process.cwd(), 'dist'),
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

export function buildServer(config: Config): ServerBundle {
  let shuttingDown = false;
  const upgradeSockets = new Set<Duplex>();

  const basePath = normalizeBasePathFromPublicBaseUrl(config.PUBLIC_BASE_URL);
  const endpoints = {
    tcp: joinBasePath(basePath, '/tcp'),
    dnsQuery: joinBasePath(basePath, '/dns-query'),
    tcpMux: joinBasePath(basePath, '/tcp-mux'),
    dnsJson: joinBasePath(basePath, '/dns-json'),
    l2: joinBasePath(basePath, '/l2'),
    udpRelayToken: joinBasePath(basePath, '/udp-relay/token'),
  } as const;

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

  // Conservative cap to avoid spending unbounded CPU/memory on attacker-controlled request targets.
  // Many HTTP stacks enforce ~8KB request target limits; keep the gateway strict and predictable.
  app.addHook('onRequest', async (request, reply) => {
    const rawUrl = request.raw.url;
    if (typeof rawUrl !== 'string' || rawUrl === '' || rawUrl.trim() !== rawUrl) {
      reply.code(400).send({ error: 'bad_request', message: 'Invalid request URL' });
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      reply.code(414).send({ error: 'url_too_long', message: 'Request URL too long' });
      return;
    }
  });

  const sessions = createSessionManager(config, app.log);
  const sessionConnections = new SessionConnectionTracker(config.TCP_PROXY_MAX_CONNECTIONS);
  const udpRelayTokenRateLimiter = new TokenBucketRateLimiter(1, 5);

  setupRequestIdHeader(app);
  setupSecurityHeaders(app);
  if (config.CROSS_ORIGIN_ISOLATION) setupCrossOriginIsolation(app);

  app.addHook('onRequest', async (request, reply) => {
    await originGuard(request, reply, { allowedOrigins: config.ALLOWED_ORIGINS });
  });

  setupRateLimit(app, { requestsPerMinute: config.RATE_LIMIT_REQUESTS_PER_MINUTE });

  const metrics = setupMetrics(app);

  const handleHealthz = async () => ({ ok: true });
  const handleReadyz = async (_request: unknown, reply: import('fastify').FastifyReply) => {
    if (shuttingDown) return reply.code(503).send({ ok: false });
    return { ok: true };
  };
  const handleVersion = async () => getVersionInfo();

  const handlePostSession = async (request: import('fastify').FastifyRequest, reply: import('fastify').FastifyReply) => {
    if (request.body !== undefined && (typeof request.body !== 'object' || request.body === null || Array.isArray(request.body))) {
      return reply.code(400).send({ error: 'bad_request', message: 'Request body must be a JSON object' });
    }

    const nowMs = Date.now();
    const existing = sessions.verifySessionRequest(request.raw);
    const { token, session } = sessions.issueSession(existing);

    const secure = isRequestSecure(request.raw, { trustProxy: config.TRUST_PROXY });
    if (config.SESSION_COOKIE_SAMESITE === 'None' && !secure) {
      request.log.warn(
        { sameSite: config.SESSION_COOKIE_SAMESITE },
        'SESSION_COOKIE_SAMESITE=None set on a non-secure request; browsers may reject the cookie',
      );
    }

    appendSetCookieHeader(
      reply.raw,
      serializeCookie(SESSION_COOKIE_NAME, token, {
        httpOnly: true,
        sameSite: config.SESSION_COOKIE_SAMESITE,
        secure,
        maxAgeSeconds: config.SESSION_TTL_SECONDS,
      }),
    );

    reply.header('cache-control', 'no-store');
    reply.code(201);
    const l2MaxFramePayloadBytes = config.L2_MAX_FRAME_PAYLOAD_BYTES ?? L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES;
    const l2MaxControlPayloadBytes = config.L2_MAX_CONTROL_PAYLOAD_BYTES ?? L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES;
    const response: Record<string, unknown> = {
      session: { expiresAt: new Date(session.expiresAtMs).toISOString() },
      endpoints,
      limits: {
        tcp: {
          maxConnections: config.TCP_PROXY_MAX_CONNECTIONS,
          maxMessageBytes: config.TCP_PROXY_MAX_MESSAGE_BYTES,
          connectTimeoutMs: config.TCP_PROXY_CONNECT_TIMEOUT_MS,
          idleTimeoutMs: config.TCP_PROXY_IDLE_TIMEOUT_MS,
        },
        dns: { maxQueryBytes: config.DNS_MAX_QUERY_BYTES },
        l2: { maxFramePayloadBytes: l2MaxFramePayloadBytes, maxControlPayloadBytes: l2MaxControlPayloadBytes },
      },
    };

    const originHeader = request.headers.origin;
    const originRaw = Array.isArray(originHeader) ? originHeader[0] : originHeader;
    const origin = originRaw ? normalizeOriginString(originRaw) ?? undefined : undefined;
    try {
      const udpRelay = buildUdpRelaySessionInfo(config, { sessionId: session.id, origin, nowMs });
      if (udpRelay) response.udpRelay = udpRelay;
    } catch (err) {
      request.log.warn({ err }, 'udp_relay_token_mint_error');
    }

    return response;
  };

  const handlePostUdpRelayToken = async (request: import('fastify').FastifyRequest, reply: import('fastify').FastifyReply) => {
    if (!config.UDP_RELAY_BASE_URL) {
      return reply.code(404).send({ error: 'not_found', message: 'UDP relay not configured' });
    }

    const originHeader = request.headers.origin;
    const originRaw = Array.isArray(originHeader) ? originHeader[0] : originHeader;
    if (!originRaw) {
      return reply.code(403).send({ error: 'forbidden', message: 'Origin header required' });
    }
    const origin = normalizeOriginString(originRaw);
    if (!origin) {
      return reply.code(403).send({ error: 'forbidden', message: 'Origin not allowed' });
    }

    const session = sessions.verifySessionRequest(request.raw);
    if (!session) {
      return reply.code(401).send({ error: 'unauthorized', message: 'Missing or expired session' });
    }

    if (!udpRelayTokenRateLimiter.allow(session.id)) {
      return reply.code(429).send({ error: 'too_many_requests', message: 'Rate limit exceeded' });
    }

    let tokenInfo;
    try {
      tokenInfo = mintUdpRelayToken(config, { sessionId: session.id, origin });
    } catch (err) {
      request.log.warn({ err }, 'udp_relay_token_mint_error');
      return reply.code(500).send({ error: 'internal_error', message: 'Failed to mint UDP relay token' });
    }
    if (!tokenInfo) {
      return reply.code(404).send({ error: 'not_found', message: 'UDP relay not configured' });
    }

    reply.header('cache-control', 'no-store');
    return tokenInfo;
  };

  // Helper endpoint to validate Secure cookie behaviour in local dev (TLS vs proxy TLS termination).
  const handleGetSessionHelper = async (request: import('fastify').FastifyRequest, reply: import('fastify').FastifyReply) => {
    const existing = sessions.verifySessionRequest(request.raw);
    const { token } = sessions.issueSession(existing);
    const secure = isRequestSecure(request.raw, { trustProxy: config.TRUST_PROXY });
    appendSetCookieHeader(
      reply.raw,
      serializeCookie(SESSION_COOKIE_NAME, token, {
        httpOnly: true,
        sameSite: config.SESSION_COOKIE_SAMESITE,
        secure,
        maxAgeSeconds: config.SESSION_TTL_SECONDS,
      }),
    );
    return { ok: true };
  };

  const handleMetrics = async (_request: unknown, reply: import('fastify').FastifyReply) => {
    reply.header('content-type', metrics.registry.contentType);
    return metrics.registry.metrics();
  };

  const handleE2e = async (_request: unknown, reply: import('fastify').FastifyReply) => {
    reply.type('text/html; charset=utf-8');
    reply.header('cache-control', 'no-store');
    return e2ePageHtml();
  };

  app.get('/healthz', handleHealthz);
  app.get('/readyz', handleReadyz);
  app.get('/version', handleVersion);
  app.post('/session', handlePostSession);
  app.post('/udp-relay/token', handlePostUdpRelayToken);
  app.get('/session', handleGetSessionHelper);

  const dohDeps = setupDohRoutes(app, config, metrics.dns, sessions);

  if (process.env.AERO_GATEWAY_E2E === '1') {
    app.get('/e2e', handleE2e);
  }

  // Some reverse proxies forward the base path prefix to the gateway (no rewrite).
  // To keep `PUBLIC_BASE_URL` + endpoint discovery consistent with such setups,
  // also serve HTTP routes under the configured base path.
  if (basePath !== '/') {
    app.register(
      async (prefixed) => {
        prefixed.get('/healthz', handleHealthz);
        prefixed.get('/readyz', handleReadyz);
        prefixed.get('/version', handleVersion);
        prefixed.post('/session', handlePostSession);
        prefixed.post('/udp-relay/token', handlePostUdpRelayToken);
        prefixed.get('/session', handleGetSessionHelper);
        prefixed.get('/metrics', handleMetrics);
        setupDohRoutes(prefixed, config, metrics.dns, sessions, dohDeps);
        if (process.env.AERO_GATEWAY_E2E === '1') {
          prefixed.get('/e2e', handleE2e);
        }
      },
      { prefix: basePath },
    );
  }

  // WebSocket upgrade endpoints (/tcp and /tcp-mux) are handled at the Node HTTP
  // server layer (Fastify does not natively handle arbitrary upgrade routing).
  app.server.on('upgrade', (req: http.IncomingMessage, socket: Duplex, head: Buffer) => {
    upgradeSockets.add(socket);
    socket.once('close', () => upgradeSockets.delete(socket));

    try {
      if (shuttingDown) {
        respondUpgradeHttp(socket, 503, 'Shutting down');
        return;
      }

      const rawUrl = req.url;
      if (typeof rawUrl !== 'string' || rawUrl === '' || rawUrl.trim() !== rawUrl) {
        respondUpgradeHttp(socket, 400, 'Invalid request URL');
        return;
      }
      if (!enforceUpgradeRequestUrlLimit(rawUrl, socket)) return;
      const url = parseUpgradeRequestUrl(rawUrl, socket, { invalidUrlMessage: 'Invalid request URL' });
      if (!url) return;

      // The gateway may be deployed behind a reverse proxy under a base path
      // prefix (e.g. `/aero`). Some proxies strip that prefix before forwarding
      // to the gateway, while others forward it verbatim. Support both.
      const authForUpgrade = () => {
        const handshake = validateWebSocketHandshakeRequest(req);
        if (!handshake.ok) {
          respondUpgradeHttp(socket, handshake.status, handshake.message);
          return null;
        }

        const session = sessions.verifySessionRequest(req);
        if (!session) {
          respondUpgradeHttp(socket, 401, 'Unauthorized');
          return null;
        }

        return { session, handshakeKey: handshake.key };
      };

      const commonTcpUpgradeOptions = (sessionId: string) =>
        ({
          allowedOrigins: config.ALLOWED_ORIGINS,
          blockedClientIps: config.TCP_BLOCKED_CLIENT_IPS,
          allowedTargetHosts: config.TCP_ALLOWED_HOSTS,
          allowedTargetPorts: config.TCP_ALLOWED_PORTS,
          allowPrivateIps: config.TCP_ALLOW_PRIVATE_IPS,
          sessionId,
          sessionConnections,
          maxMessageBytes: config.TCP_PROXY_MAX_MESSAGE_BYTES,
          maxTcpBufferedBytes: config.TCP_PROXY_MAX_TCP_BUFFER_BYTES,
          connectTimeoutMs: config.TCP_PROXY_CONNECT_TIMEOUT_MS,
          idleTimeoutMs: config.TCP_PROXY_IDLE_TIMEOUT_MS,
          metrics: metrics.tcpProxy,
        }) as const;

      if (url.pathname === '/tcp' || url.pathname === endpoints.tcp) {
        const auth = authForUpgrade();
        if (!auth) return;

        handleTcpProxyUpgrade(req, socket, head, {
          ...commonTcpUpgradeOptions(auth.session.id),
          handshakeKey: auth.handshakeKey,
          upgradeUrl: url,
        });
        return;
      }

      if (url.pathname === '/tcp-mux' || url.pathname === endpoints.tcpMux) {
        const auth = authForUpgrade();
        if (!auth) return;

        handleTcpMuxUpgrade(req, socket, head, {
          ...commonTcpUpgradeOptions(auth.session.id),
          handshakeKey: auth.handshakeKey,
          upgradeUrl: url,
          maxStreams: config.TCP_MUX_MAX_STREAMS,
          maxStreamBufferedBytes: config.TCP_MUX_MAX_STREAM_BUFFER_BYTES,
          maxFramePayloadBytes: config.TCP_MUX_MAX_FRAME_PAYLOAD_BYTES,
        });
        return;
      }

      respondUpgradeHttp(socket, 404, 'Not Found');
    } catch (err) {
      app.log.error({ err: formatOneLineError(err, 512) }, 'upgrade_unexpected_error');
      if (isUpgradeSocketDestroyed(socket)) return;
      respondUpgradeHttp(socket, 500, 'WebSocket upgrade failed');
    }
  });

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
      for (const socket of upgradeSockets) {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
      upgradeSockets.clear();
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
        let text = '';
        try {
          text = JSON.stringify(obj, null, 2);
        } catch {
          text = '[unserializable]';
        }
        outEl.textContent = text;
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

      const UTF8 = Object.freeze({ encoding: 'utf-8' });
      const textEncoder = new TextEncoder();
      const textDecoder = new TextDecoder(UTF8.encoding);

      function coerceString(input) {
        try {
          return String(input ?? '');
        } catch {
          return '';
        }
      }

      function formatOneLineUtf8(input, maxBytes) {
        if (!Number.isInteger(maxBytes) || maxBytes < 0) return '';
        if (maxBytes === 0) return '';
        const buf = new Uint8Array(maxBytes);
        let written = 0;
        let pendingSpace = false;
        for (const ch of coerceString(input)) {
          const code = ch.codePointAt(0) ?? 0;
          const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
          if (forbidden || /\\s/u.test(ch)) {
            pendingSpace = written > 0;
            continue;
          }
          if (pendingSpace) {
            const spaceRes = textEncoder.encodeInto(' ', buf.subarray(written));
            if (spaceRes.written === 0) break;
            written += spaceRes.written;
            pendingSpace = false;
            if (written >= maxBytes) break;
          }
          const res = textEncoder.encodeInto(ch, buf.subarray(written));
          if (res.written === 0) break;
          written += res.written;
          if (written >= maxBytes) break;
        }
        return written === 0 ? '' : textDecoder.decode(buf.subarray(0, written));
      }

      function safeErrorMessageInput(err) {
        if (err === null) return 'null';

        const t = typeof err;
        if (t === 'string') return err;
        if (t === 'number' || t === 'boolean' || t === 'bigint' || t === 'symbol' || t === 'undefined') return String(err);

        if (t === 'object') {
          try {
            const msg = err && typeof err.message === 'string' ? err.message : null;
            if (msg !== null) return msg;
          } catch {
            // ignore getters throwing
          }
        }

        // Avoid calling toString() on arbitrary objects/functions (can throw / be expensive).
        return 'Error';
      }

      function formatOneLineError(err) {
        return formatOneLineUtf8(safeErrorMessageInput(err), 256) || 'Error';
      }

      (async () => {
        const results = {
          crossOriginIsolated: window.crossOriginIsolated,
          session: { ok: false, error: null },
          sharedArrayBuffer: { ok: false, error: null },
          websocket: { ok: false, echo: null, error: null },
          dnsQuery: { ok: false, meta: null, error: null },
          dnsJson: { ok: false, answer: null, error: null },
        };
        const basePath = location.pathname.replace(/\/+$/, '').replace(/\/e2e$/, '');
        const defaultEndpoints = {
          tcp: basePath + '/tcp',
          dnsQuery: basePath + '/dns-query',
          dnsJson: basePath + '/dns-json',
        };
        let discoveredEndpoints = null;

        try {
          const buf = new SharedArrayBuffer(16);
          results.sharedArrayBuffer.ok = buf.byteLength === 16;
        } catch (err) {
          results.sharedArrayBuffer.error = formatOneLineError(err);
        }

        try {
          const res = await withTimeout(fetch(basePath + '/session', {
            method: 'POST',
            credentials: 'include',
            headers: { 'content-type': 'application/json' },
            body: '{}',
          }), 5000, 'session fetch');
          results.session.ok = res.ok;
          if (!res.ok) results.session.error = 'HTTP ' + res.status;
          const json = await res.json().catch(() => null);
          discoveredEndpoints = json?.endpoints ?? null;
          results.session.endpoints = discoveredEndpoints;
        } catch (err) {
          results.session.error = formatOneLineError(err);
        }

        try {
          const echoPort = Number(requireParam('echoPort'));
          if (!Number.isInteger(echoPort) || echoPort < 1 || echoPort > 65535) {
            throw new Error('Invalid echoPort');
          }

          const wsBase = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host;
          const tcpPath =
            discoveredEndpoints && typeof discoveredEndpoints.tcp === 'string'
              ? discoveredEndpoints.tcp
              : defaultEndpoints.tcp;
          const wsUrl = new URL(tcpPath, wsBase);
          wsUrl.searchParams.set('v', '1');
          wsUrl.searchParams.set('host', '127.0.0.1');
          wsUrl.searchParams.set('port', String(echoPort));
          const echo = await withTimeout(new Promise((resolve, reject) => {
            const MAX_WS_CLOSE_REASON_BYTES = 123;
            const wsSendSafe = (ws, data) => {
              let openState = 1;
              try {
                openState = typeof WebSocket.OPEN === 'number' ? WebSocket.OPEN : 1;
              } catch {
                // ignore (treat as default)
              }
              try {
                // If we can observe readyState, avoid calling send() when not open.
                if (typeof ws?.readyState === 'number' && ws.readyState !== openState) return false;
              } catch {
                return false;
              }
              try {
                ws.send(data);
                return true;
              } catch {
                return false;
              }
            };
            const wsCloseSafe = (ws, code, reason) => {
              let closeFn;
              try {
                closeFn = ws?.close;
              } catch {
                closeFn = null;
              }
              if (typeof closeFn !== 'function') return;
              try {
                if (code === undefined) {
                  closeFn.call(ws);
                  return;
                }
                if (reason === undefined) {
                  closeFn.call(ws, code);
                  return;
                }
                const safeReason = formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES);
                if (!safeReason) {
                  closeFn.call(ws, code);
                  return;
                }
                closeFn.call(ws, code, safeReason);
              } catch {
                // ignore
              }
            };
            const ws = new WebSocket(wsUrl.toString());
            ws.binaryType = 'arraybuffer';
            ws.onopen = () => {
              const data = new TextEncoder().encode('ping');
              if (!wsSendSafe(ws, data)) {
                wsCloseSafe(ws);
                reject(new Error('WebSocket send failed'));
              }
            };
            ws.onerror = () => reject(new Error('WebSocket error'));
            ws.onmessage = (event) => {
              resolve(event.data);
              wsCloseSafe(ws);
            };
          }), 5000, 'WebSocket');

          if (!(echo instanceof ArrayBuffer)) {
            const kind = echo === null ? 'null' : typeof echo;
            throw new Error('Expected ArrayBuffer echo, got type ' + kind);
          }
          const decoded = new TextDecoder().decode(new Uint8Array(echo));
          results.websocket.ok = decoded === 'ping';
          results.websocket.echo = decoded;
        } catch (err) {
          results.websocket.error = formatOneLineError(err);
        }

        try {
          const query = buildDnsQueryA('example.com');
          const dns = base64Url(query.bytes);
          const dnsQueryPath =
            discoveredEndpoints && typeof discoveredEndpoints.dnsQuery === 'string'
              ? discoveredEndpoints.dnsQuery
              : defaultEndpoints.dnsQuery;
          const res = await withTimeout(fetch(dnsQueryPath + '?dns=' + encodeURIComponent(dns)), 5000, 'dns-query fetch');
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
          results.dnsQuery.error = formatOneLineError(err);
        }

        try {
          const dnsJsonPath =
            discoveredEndpoints && typeof discoveredEndpoints.dnsJson === 'string'
              ? discoveredEndpoints.dnsJson
              : defaultEndpoints.dnsJson;
          const res = await withTimeout(fetch(dnsJsonPath + '?name=example.com&type=A'), 5000, 'dns-json fetch');
          const json = await res.json();
          results.dnsJson.answer = json?.Answer?.[0]?.data ?? null;
          results.dnsJson.ok = Boolean(res.ok && json && json.Status === 0 && typeof results.dnsJson.answer === 'string');
        } catch (err) {
          results.dnsJson.error = formatOneLineError(err);
        }

        window.__aeroGatewayE2E = results;
        render(results);
      })();
    </script>
  </body>
</html>`;
}
