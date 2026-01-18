import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";
import WebSocket from "ws";
import { EventEmitter } from "node:events";

import { wsSendSafe } from "../../../scripts/_shared/ws_safe.js";
import { unrefBestEffort } from "../../../src/unref_safe.js";

import { createProxyServer } from "../src/server.js";
import {
  TCP_MUX_SUBPROTOCOL,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/protocol.js";

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

function waitForWsFailure(ws) {
  return new Promise((resolve) => {
    ws.once("error", () => resolve());
    ws.once("unexpected-response", () => resolve());
    ws.once("close", () => resolve());
  });
}

function wsSendOk(ws, data) {
  assert.ok(wsSendSafe(ws, data));
}

function asBuffer(data) {
  if (Buffer.isBuffer(data)) return data;
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  const t = data === null ? "null" : typeof data;
  throw new TypeError(`Unsupported ws message payload type: ${t}`);
}

function createFrameWaiter(ws) {
  const parser = new TcpMuxFrameParser();

  /** @type {any[]} */
  const buffered = [];
  /** @type {Array<{ predicate: (f:any)=>boolean, resolve:(f:any)=>void, reject:(e:any)=>void, timer:any }>} */
  const pending = [];

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    for (const frame of parser.push(asBuffer(data))) {
      const idx = pending.findIndex((p) => p.predicate(frame));
      if (idx !== -1) {
        const p = pending.splice(idx, 1)[0];
        clearTimeout(p.timer);
        p.resolve(frame);
        continue;
      }
      buffered.push(frame);
    }
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
        let entry;
        const timer = setTimeout(() => {
          const idx = pending.indexOf(entry);
          if (idx !== -1) pending.splice(idx, 1);
          reject(new Error("timeout"));
        }, timeoutMs);
        unrefBestEffort(timer);
        entry = { predicate, resolve, reject, timer };
        pending.push(entry);
      });
    },
  };
}

test("integration: OPEN+DATA roundtrip to echo server (split + concatenated WS messages)", async () => {
  let echoServer;
  let proxy;
  let ws;

  try {
    echoServer = net.createServer((socket) => {
      socket.on("data", (d) => {
        try {
          socket.write(d);
        } catch {
          try {
            socket.destroy();
          } catch {
            // ignore
          }
        }
      });
    });
    const echoPort = await listen(echoServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);
    assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

    // Stream 1: send OPEN + DATA concatenated into a single WebSocket message.
    const open1 = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort }));
    const data1 = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("hello", "utf8"));
    wsSendOk(ws, Buffer.concat([open1, data1]));

    // Stream 2: send OPEN split across two WebSocket messages (tests stream reassembly).
    const open2 = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 2, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort }));
    wsSendOk(ws, open2.subarray(0, 4));
    wsSendOk(ws, open2.subarray(4));
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, Buffer.from("world", "utf8")));

    const d1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === 1);
    const d2 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === 2);
    assert.equal(d1.payload.toString("utf8"), "hello");
    assert.equal(d2.payload.toString("utf8"), "world");

    // Graceful close.
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
    const close1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1);
    assert.equal(decodeTcpMuxClosePayload(close1.payload).flags, TcpMuxCloseFlags.FIN);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
    if (echoServer) await new Promise((resolve) => echoServer.close(resolve));
  }
});

test("integration: OPEN+DATA+FIN in one WS message", async () => {
  let echoServer;
  let proxy;
  let ws;

  try {
    echoServer = net.createServer((socket) => {
      socket.on("data", (d) => {
        try {
          socket.write(d);
        } catch {
          try {
            socket.destroy();
          } catch {
            // ignore
          }
        }
      });
    });
    const echoPort = await listen(echoServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const open = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort }));
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("hello", "utf8"));
    const fin = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));

    wsSendOk(ws, Buffer.concat([open, data, fin]));

    const d1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === 1);
    assert.equal(d1.payload.toString("utf8"), "hello");

    const close1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1, 5000);
    assert.equal(decodeTcpMuxClosePayload(close1.payload).flags, TcpMuxCloseFlags.FIN);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
    if (echoServer) await new Promise((resolve) => echoServer.close(resolve));
  }
});

test("integration: enforces socket-level buffering with STREAM_BUFFER_OVERFLOW (writableLength)", async () => {
  let proxy;
  let ws;

  let createdResolve = null;
  const created = new Promise((resolve) => {
    createdResolve = resolve;
  });

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
      maxStreamBufferedBytes: 32,
      createTcpConnection: () => {
        class FakeTcpSocket extends EventEmitter {
          writableLength = 0;
          setNoDelay() {}
          pause() {}
          resume() {}
          write(chunk) {
            void chunk;
            // Simulate a pathological/custom socket that buffers far more than it reports via
            // backpressure return values.
            this.writableLength += 1024;
            return true;
          }
          end() {
            this.destroy();
          }
          destroy() {
            queueMicrotask(() => this.emit("close"));
          }
        }

        createdResolve?.();
        const socket = new FakeTcpSocket();
        queueMicrotask(() => socket.emit("connect"));
        return socket;
      },
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const open = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: 80 }));
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("a", "utf8"));
    wsSendOk(ws, Buffer.concat([open, data]));

    await Promise.race([
      created,
      new Promise((_, reject) => {
        const t = setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000);
        unrefBestEffort(t);
      }),
    ]);

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW);
    assert.equal(err.message, "stream buffered too much data");

    // Ensure the mux session stays alive.
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: enforces STREAM_BUFFER_OVERFLOW when writableLength getter throws", async () => {
  let proxy;
  let ws;

  let createdResolve = null;
  const created = new Promise((resolve) => {
    createdResolve = resolve;
  });

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
      maxStreamBufferedBytes: 32,
      createTcpConnection: () => {
        class FakeTcpSocket extends EventEmitter {
          setNoDelay() {}
          pause() {}
          resume() {}
          get writableLength() {
            throw new Error("boom");
          }
          write(chunk) {
            void chunk;
            return true;
          }
          end() {
            this.destroy();
          }
          destroy() {
            queueMicrotask(() => this.emit("close"));
          }
        }

        createdResolve?.();
        const socket = new FakeTcpSocket();
        queueMicrotask(() => socket.emit("connect"));
        return socket;
      },
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const open = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: 80 }));
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("a", "utf8"));
    wsSendOk(ws, Buffer.concat([open, data]));

    await Promise.race([
      created,
      new Promise((_, reject) => {
        const t = setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000);
        unrefBestEffort(t);
      }),
    ]);

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW);
    assert.equal(err.message, "stream buffered too much data");

    // Ensure the mux session stays alive.
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: requires aero-tcp-mux-v1 subprotocol", async () => {
  let proxy;
  let ws;

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`);
    await waitForWsFailure(ws);
    assert.notEqual(ws.readyState, WebSocket.OPEN);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: rejects oversized request targets with 414", async () => {
  let proxy;
  let ws;
  let statusCode;

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    const huge = "a".repeat(9_000);
    ws = new WebSocket(`${proxy.url}?token=test-token&x=${huge}`, TCP_MUX_SUBPROTOCOL);
    ws.once("unexpected-response", (_req, res) => {
      statusCode = res.statusCode;
      res.resume();
    });

    await waitForWsFailure(ws);
    assert.equal(statusCode, 414);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: PING -> PONG (same payload)", async () => {
  let proxy;
  let ws;

  try {
    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    const payload = Buffer.from([1, 2, 3, 4]);
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, payload));

    const pong = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);
    assert.deepEqual(pong.payload, payload);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: policy denies private IPs by default", async () => {
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

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: 80 })));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.POLICY_DENIED);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 2, encodeTcpMuxOpenPayload({ host: "192.0.2.1", port: 80 })));
    const errFrame2 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 2);
    const err2 = decodeTcpMuxErrorPayload(errFrame2.payload);
    assert.equal(err2.code, TcpMuxErrorCode.POLICY_DENIED);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 3, encodeTcpMuxOpenPayload({ host: "2001:db8::1", port: 80 })));
    const errFrame3 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 3);
    const err3 = decodeTcpMuxErrorPayload(errFrame3.payload);
    assert.equal(err3.code, TcpMuxErrorCode.POLICY_DENIED);

    // IPv4-mapped IPv6 should not bypass the IPv4 policy.
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 4, encodeTcpMuxOpenPayload({ host: "::ffff:127.0.0.1", port: 80 })));
    const errFrame4 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 4);
    const err4 = decodeTcpMuxErrorPayload(errFrame4.payload);
    assert.equal(err4.code, TcpMuxErrorCode.POLICY_DENIED);

    // Hostnames that resolve only to blocked ranges should also be denied (DNS
    // rebinding / local-network bypass mitigation).
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 5, encodeTcpMuxOpenPayload({ host: "localhost", port: 80 })));
    const errFrame5 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 5);
    const err5 = decodeTcpMuxErrorPayload(errFrame5.payload);
    assert.equal(err5.code, TcpMuxErrorCode.POLICY_DENIED);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
  }
});

test("integration: allowCidrs permits specific private IPv4 destinations", async () => {
  let echoServer;
  let proxy;
  let ws;

  try {
    echoServer = net.createServer((socket) => {
      socket.on("data", (d) => {
        try {
          socket.write(d);
        } catch {
          try {
            socket.destroy();
          } catch {
            // ignore
          }
        }
      });
    });
    const echoPort = await listen(echoServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: false,
      allowCidrs: ["127.0.0.1/32"],
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort })));
    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("ok", "utf8")));

    const d1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === 1);
    assert.equal(d1.payload.toString("utf8"), "ok");
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
    if (echoServer) await new Promise((resolve) => echoServer.close(resolve));
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
      const timer = setTimeout(() => {
        const chunk = Buffer.alloc(64 * 1024, 0x42);
        let remaining = payloadSize;

        const writeMore = () => {
          while (remaining > 0) {
            const n = Math.min(remaining, chunk.length);
            let ok;
            try {
              ok = socket.write(chunk.subarray(0, n));
            } catch {
              try {
                socket.destroy();
              } catch {
                // ignore
              }
              return;
            }
            remaining -= n;
            if (!ok) {
              socket.once("drain", writeMore);
              return;
            }
          }
          try {
            socket.end();
          } catch {
            try {
              socket.destroy();
            } catch {
              // ignore
            }
          }
        };

        writeMore();
      }, 50);
      unrefBestEffort(timer);
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

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: burstPort })));

    // Stop the WS client from reading, causing the server-side send queue to grow.
    ws._socket.pause();

    await new Promise((r) => setTimeout(r, 100));
    assert.ok(proxy.stats.wsBackpressurePauses > 0);

    // Resume and drain.
    ws._socket.resume();

    let receivedLen = 0;
    while (receivedLen < payloadSize) {
      // eslint-disable-next-line no-await-in-loop
      const f = await waiter.waitFor((x) => x.msgType === TcpMuxMsgType.DATA && x.streamId === 1, 5000);
      receivedLen += f.payload.length;
    }

    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1, 5000);
    assert.ok(proxy.stats.wsBackpressureResumes > 0);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
    if (burstServer) await new Promise((resolve) => burstServer.close(resolve));
  }
});

test("integration: backpressure poll resumes TCP reads after WS drains (small thresholds)", async () => {
  // This test exercises a subtle edge case: if the proxy pauses TCP reads due to
  // WS backpressure and then the send queue drains completely while
  // `ws.bufferedAmount` is still high, we still need to resume TCP reads once
  // the WS drains.
  const payloadSize = 256 * 1024;
  let burstServer;
  let proxy;
  let ws;

  try {
    burstServer = net.createServer((socket) => {
      const timer = setTimeout(() => {
        const chunk = Buffer.alloc(64 * 1024, 0x33);
        let remaining = payloadSize;

        const writeMore = () => {
          while (remaining > 0) {
            const n = Math.min(remaining, chunk.length);
            let ok;
            try {
              ok = socket.write(chunk.subarray(0, n));
            } catch {
              try {
                socket.destroy();
              } catch {
                // ignore
              }
              return;
            }
            remaining -= n;
            if (!ok) {
              socket.once("drain", writeMore);
              return;
            }
          }
          try {
            socket.end();
          } catch {
            try {
              socket.destroy();
            } catch {
              // ignore
            }
          }
        };

        writeMore();
      }, 50);
      unrefBestEffort(timer);
    });
    const burstPort = await listen(burstServer);

    proxy = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      // Tiny thresholds make it likely we pause after a single DATA frame.
      wsBackpressureHighWatermarkBytes: 1024,
      wsBackpressureLowWatermarkBytes: 512,
      metricsIntervalMs: 0,
    });

    ws = new WebSocket(`${proxy.url}?token=test-token`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);
    await waitForWsOpen(ws);

    wsSendOk(ws, encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: burstPort })));

    // Stop the WS client from reading, causing the server-side socket buffer to
    // fill up quickly.
    ws._socket.pause();

    await new Promise((r) => setTimeout(r, 100));
    assert.ok(proxy.stats.wsBackpressurePauses > 0);

    ws._socket.resume();

    let receivedLen = 0;
    while (receivedLen < payloadSize) {
      // eslint-disable-next-line no-await-in-loop
      const f = await waiter.waitFor((x) => x.msgType === TcpMuxMsgType.DATA && x.streamId === 1, 5000);
      receivedLen += f.payload.length;
    }

    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1, 5000);
    assert.ok(proxy.stats.wsBackpressureResumes > 0);
  } finally {
    if (ws) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    if (proxy) await proxy.close();
    if (burstServer) await new Promise((resolve) => burstServer.close(resolve));
  }
});
