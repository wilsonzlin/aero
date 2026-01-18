import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { once } from "node:events";
import test from "node:test";
import type http from "node:http";

import { buildServer } from "../src/server.js";
import { makeTestConfig, TEST_WS_HANDSHAKE_HEADERS } from "./testConfig.js";

async function captureUpgradeResponse(app: import("fastify").FastifyInstance, req: http.IncomingMessage): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");

  app.server.emit("upgrade", req, socket, Buffer.alloc(0));
  await ended;
  try {
    socket.destroy();
  } catch {
    // ignore
  }
  return Buffer.concat(chunks).toString("utf8");
}

test("server upgrade routing returns 500 on unexpected throws (and stays alive)", async () => {
  const { app } = buildServer(makeTestConfig());

  await app.ready();
  try {
    const req = {
      headers: { ...TEST_WS_HANDSHAKE_HEADERS },
    } as unknown as http.IncomingMessage;
    Object.defineProperty(req, "url", {
      get() {
        throw new Error("boom");
      },
    });

    const res = await captureUpgradeResponse(app, req);
    assert.ok(res.startsWith("HTTP/1.1 500 "), res);
    assert.ok(res.includes("WebSocket upgrade failed"), res);

    const health = await app.inject({ method: "GET", url: "/healthz" });
    assert.equal(health.statusCode, 200);
    assert.deepEqual(health.json(), { ok: true });
  } finally {
    await app.close();
  }
});

