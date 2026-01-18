import test from "node:test";
import assert from "node:assert/strict";
import { WebSocket } from "ws";
import { PassThrough } from "node:stream";
import { once } from "node:events";
import { startProxyServer } from "../server";
import { unrefBestEffort } from "../unrefSafe";

async function captureUpgradeResponse(server: import("node:http").Server, req: any): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");

  server.emit("upgrade", req, socket as any, Buffer.alloc(0));
  await ended;
  try {
    socket.destroy();
  } catch {
    // ignore
  }
  return Buffer.concat(chunks).toString("utf8");
}

test("websocket upgrade rejects overly long request URLs (414)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const url = `ws://127.0.0.1:${addr.port}/${"a".repeat(9_000)}`;
    const ws = new WebSocket(url);

    const statusCode = await new Promise<number>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket rejection")), 2_000);
      unrefBestEffort(timeout);

      ws.once("unexpected-response", (_req, res) => {
        clearTimeout(timeout);
        resolve(res.statusCode ?? 0);
      });
      ws.once("open", () => {
        clearTimeout(timeout);
        reject(new Error("unexpectedly opened websocket"));
      });
      ws.once("error", () => {
        // ignore: ws will usually emit error after unexpected-response
      });
    });

    ws.terminate();
    assert.equal(statusCode, 414);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade rejects whitespace-padded request URLs (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  try {
    const res = await captureUpgradeResponse(proxy.server, { url: " /tcp", headers: {} });
    assert.ok(res.startsWith("HTTP/1.1 400 "), res);
  } finally {
    await proxy.close();
  }
});

