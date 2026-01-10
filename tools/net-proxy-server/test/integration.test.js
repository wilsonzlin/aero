import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";
import WebSocket from "ws";
import { createProxyServer } from "../src/server.js";
import { decodeFrame, encodeData, encodeOpenRequest, FrameType } from "../src/protocol.js";

function listen(server, host = "127.0.0.1") {
  return new Promise((resolve) => {
    server.listen(0, host, () => resolve(server.address().port));
  });
}

function waitForWsOpen(ws) {
  return new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });
}

function createFrameWaiter(ws) {
  /** @type {any[]} */
  const buffered = [];
  /** @type {Array<{ predicate: (f:any)=>boolean, resolve:(f:any)=>void, reject:(e:any)=>void, timer:any }>} */
  const pending = [];
  ws.on("message", (data) => {
    const frame = decodeFrame(data);
    const idx = pending.findIndex((p) => p.predicate(frame));
    if (idx !== -1) {
      const p = pending.splice(idx, 1)[0];
      clearTimeout(p.timer);
      p.resolve(frame);
      return;
    }
    buffered.push(frame);
  });
  return {
    waitFor(predicate, timeoutMs = 2000) {
      return new Promise((resolve, reject) => {
        const bufferedIdx = buffered.findIndex(predicate);
        if (bufferedIdx !== -1) {
          const frame = buffered.splice(bufferedIdx, 1)[0];
          resolve(frame);
          return;
        }
        const timer = setTimeout(() => reject(new Error("timeout")), timeoutMs);
        pending.push({ predicate, resolve, reject, timer });
      });
    },
  };
}

test("integration: multiplexed streams + echo + clean close", async () => {
  let echoServer;
  let proxy;
  let ws;

  try {
    echoServer = net.createServer((socket) => {
      socket.on("data", (d) => socket.write(d));
    });
    const echoPort = await listen(echoServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const dstIp = new Uint8Array([127, 0, 0, 1]);
    ws.send(encodeOpenRequest(1, dstIp, echoPort));
    ws.send(encodeOpenRequest(2, dstIp, echoPort));

    await waiter.waitFor((f) => f.type === FrameType.OPEN && f.connectionId === 1 && f.kind === "ack");
    await waiter.waitFor((f) => f.type === FrameType.OPEN && f.connectionId === 2 && f.kind === "ack");

    ws.send(encodeData(1, Buffer.from("hello")));
    ws.send(encodeData(2, Buffer.from("world")));

    const d1 = await waiter.waitFor((f) => f.type === FrameType.DATA && f.connectionId === 1);
    const d2 = await waiter.waitFor((f) => f.type === FrameType.DATA && f.connectionId === 2);
    assert.equal(Buffer.from(d1.data).toString("utf8"), "hello");
    assert.equal(Buffer.from(d2.data).toString("utf8"), "world");

    // Large payload round-trip (>= 1MB) using chunked DATA frames.
    const big = Buffer.alloc(1024 * 1024, 0x5a);
    const chunkSize = 16 * 1024;
    for (let off = 0; off < big.length; off += chunkSize) {
      ws.send(encodeData(1, big.subarray(off, Math.min(big.length, off + chunkSize))));
    }

    /** @type {Buffer[]} */
    const received = [];
    let receivedLen = 0;
    while (receivedLen < big.length) {
      // eslint-disable-next-line no-await-in-loop
      const f = await waiter.waitFor((x) => x.type === FrameType.DATA && x.connectionId === 1, 5000);
      const b = Buffer.from(f.data);
      received.push(b);
      receivedLen += b.length;
    }
    const roundTrip = Buffer.concat(received, receivedLen).subarray(0, big.length);
    assert.deepEqual(roundTrip, big);

    ws.send(new Uint8Array([3, 0, 0, 0, 1])); // CLOSE connId=1
    await waiter.waitFor((f) => f.type === FrameType.CLOSE && f.connectionId === 1);
  } finally {
    if (ws) ws.terminate();
    if (proxy) await proxy.close();
    if (echoServer) await new Promise((resolve) => echoServer.close(resolve));
  }
});

test("integration: policy denies private IPv4 by default", async () => {
  let proxy;
  let ws;

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: false,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const dstIp = new Uint8Array([127, 0, 0, 1]);
    ws.send(encodeOpenRequest(1, dstIp, 80));
    const err = await waiter.waitFor((f) => f.type === FrameType.ERROR && f.connectionId === 1);
    assert.equal(err.code, 3); // POLICY_DENIED
  } finally {
    if (ws) ws.terminate();
    if (proxy) await proxy.close();
  }
});

test("integration: TCP->WS backpressure pauses TCP read (>=1MB)", async () => {
  const payloadSize = 2 * 1024 * 1024;
  let burstServer;
  let proxy;
  let ws;

  try {
    burstServer = net.createServer((socket) => {
      // Give the test client time to pause its WebSocket socket before we start
      // flooding data.
      setTimeout(() => {
        const chunk = Buffer.alloc(64 * 1024, 0x42);
        let remaining = payloadSize;

        const writeMore = () => {
          while (remaining > 0) {
            const n = Math.min(remaining, chunk.length);
            const ok = socket.write(chunk.subarray(0, n));
            remaining -= n;
            if (!ok) {
              socket.once("drain", writeMore);
              return;
            }
          }
          socket.end();
        };

        writeMore();
      }, 50);
    });
    const burstPort = await listen(burstServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      wsBackpressureHighWatermarkBytes: 128 * 1024,
      wsBackpressureLowWatermarkBytes: 64 * 1024,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    ws.send(encodeOpenRequest(1, new Uint8Array([127, 0, 0, 1]), burstPort));
    await waiter.waitFor((f) => f.type === FrameType.OPEN && f.connectionId === 1 && f.kind === "ack");

    // Stop the WS client from reading, causing the server-side send queue to grow.
    ws._socket.pause();

    await new Promise((r) => setTimeout(r, 100));
    assert.ok(proxy.stats.wsBackpressurePauses > 0);

    // Resume and drain.
    ws._socket.resume();

    let receivedLen = 0;
    while (receivedLen < payloadSize) {
      // eslint-disable-next-line no-await-in-loop
      const f = await waiter.waitFor((x) => x.type === FrameType.DATA && x.connectionId === 1, 5000);
      receivedLen += f.data.byteLength;
    }

    await waiter.waitFor((f) => f.type === FrameType.CLOSE && f.connectionId === 1, 5000);
    assert.ok(proxy.stats.wsBackpressureResumes > 0);
  } finally {
    if (ws) ws.terminate();
    if (proxy) await proxy.close();
    if (burstServer) await new Promise((resolve) => burstServer.close(resolve));
  }
});
