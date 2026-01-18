import test from "node:test";
import assert from "node:assert/strict";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";
import { unrefBestEffort } from "../unrefSafe";

test("tcp-mux upgrade rejects oversized Sec-WebSocket-Protocol headers (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    // Large enough to exceed MAX_SEC_WEBSOCKET_PROTOCOL_LEN, but within typical HTTP header limits.
    const protocols = Array.from({ length: 100 }, (_v, i) => `p${String(i).padStart(3, "0")}${"x".repeat(46)}`);
    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp-mux`, protocols);

    const statusCode = await new Promise<number>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket rejection")), 2_000);
      unrefBestEffort(timeout);

      ws.once("unexpected-response", (_req, res) => {
        clearTimeout(timeout);
        res.resume();
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
    assert.equal(statusCode, 400);
  } finally {
    await proxy.close();
  }
});

