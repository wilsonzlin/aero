import type { FastifyInstance } from 'fastify';

export function setupCrossOriginIsolation(app: FastifyInstance): void {
  app.addHook('onSend', async (_request, reply, payload) => {
    reply.header('cross-origin-opener-policy', 'same-origin');
    reply.header('cross-origin-embedder-policy', 'require-corp');
    reply.header('cross-origin-resource-policy', 'same-origin');
    reply.header('origin-agent-cluster', '?1');
    return payload;
  });
}

