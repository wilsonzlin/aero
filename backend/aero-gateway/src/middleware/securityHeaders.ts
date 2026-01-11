import type { FastifyInstance } from 'fastify';

export function setupSecurityHeaders(app: FastifyInstance): void {
  app.addHook('onSend', async (_request, reply, payload) => {
    reply.header('x-content-type-options', 'nosniff');
    reply.header('referrer-policy', 'no-referrer');
    reply.header('x-frame-options', 'DENY');
    // Baseline: allow only explicitly listed powerful features.
    // Note that Permissions-Policy does not grant permission on its own; it only
    // controls whether the origin is allowed to request it.
    reply.header('permissions-policy', 'camera=(), geolocation=(), microphone=(self), usb=(self)');
    return payload;
  });
}
