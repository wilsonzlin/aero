import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { once } from "node:events";
import { describe, it } from "node:test";

import { WebSocketServer } from "ws";

import { handleTcpMuxUpgrade } from "../src/routes/tcpMux.js";
import { decodeTcpMuxOpenPayload as decodeGatewayTcpMuxOpenPayload } from "../src/protocol/tcpMux.js";

import {
  decodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
  TCP_MUX_SUBPROTOCOL,
  TCP_MUX_HEADER_BYTES,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  WebSocketTcpMuxProxyClient,
} from "../../../web/src/net/tcpMuxProxy.ts";

import { WebSocketTcpProxyMuxClient, type TcpProxyEvent } from "../../../web/src/net/tcpProxy.ts";
import { unrefBestEffort } from "../../../src/unref_safe.js";

function concatBytes(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

async function listen(server: http.Server | net.Server, host?: string): Promise<number> {
  server.listen(0, host);
  await once(server, "listening");
  const addr = server.address();
  if (addr && typeof addr === "object") return addr.port;
  throw new Error("Expected server to bind to an ephemeral port");
}

async function closeServer(server: http.Server | net.Server): Promise<void> {
  server.close();
  await once(server, "close");
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs);
    // Avoid keeping the process open just for the timer in case the promise
    // settles quickly.
    unrefBestEffort(timer);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

describe("tcp-mux browser client codec", () => {
  it("parses multiple concatenated frames", () => {
    const f1 = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, new Uint8Array([1, 2, 3]));
    const f2 = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, 2, new Uint8Array([0x01]));
    const stream = concatBytes([f1, f2]);

    const parser = new TcpMuxFrameParser();
    const frames = parser.push(stream);
    assert.equal(frames.length, 2);
    assert.equal(frames[0]!.msgType, TcpMuxMsgType.DATA);
    assert.equal(frames[0]!.streamId, 1);
    assert.deepEqual(frames[0]!.payload, new Uint8Array([1, 2, 3]));
    assert.equal(frames[1]!.msgType, TcpMuxMsgType.CLOSE);
    assert.equal(frames[1]!.streamId, 2);
    assert.deepEqual(frames[1]!.payload, new Uint8Array([0x01]));
    parser.finish();
  });

  it("parses a frame split across chunks", () => {
    const f = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 42, new Uint8Array([9, 8, 7, 6]));
    const a = f.subarray(0, 4);
    const b = f.subarray(4);

    const parser = new TcpMuxFrameParser();
    assert.deepEqual(parser.push(a), []);
    const frames = parser.push(b);
    assert.equal(frames.length, 1);
    assert.equal(frames[0]!.msgType, TcpMuxMsgType.DATA);
    assert.equal(frames[0]!.streamId, 42);
    assert.deepEqual(frames[0]!.payload, new Uint8Array([9, 8, 7, 6]));
    parser.finish();
  });

  it("throws on truncated payload", () => {
    const f = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, new Uint8Array([1, 2, 3]));
    const truncated = f.subarray(0, f.byteLength - 1);
    const parser = new TcpMuxFrameParser();
    assert.deepEqual(parser.push(truncated), []);
    assert.throws(() => parser.finish(), /truncated tcp-mux frame stream/i);
  });

  it("throws on payload length larger than max", () => {
    const header = new Uint8Array(TCP_MUX_HEADER_BYTES);
    const dv = new DataView(header.buffer);
    dv.setUint8(0, TcpMuxMsgType.DATA);
    dv.setUint32(1, 123, false);
    dv.setUint32(5, 1024, false);

    const parser = new TcpMuxFrameParser({ maxPayloadBytes: 16 });
    assert.throws(() => parser.push(header), /frame payload too large/i);
  });

  it("decodes ERROR payload", () => {
    const msg = "nope";
    const msgBytes = new TextEncoder().encode(msg);
    const payload = new Uint8Array(4 + msgBytes.byteLength);
    const dv = new DataView(payload.buffer);
    dv.setUint16(0, 2, false);
    dv.setUint16(2, msgBytes.byteLength, false);
    payload.set(msgBytes, 4);

    assert.deepEqual(decodeTcpMuxErrorPayload(payload), { code: 2, message: "nope" });
  });
});

describe("tcp-mux browser client integration", () => {
  it("OPEN + DATA roundtrips and CLOSE FIN reaches the TCP server", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedTargetHosts: ["8.8.8.8"],
        allowedTargetPorts: [echoPort],
        maxStreams: 16,
        createConnection: (() =>
          net.createConnection({
            host: "127.0.0.1",
            port: echoPort,
            allowHalfOpen: true,
          })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${proxyPort}`);
    const streamId = 1;

    const opened = new Promise<void>((resolve) => {
      client.onOpen = (id) => {
        if (id === streamId) resolve();
      };
    });

    let received = "";
    const gotHello = new Promise<void>((resolve, reject) => {
      client.onData = (id, data) => {
        if (id !== streamId) return;
        received += new TextDecoder().decode(data);
        if (received.includes("hello")) resolve();
      };
      client.onError = (id, err) => {
        if (id !== streamId) return;
        reject(new Error(`unexpected ERROR code=${err.code} message=${err.message}`));
      };
    });

    const closed = new Promise<void>((resolve) => {
      client.onClose = (id) => {
        if (id === streamId) resolve();
      };
    });

    const echoConnPromise = once(echoServer, "connection") as Promise<[net.Socket]>;

    client.open(streamId, "8.8.8.8", echoPort);
    client.send(streamId, new TextEncoder().encode("hello"));

    await withTimeout(opened, 2_000, "expected onOpen");
    await withTimeout(gotHello, 2_000, "expected DATA roundtrip");
    assert.ok(received.includes("hello"));

    const [echoSocket] = await withTimeout(echoConnPromise, 2_000, "expected echo TCP connection");
    const tcpEnd = once(echoSocket, "end");

    client.close(streamId, { fin: true });

    await withTimeout(tcpEnd, 2_000, "expected TCP FIN to reach echo server");
    await withTimeout(closed, 2_000, "expected onClose after FIN exchange");

    try {
      await client.shutdown();
    } finally {
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("bridges /tcp-mux streams into TcpProxyEventSink (WebSocketTcpProxyMuxClient)", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedTargetHosts: ["8.8.8.8"],
        allowedTargetPorts: [echoPort],
        maxStreams: 16,
        createConnection: (() =>
          net.createConnection({
            host: "127.0.0.1",
            port: echoPort,
            allowHalfOpen: true,
          })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const events: TcpProxyEvent[] = [];
    let connectedResolve: (() => void) | null = null;
    let closedResolve: (() => void) | null = null;
    let dataResolve: (() => void) | null = null;
    const streamId = 1;

    const connected = new Promise<void>((resolve) => (connectedResolve = resolve));
    const closed = new Promise<void>((resolve) => (closedResolve = resolve));
    const gotHello = new Promise<void>((resolve) => (dataResolve = resolve));

    const client = new WebSocketTcpProxyMuxClient(`http://127.0.0.1:${proxyPort}`, (evt) => {
      events.push(evt);
      if (evt.connectionId !== streamId) return;
      if (evt.type === "connected") connectedResolve?.();
      if (evt.type === "closed") closedResolve?.();
      if (evt.type === "data" && new TextDecoder().decode(evt.data).includes("hello")) dataResolve?.();
    });

    client.connect(streamId, "8.8.8.8", echoPort);
    client.send(streamId, new TextEncoder().encode("hello"));

    await withTimeout(connected, 2_000, "expected connected event");
    await withTimeout(gotHello, 2_000, "expected DATA event");

    client.close(streamId);
    await withTimeout(closed, 2_000, "expected closed event after FIN exchange");

    assert.ok(events.some((e) => e.type === "connected" && e.connectionId === streamId));
    assert.ok(events.some((e) => e.type === "data" && e.connectionId === streamId));
    assert.ok(events.some((e) => e.type === "closed" && e.connectionId === streamId));

    try {
      await client.shutdown();
    } finally {
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("responds to mux-level PING with PONG (same payload)", async () => {
    const server = http.createServer();

    const wss = new WebSocketServer({
      server,
      path: "/tcp-mux",
      // Mimic the gateway: require & select the canonical subprotocol.
      handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false),
    });

    let client: WebSocketTcpMuxProxyClient | null = null;
    try {
      const port = await listen(server, "127.0.0.1");

      client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${port}`);

      const serverWs = await withTimeout(
        new Promise<any>((resolve) => wss.once("connection", (ws) => resolve(ws))),
        2_000,
        "expected WebSocket connection",
      );

      const pingPayload = new Uint8Array([1, 2, 3, 4]);
      const parser = new TcpMuxFrameParser();

      const gotPong = new Promise<void>((resolve, reject) => {
        const onError = (err: unknown) => reject(err);
        const onMessage = (data: unknown) => {
          const chunk =
            typeof data === "string"
              ? Buffer.from(data, "utf8")
              : Array.isArray(data)
                ? Buffer.concat(data as Buffer[])
                : Buffer.from(data as ArrayBuffer);

          try {
            for (const frame of parser.push(chunk)) {
              if (frame.msgType !== TcpMuxMsgType.PONG) continue;
              assert.equal(frame.streamId, 0);
              assert.deepEqual(new Uint8Array(frame.payload), pingPayload);
              serverWs.off("message", onMessage);
              serverWs.off("error", onError);
              resolve();
              return;
            }
          } catch (err) {
            serverWs.off("message", onMessage);
            serverWs.off("error", onError);
            reject(err);
          }
        };

        serverWs.on("message", onMessage);
        serverWs.on("error", onError);
      });

      serverWs.send(Buffer.from(encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, pingPayload)));
      await withTimeout(gotPong, 2_000, "expected PONG response");
    } finally {
      await client?.shutdown().catch(() => {});
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await closeServer(server);
    }
  });

  it("appends ?token= when authToken option is provided (dev relay compatibility)", async () => {
    const server = http.createServer();

    const wss = new WebSocketServer({
      server,
      path: "/tcp-mux",
      handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false),
    });
    const seenUrlPromise = new Promise<string>((resolve) => {
      wss.once("connection", (_ws, req) => {
        const rawUrl = req.url;
        assert.equal(typeof rawUrl, "string");
        resolve(rawUrl);
      });
    });

    let client: WebSocketTcpMuxProxyClient | null = null;
    try {
      const port = await listen(server, "127.0.0.1");
      client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${port}`, { authToken: "dev-token" });

      const seenUrl = await withTimeout(seenUrlPromise, 2_000, "expected WebSocket connection");
      assert.ok(seenUrl.includes("token=dev-token"), `expected token in url, got ${seenUrl}`);
    } finally {
      await client?.shutdown().catch(() => {});
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await closeServer(server);
    }
  });

  it("drops queued DATA when a stream is locally RST before the queue flushes", async () => {
    const server = http.createServer();

    const wss = new WebSocketServer({
      server,
      path: "/tcp-mux",
      handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false),
    });

    let client: WebSocketTcpMuxProxyClient | null = null;
    try {
      const port = await listen(server, "127.0.0.1");
      client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${port}`);

      const serverWs = await withTimeout(
        new Promise<any>((resolve) => wss.once("connection", (ws) => resolve(ws))),
        2_000,
        "expected WebSocket connection",
      );

      const parser = new TcpMuxFrameParser();

      const rstObserved = new Promise<void>((resolve, reject) => {
        const onMessage = (data: unknown) => {
          const chunk =
            typeof data === "string"
              ? Buffer.from(data, "utf8")
              : Array.isArray(data)
                ? Buffer.concat(data as Buffer[])
                : Buffer.from(data as ArrayBuffer);

          for (const frame of parser.push(chunk)) {
            if (frame.msgType === TcpMuxMsgType.CLOSE && frame.streamId === 1) {
              serverWs.off("message", onMessage);
              resolve();
              return;
            }
            reject(new Error(`unexpected frame before RST: msgType=${frame.msgType} streamId=${frame.streamId}`));
            return;
          }
        };

        serverWs.on("message", onMessage);
        serverWs.once("error", reject);
      });

      // Queue OPEN + DATA then immediately RST the stream before the internal
      // microtask flush runs. The client should purge the queued DATA/OPEN and
      // only send CLOSE(RST).
      client.open(1, "example.com", 80);
      client.send(1, new TextEncoder().encode("hello"));
      client.close(1, { rst: true });

      await withTimeout(rstObserved, 2_000, "expected CLOSE frame after RST");
    } finally {
      await client?.shutdown().catch(() => {});
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await closeServer(server);
    }
  });

  it("surfaces stream-level ERROR without closing the mux session", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedTargetHosts: ["8.8.8.8"],
        allowedTargetPorts: [echoPort],
        maxStreams: 16,
        createConnection: (() =>
          net.createConnection({
            host: "127.0.0.1",
            port: echoPort,
            allowHalfOpen: true,
          })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${proxyPort}`);
    const blockedStream = 10;
    const okStream = 11;

    const events: string[] = [];

    const blockedError = new Promise<{ code: number; message: string }>((resolve) => {
      client.onError = (id, err) => {
        events.push(`error:${id}:${err.code}`);
        if (id === blockedStream) resolve(err);
      };
    });

    const blockedClosed = new Promise<void>((resolve) => {
      client.onClose = (id) => {
        events.push(`close:${id}`);
        if (id === blockedStream) resolve();
      };
    });

    const opened = new Promise<void>((resolve) => {
      client.onOpen = (id) => {
        events.push(`open:${id}`);
        // Resolve once both opens have been synchronously delivered.
        if (events.includes(`open:${blockedStream}`) && events.includes(`open:${okStream}`)) resolve();
      };
    });

    let okData = "";
    const okRoundtrip = new Promise<void>((resolve, reject) => {
      client.onData = (id, data) => {
        if (id !== okStream) return;
        okData += new TextDecoder().decode(data);
        if (okData.includes("ok\n")) resolve();
      };
      // `client.onError` is already set above; we rely on the events list.
      // If okStream errors, the assertions below will fail.
      void reject;
    });

    client.open(blockedStream, "8.8.8.8", echoPort + 1);
    client.open(okStream, "8.8.8.8", echoPort);
    client.send(okStream, new TextEncoder().encode("ok\n"));

    await withTimeout(opened, 2_000, "expected synchronous onOpen callbacks");

    const err = await withTimeout(blockedError, 2_000, "expected stream-level ERROR for blocked target");
    assert.equal(err.code, 1);
    await withTimeout(blockedClosed, 2_000, "expected blocked stream to close after ERROR");

    await withTimeout(okRoundtrip, 2_000, "expected ok stream to remain usable after ERROR");
    assert.ok(okData.includes("ok\n"));

    // Ensure `onOpen` was invoked before `onError` for the blocked stream.
    assert.ok(events.indexOf(`open:${blockedStream}`) !== -1);
    assert.ok(events.indexOf(`error:${blockedStream}:1`) !== -1);
    assert.ok(events.indexOf(`open:${blockedStream}`) < events.indexOf(`error:${blockedStream}:1`));

    try {
      client.close(okStream, { fin: true });
      await client.shutdown();
    } finally {
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("allows sending DATA after receiving CLOSE(FIN) from the server (half-close)", async () => {
    const halfCloseServer = net.createServer({ allowHalfOpen: true }, (socket) => {
      socket.end("bye\n");
    });
    const halfClosePort = await listen(halfCloseServer, "127.0.0.1");

    let receivedAfterFin = "";
    const serverSawAfter = new Promise<void>((resolve, reject) => {
      halfCloseServer.once("connection", (socket) => {
        socket.on("data", (data) => {
          receivedAfterFin += data.toString("utf8");
          if (receivedAfterFin.includes("after\n")) resolve();
        });
        socket.on("error", reject);
      });
    });

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedTargetHosts: ["8.8.8.8"],
        allowedTargetPorts: [halfClosePort],
        maxStreams: 16,
        createConnection: (() =>
          net.createConnection({
            host: "127.0.0.1",
            port: halfClosePort,
            allowHalfOpen: true,
          })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${proxyPort}`);
    const streamId = 1;

    let recv = "";
    const gotBye = new Promise<void>((resolve) => {
      client.onData = (id, data) => {
        if (id !== streamId) return;
        recv += new TextDecoder().decode(data);
        if (recv.includes("bye\n")) resolve();
      };
    });

    const gotRemoteFin = new Promise<void>((resolve, reject) => {
      client.onClose = (id) => {
        if (id === streamId) resolve();
      };
      client.onError = (id, err) => {
        if (id === streamId) reject(new Error(`unexpected ERROR code=${err.code} message=${err.message}`));
      };
    });

    client.open(streamId, "8.8.8.8", halfClosePort);

    await withTimeout(gotBye, 2_000, "expected DATA before FIN");
    await withTimeout(gotRemoteFin, 2_000, "expected remote CLOSE(FIN)");

    client.send(streamId, new TextEncoder().encode("after\n"));
    await withTimeout(serverSawAfter, 2_000, "expected server to receive DATA after FIN");

    try {
      client.close(streamId, { fin: true });
      await client.shutdown();
    } finally {
      await closeServer(proxyServer);
      await closeServer(halfCloseServer);
    }
  });

  it("rejects reusing a stream_id within a session", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, {
        allowedTargetHosts: ["8.8.8.8"],
        allowedTargetPorts: [echoPort],
        maxStreams: 16,
        createConnection: (() =>
          net.createConnection({
            host: "127.0.0.1",
            port: echoPort,
            allowHalfOpen: true,
          })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const client = new WebSocketTcpMuxProxyClient(`http://127.0.0.1:${proxyPort}`);
    const streamId = 1;

    const closed = new Promise<void>((resolve) => {
      client.onClose = (id) => {
        if (id === streamId) resolve();
      };
    });

    client.open(streamId, "8.8.8.8", echoPort);
    client.send(streamId, new TextEncoder().encode("hello"));
    client.close(streamId, { fin: true });

    await withTimeout(closed, 2_000, "expected stream to close before reuse");

    const reuseError = new Promise<{ code: number; message: string }>((resolve) => {
      client.onError = (id, err) => {
        if (id === streamId) resolve(err);
      };
    });

    client.open(streamId, "8.8.8.8", echoPort);

    const err = await withTimeout(reuseError, 2_000, "expected error when reusing stream_id");
    assert.equal(err.code, 3);
    assert.match(err.message, /already used/i);

    try {
      await client.shutdown();
    } finally {
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("OPEN encoding matches the gateway's expectations", () => {
    // Lightweight invariant test: ensure OPEN payload encoder can be decoded by
    // the gateway-side codec. This catches accidental endianness regressions.
    const payload = encodeTcpMuxOpenPayload({ host: "example.com", port: 443, metadata: "{\"a\":1}" });
    assert.deepEqual(decodeGatewayTcpMuxOpenPayload(Buffer.from(payload)), {
      host: "example.com",
      port: 443,
      metadata: "{\"a\":1}",
    });
  });
});
