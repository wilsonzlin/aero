import test from "node:test";
import assert from "node:assert/strict";
import { EventEmitter, once } from "node:events";
import net from "node:net";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";
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
  type TcpMuxFrame
} from "../tcpMuxProtocol";

async function startTcpEchoServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = net.createServer((socket) => {
    socket.on("error", () => {
      // Ignore socket errors for test shutdown.
    });
    socket.pipe(socket);
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");

  return {
    port: addr.port,
    close: async () => new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())))
  };
}

async function openWebSocket(url: string, protocol: string): Promise<WebSocket> {
  const ws = new WebSocket(url, protocol);
  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket open")), 2_000);
    timeout.unref();
    ws.once("open", () => {
      clearTimeout(timeout);
      resolve();
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
  return ws;
}

async function waitForClose(ws: WebSocket, timeoutMs = 2_000): Promise<{ code: number; reason: string }> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for close")), timeoutMs);
    timeout.unref();
    ws.once("close", (code, reason) => {
      clearTimeout(timeout);
      resolve({ code, reason: reason.toString() });
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

type FrameWaiter = {
  waitFor: (predicate: (frame: TcpMuxFrame) => boolean, timeoutMs?: number) => Promise<TcpMuxFrame>;
};

function createFrameWaiter(ws: WebSocket): FrameWaiter {
  const parser = new TcpMuxFrameParser();
  const backlog: TcpMuxFrame[] = [];
  const waiters: Array<{
    predicate: (frame: TcpMuxFrame) => boolean;
    resolve: (frame: TcpMuxFrame) => void;
    reject: (err: Error) => void;
    timer: NodeJS.Timeout;
  }> = [];

  ws.on("message", (data, isBinary) => {
    assert.equal(isBinary, true);
    const buf = Buffer.isBuffer(data)
      ? data
      : Array.isArray(data)
        ? Buffer.concat(data)
        : Buffer.from(data as ArrayBuffer);
    const frames = parser.push(buf);
    for (const frame of frames) {
      const waiterIdx = waiters.findIndex((w) => w.predicate(frame));
      if (waiterIdx !== -1) {
        const [w] = waiters.splice(waiterIdx, 1);
        clearTimeout(w!.timer);
        w!.resolve(frame);
        continue;
      }
      backlog.push(frame);
    }
  });

  const waitFor = (predicate: (frame: TcpMuxFrame) => boolean, timeoutMs = 2_000): Promise<TcpMuxFrame> => {
    const idx = backlog.findIndex(predicate);
    if (idx !== -1) {
      return Promise.resolve(backlog.splice(idx, 1)[0]!);
    }

    return new Promise<TcpMuxFrame>((resolve, reject) => {
      let waiter: (typeof waiters)[number];
      const timer = setTimeout(() => {
        const i = waiters.indexOf(waiter);
        if (i !== -1) waiters.splice(i, 1);
        reject(new Error("timeout waiting for frame"));
      }, timeoutMs);
      timer.unref();

      waiter = { predicate, resolve, reject, timer };
      waiters.push(waiter);
    });
  };

  return { waitFor };
}

async function waitForEcho(waiter: FrameWaiter, streamId: number, expected: Buffer): Promise<void> {
  const chunks: Buffer[] = [];
  let total = 0;
  while (total < expected.length) {
    const frame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === streamId);
    chunks.push(frame.payload);
    total += frame.payload.length;
  }

  const received = Buffer.concat(chunks);
  assert.deepEqual(received, expected);
}

test("tcp-mux upgrade requires aero-tcp-mux-v1 subprotocol", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    for (const protocol of [undefined, "bogus-subprotocol"]) {
      const ws = protocol
        ? new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, protocol)
        : new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`);

      const statusCode = await new Promise<number>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket rejection")), 2_000);
        timeout.unref();

        ws.once("open", () => {
          clearTimeout(timeout);
          ws.close();
          reject(new Error("expected websocket upgrade to be rejected"));
        });

        ws.once("unexpected-response", (_req, res) => {
          clearTimeout(timeout);
          res.resume();
          resolve(res.statusCode ?? 0);
        });

        ws.once("error", (err) => {
          clearTimeout(timeout);
          reject(err);
        });
      });

      assert.equal(statusCode, 400);
    }
  } finally {
    await proxy.close();
  }
});

test("tcp-mux negotiates aero-tcp-mux-v1 subprotocol when multiple are offered", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, ["bogus-subprotocol", TCP_MUX_SUBPROTOCOL]);
    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket open")), 2_000);
      timeout.unref();
      ws!.once("open", () => {
        clearTimeout(timeout);
        resolve();
      });
      ws!.once("error", (err) => {
        clearTimeout(timeout);
        reject(err);
      });
    });

    assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

    const waiter = createFrameWaiter(ws);
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux closes with 1003 (unsupported data) on WebSocket text messages", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const closePromise = waitForClose(ws);
    ws.send("hello");
    const closed = await closePromise;
    assert.equal(closed.code, 1003);
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux closes with 1002 (protocol error) on oversized frame length", async () => {
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    tcpMuxMaxFramePayloadBytes: 4
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const closePromise = waitForClose(ws);

    const header = Buffer.alloc(9);
    header.writeUInt8(TcpMuxMsgType.DATA, 0);
    header.writeUInt32BE(1, 1);
    header.writeUInt32BE(5, 5);
    ws.send(header);

    const closed = await closePromise;
    assert.equal(closed.code, 1002);
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux ignores OPEN metadata payload", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port, metadata: "{\"hello\":\"world\"}" })
    );
    const payload = Buffer.from("metadata-ok");
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);
    ws.send(Buffer.concat([open, data]));

    await waitForEcho(waiter, 1, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux relay echoes bytes on multiple concurrent streams", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open1 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "[127.0.0.1]", port: echoServer.port })
    );
    const open2 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );

    const payload1 = Buffer.from("stream-one");
    const payload2 = Buffer.from("stream-two");
    const data1 = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload1);
    const data2 = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, payload2);

    // Exercise "multiple frames per message" and "frame split across messages".
    ws.send(Buffer.concat([open1, open2, data2]));
    ws.send(data1.subarray(0, 4));
    ws.send(data1.subarray(4));

    await Promise.all([waitForEcho(waiter, 1, payload1), waitForEcho(waiter, 2, payload2)]);

    const close1 = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
    const close2 = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 2, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
    ws.send(Buffer.concat([close1, close2]));

    const serverClose1 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1);
    const serverClose2 = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 2);
    assert.ok((decodeTcpMuxClosePayload(serverClose1.payload).flags & TcpMuxCloseFlags.FIN) !== 0);
    assert.ok((decodeTcpMuxClosePayload(serverClose2.payload).flags & TcpMuxCloseFlags.FIN) !== 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux policy denials are returned as ERROR without closing the websocket", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: false,
    allow: `127.0.0.1:${echoServer.port}`
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const deniedPort = echoServer.port === 65535 ? 65534 : echoServer.port + 1;

    const openAllowed = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const openDenied = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: deniedPort })
    );

    const payload = Buffer.from("still-works");
    const dataAllowed = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);

    ws.send(Buffer.concat([openAllowed, openDenied, dataAllowed]));

    const errorPromise = waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 2);
    const echoPromise = waitForEcho(waiter, 1, payload);

    const [errFrame] = await Promise.all([errorPromise, echoPromise]);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.POLICY_DENIED);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux enforces max streams per websocket with STREAM_LIMIT_EXCEEDED", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    tcpMuxMaxStreams: 1
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const openAllowed = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const openDenied = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );

    const payload = Buffer.from("one-stream");
    const dataAllowed = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);

    ws.send(Buffer.concat([openAllowed, openDenied, dataAllowed]));

    const errorPromise = waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 2);
    const echoPromise = waitForEcho(waiter, 1, payload);

    const [errFrame] = await Promise.all([errorPromise, echoPromise]);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux enforces per-stream buffered bytes with STREAM_BUFFER_OVERFLOW", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    tcpMuxMaxStreamBufferedBytes: 1
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const openOverflow = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const dataOverflow = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from([1, 2]));
    ws.send(Buffer.concat([openOverflow, dataOverflow]));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW);

    const openOk = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("a");
    const dataOk = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, payload);
    ws.send(Buffer.concat([openOk, dataOk]));

    await waitForEcho(waiter, 2, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux enforces socket-level buffering with STREAM_BUFFER_OVERFLOW", async () => {
  const echoServer = await startTcpEchoServer();

  let createdResolve: (() => void) | null = null;
  const created = new Promise<void>((resolve) => {
    createdResolve = resolve;
  });

  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    tcpMuxMaxStreamBufferedBytes: 32,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        writableLength = 0;
        setNoDelay() {}
        pause() {}
        resume() {}
        write(chunk: unknown) {
          const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as any);
          this.writableLength += buf.length;
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
      return socket as unknown as net.Socket;
    },
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    ws.send(open);

    await Promise.race([
      created,
      new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000)),
    ]);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("x".repeat(1024), "utf8")));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW);
    assert.equal(err.message, "stream buffered too much data");

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux enforces STREAM_BUFFER_OVERFLOW if socket.writableLength getter throws", async () => {
  const echoServer = await startTcpEchoServer();

  let createdResolve: (() => void) | null = null;
  const created = new Promise<void>((resolve) => {
    createdResolve = resolve;
  });

  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    tcpMuxMaxStreamBufferedBytes: 32,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        setNoDelay() {}
        pause() {}
        resume() {}
        get writableLength() {
          throw new Error("boom");
        }
        write(_chunk: unknown) {
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
      return socket as unknown as net.Socket;
    },
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    ws.send(open);

    await Promise.race([
      created,
      new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000)),
    ]);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("a")));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW);
    assert.equal(err.message, "stream buffered too much data");

    // Ensure the mux WS is still alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux returns DIAL_FAILED when createTcpConnection throws (and keeps the websocket alive)", async () => {
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    createTcpConnection: () => {
      throw new Error("boom");
    },
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: 80 })));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.DIAL_FAILED);

    // Ensure the mux WS is still alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([4])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux replies to PING with PONG (same payload)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const payload = Buffer.from([1, 2, 3, 4, 5]);
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, payload));

    const pong = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);
    assert.deepEqual(pong.payload, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux returns UNKNOWN_STREAM for DATA on unopened stream without closing the websocket", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 123, Buffer.from("hi")));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 123);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.UNKNOWN_STREAM);

    // Ensure the mux WS is still alive by pinging.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([9])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux rejects OPEN with stream_id=0 as PROTOCOL_ERROR without closing the websocket", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 0, Buffer.alloc(0)));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 0);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure the mux WS is still alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([7])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux rejects malformed OPEN payload as PROTOCOL_ERROR without closing the websocket", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    // Craft a truncated OPEN payload: metadata_len=2 but omit the metadata bytes.
    const hostBytes = Buffer.from("127.0.0.1", "utf8");
    const openPayload = Buffer.alloc(2 + hostBytes.length + 2 + 2);
    let off = 0;
    openPayload.writeUInt16BE(hostBytes.length, off);
    off += 2;
    hostBytes.copy(openPayload, off);
    off += hostBytes.length;
    openPayload.writeUInt16BE(echoServer.port, off);
    off += 2;
    openPayload.writeUInt16BE(2, off);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, openPayload));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // The stream was never created; DATA should return UNKNOWN_STREAM.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("should-not-exist")));
    const unknown = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const unknownErr = decodeTcpMuxErrorPayload(unknown.payload);
    assert.equal(unknownErr.code, TcpMuxErrorCode.UNKNOWN_STREAM);

    // Ensure the mux WS remains alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([5])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    // Ensure other streams still work.
    const open2 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("ok");
    ws.send(Buffer.concat([open2, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, payload)]));
    await waitForEcho(waiter, 2, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux rejects OPEN with invalid port=0 as PROTOCOL_ERROR without closing the websocket", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const hostBytes = Buffer.from("127.0.0.1", "utf8");
    const openPayload = Buffer.alloc(2 + hostBytes.length + 2 + 2);
    let off = 0;
    openPayload.writeUInt16BE(hostBytes.length, off);
    off += 2;
    hostBytes.copy(openPayload, off);
    off += hostBytes.length;
    openPayload.writeUInt16BE(0, off); // invalid port
    off += 2;
    openPayload.writeUInt16BE(0, off); // metadata_len

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, openPayload));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure the mux WS remains alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([6])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    // Ensure other streams still work.
    const open2 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("ok");
    ws.send(Buffer.concat([open2, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, payload)]));
    await waitForEcho(waiter, 2, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux rejects OPEN with empty host as PROTOCOL_ERROR without closing the websocket", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    // OPEN payload with host_len=0.
    const openPayload = Buffer.alloc(2 + 0 + 2 + 2);
    openPayload.writeUInt16BE(0, 0); // host_len
    openPayload.writeUInt16BE(echoServer.port, 2); // port
    openPayload.writeUInt16BE(0, 4); // metadata_len

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, 1, openPayload));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure the mux WS remains alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([7])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    // Ensure other streams still work.
    const open2 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      2,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("ok");
    ws.send(Buffer.concat([open2, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 2, payload)]));
    await waitForEcho(waiter, 2, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux returns PROTOCOL_ERROR for duplicate OPEN stream_id without breaking the existing stream", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open1 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const open1Dup = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("dup-open-still-works");
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);

    ws.send(Buffer.concat([open1, open1Dup, data]));

    const errPromise = waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const echoPromise = waitForEcho(waiter, 1, payload);

    const [errFrame] = await Promise.all([errPromise, echoPromise]);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux stream_id is unique for the lifetime of the websocket", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open1 = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("first-stream");
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);
    ws.send(Buffer.concat([open1, data]));

    await waitForEcho(waiter, 1, payload);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1);

    // Attempt to reuse stream_id=1 after it has closed; this should still be rejected.
    ws.send(open1);
    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure mux WS still works after the rejected OPEN.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux supports CLOSE(RST) without killing the websocket", async () => {
  const server = net.createServer((socket) => {
    socket.on("error", () => {
      // ignore
    });
    socket.on("data", (data) => socket.write(data));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");

  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    const connPromise = once(server, "connection") as Promise<[net.Socket]>;

    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: addr.port })
    );
    const payload = Buffer.from("rst-test");
    const data = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload);
    ws.send(Buffer.concat([open, data]));
    await waitForEcho(waiter, 1, payload);

    const [serverSocket] = await connPromise;
    const serverClosePromise = waitForServerSocketClose(serverSocket);

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.RST)));

    await serverClosePromise;

    // Ensure the mux WS is still alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([11])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    // Further DATA on the stream should yield UNKNOWN_STREAM (stream already destroyed).
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("after-rst")));
    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.UNKNOWN_STREAM);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
  }
});

test("tcp-mux propagates CLOSE(FIN) as TCP FIN while still allowing server->client DATA", async () => {
  const server = net.createServer({ allowHalfOpen: true }, (socket) => {
    socket.on("error", () => {
      // ignore
    });
    socket.on("data", (data) => socket.write(data));
    socket.on("end", () => {
      socket.write("bye");
      socket.end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");

  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    const connPromise = once(server, "connection") as Promise<[net.Socket]>;

    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: addr.port })
    );
    const payload = Buffer.from("fin-half-close");
    ws.send(Buffer.concat([open, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload)]));
    await waitForEcho(waiter, 1, payload);

    const [serverSocket] = await connPromise;
    const serverSawFin = once(serverSocket, "end");

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));

    // Ensure the FIN reached the TCP server.
    await Promise.race([
      serverSawFin,
      new Promise<never>((_resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for TCP FIN")), 2_000);
        timeout.unref();
      })
    ]);

    // The TCP server sends "bye" after FIN; ensure we still receive it.
    const byeFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === 1);
    assert.deepEqual(byeFrame.payload, Buffer.from("bye"));

    // The server also closes its side; ensure we receive CLOSE(FIN).
    const closeFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.CLOSE && f.streamId === 1);
    assert.ok((decodeTcpMuxClosePayload(closeFrame.payload).flags & TcpMuxCloseFlags.FIN) !== 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
  }
});

test("tcp-mux returns PROTOCOL_ERROR for invalid CLOSE payload length without killing the stream", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    ws.send(open);

    const payload1 = Buffer.from("still-open-1");
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload1));
    await waitForEcho(waiter, 1, payload1);

    // CLOSE payload must be exactly 1 byte. Send an empty payload to trigger a stream-level protocol error.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, Buffer.alloc(0)));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure the stream is still usable after the invalid CLOSE.
    const payload2 = Buffer.from("still-open-2");
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload2));
    await waitForEcho(waiter, 1, payload2);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp-mux returns PROTOCOL_ERROR for unknown msg_type without closing the websocket", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    // msg_type=99 is not defined by `aero-tcp-mux-v1`.
    const unknown = Buffer.alloc(9);
    unknown.writeUInt8(99, 0);
    unknown.writeUInt32BE(0, 1);
    unknown.writeUInt32BE(0, 5);
    ws.send(unknown);

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 0);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    // Ensure the mux WS is still alive.
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([3])));
    await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.PONG && f.streamId === 0);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp-mux rejects DATA after client FIN with PROTOCOL_ERROR", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    const waiter = createFrameWaiter(ws);

    const open = encodeTcpMuxFrame(
      TcpMuxMsgType.OPEN,
      1,
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
    );
    const payload = Buffer.from("fin-test");
    ws.send(Buffer.concat([open, encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, payload)]));
    await waitForEcho(waiter, 1, payload);

    const fin = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
    const dataAfterFin = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("should-error"));
    ws.send(Buffer.concat([fin, dataAfterFin]));

    const errFrame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.ERROR && f.streamId === 1);
    const err = decodeTcpMuxErrorPayload(errFrame.payload);
    assert.equal(err.code, TcpMuxErrorCode.PROTOCOL_ERROR);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
    await echoServer.close();
  }
});

async function waitForServerSocketClose(socket: net.Socket, timeoutMs = 2_000): Promise<void> {
  const closePromise = once(socket, "close");
  await Promise.race([
    closePromise,
    new Promise<never>((_resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("timeout waiting for TCP socket close")), timeoutMs);
      timeout.unref();
    })
  ]);
}
