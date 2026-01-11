import test from "node:test";
import assert from "node:assert/strict";
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
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);
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
      encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })
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
