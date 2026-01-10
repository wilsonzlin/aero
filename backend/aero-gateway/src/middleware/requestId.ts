import type { FastifyInstance } from 'fastify';

export function setupRequestIdHeader(app: FastifyInstance): void {
  app.addHook('onSend', async (request, reply, payload) => {
    reply.header('x-request-id', request.id);
    return payload;
  });
}

