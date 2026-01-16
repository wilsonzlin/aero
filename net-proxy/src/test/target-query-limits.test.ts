import test from "node:test";
import assert from "node:assert/strict";
import { WebSocket } from "ws";

import { startProxyServer } from "../server";

function waitForClose(ws: WebSocket): Promise<{ code: number; reason: Buffer }> {
  return new Promise((resolve) => {
    ws.once("close", (code, reason) => resolve({ code, reason }));
  });
}

test("target query limits: rejects overly long host query parameter", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const host = "a".repeat(2000);
    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=${host}&port=80`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);
  } finally {
    await proxy.close();
  }
});

test("target query limits: rejects overly long target query parameter", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const target = `example.com:${"1".repeat(3000)}`;
    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&target=${encodeURIComponent(target)}`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);
  } finally {
    await proxy.close();
  }
});

test("target query parsing: rejects non-decimal port formats", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const ws1 = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=example.com&port=1e3`);
    const closed1 = await waitForClose(ws1);
    assert.equal(closed1.code, 1008);

    const ws2 = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&target=example.com:0x10`);
    const closed2 = await waitForClose(ws2);
    assert.equal(closed2.code, 1008);
  } finally {
    await proxy.close();
  }
});

test("target query parsing: rejects control characters in host", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const badHost = "example.com\nbad";
    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=${encodeURIComponent(badHost)}&port=80`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);
  } finally {
    await proxy.close();
  }
});

