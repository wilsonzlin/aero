import type { FastifyInstance } from 'fastify';

export function setupSecurityHeaders(app: FastifyInstance): void {
  app.addHook('onSend', async (_request, reply, payload) => {
    reply.header('x-content-type-options', 'nosniff');
    reply.header('referrer-policy', 'no-referrer');
    reply.header('x-frame-options', 'DENY');
    // Baseline: explicitly disable high-risk powerful features unless/when the
    // frontend opts in (e.g. microphone for audio input).
    reply.header('permissions-policy', 'camera=(), microphone=(), geolocation=()');
    return payload;
  });
}
