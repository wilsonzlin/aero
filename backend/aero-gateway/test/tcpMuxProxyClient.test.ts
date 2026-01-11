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
} from "../../../web/src/net/tcpMuxProxy.js";

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
  const timeout = new Promise<never>((_, reject) => {
    const id = setTimeout(() => reject(new Error(message)), timeoutMs);
    // Avoid keeping the process open just for the timer in case the promise
    // settles quickly.
    (id as unknown as { unref?: () => void }).unref?.();
  });
  return Promise.race([promise, timeout]);
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
