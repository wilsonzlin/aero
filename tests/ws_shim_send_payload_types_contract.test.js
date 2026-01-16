import assert from "node:assert/strict";
import test from "node:test";

import WebSocket, { WebSocketServer } from "../scripts/ws-shim.mjs";

test("ws-shim: send() rejects object payloads without throwing", async () => {
  const wss = new WebSocketServer({ port: 0, host: "127.0.0.1" });
  await new Promise((resolve, reject) => {
    wss.once("listening", resolve);
    wss.once("error", reject);
  });

  const addr = wss.address();
  assert(addr && typeof addr === "object");
  const url = `ws://127.0.0.1:${addr.port}/`;

  const ws = new WebSocket(url);

  await new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });

  /** @type {Error | null} */
  let errorEvent = null;
  ws.once("error", (err) => {
    errorEvent = err;
  });

  /** @type {Error | null} */
  let callbackErr = null;
  // Should NOT throw synchronously.
  ws.send(
    /** @type {any} */ ({ nope: true }),
    (err) => {
      callbackErr = err ?? null;
    },
  );

  assert(callbackErr instanceof Error, "send callback should receive an Error");
  assert(errorEvent instanceof Error, "send should emit an error event");

  ws.close();
  await new Promise((resolve) => ws.once("close", resolve));

  await new Promise((resolve) => wss.close(resolve));
});

