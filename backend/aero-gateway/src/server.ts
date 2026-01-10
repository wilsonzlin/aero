import fastify, { type FastifyInstance } from 'fastify';
import fastifyStatic from '@fastify/static';
import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import type { Config } from './config.js';
import { setupCrossOriginIsolation } from './middleware/crossOriginIsolation.js';
import { originGuard } from './middleware/originGuard.js';
import { setupRequestIdHeader } from './middleware/requestId.js';
import { setupRateLimit } from './middleware/rateLimit.js';
import { setupSecurityHeaders } from './middleware/securityHeaders.js';
import { setupMetrics } from './metrics.js';
import { getVersionInfo } from './version.js';

type ServerBundle = {
  app: FastifyInstance;
  markShuttingDown: () => void;
};

function findFrontendDistDir(): string | null {
  const candidates = [
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

  const app = fastify({
    logger: { level: config.LOG_LEVEL },
    requestIdHeader: 'x-request-id',
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

  setupMetrics(app);

  app.get('/healthz', async () => ({ ok: true }));

  app.get('/readyz', async (_request, reply) => {
    if (shuttingDown) return reply.code(503).send({ ok: false });
    return { ok: true };
  });

  app.get('/version', async () => getVersionInfo());

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
  };
}

