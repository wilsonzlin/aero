import assert from "node:assert/strict";
import test from "node:test";

import WebSocket, { WebSocketServer } from "../tools/minimal_ws.js";

test("minimal_ws: send() rejects object payloads", async () => {
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

  assert.throws(() => ws.send(/** @type {any} */ ({ nope: true })), TypeError);

  ws.close();
  await new Promise((resolve) => ws.once("close", resolve));
  await new Promise((resolve) => wss.close(resolve));
});

