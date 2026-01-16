import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { once } from "node:events";
import { PassThrough } from "node:stream";
import { describe, it } from "node:test";

import { handleTcpMuxUpgrade } from "../src/routes/tcpMux.js";
import {
  decodeTcpMuxErrorPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  TCP_MUX_HEADER_BYTES,
  TCP_MUX_SUBPROTOCOL,
} from "../src/protocol/tcpMux.js";
import { SessionConnectionTracker } from "../src/session.js";
import { TEST_WS_HANDSHAKE_HEADERS } from "./testConfig.js";

async function listen(server: http.Server | net.Server, host?: string): Promise<number> {
  server.listen(0, host);
  await once(server, "listening");
  const addr = server.address();
  if (addr && typeof addr === "object") return addr.port;
  throw new Error("Expected server to bind to an ephemeral port");
}

async function closeServer(server: http.Server | net.Server): Promise<void> {
  try {
    // Ensure tests don't hang on leaked upgrade sockets.
    (server as unknown as { closeAllConnections?: () => void }).closeAllConnections?.();
    (server as unknown as { closeIdleConnections?: () => void }).closeIdleConnections?.();
    server.close();
  } catch (err) {
    const code = (err as { code?: unknown } | null)?.code;
    if (code === "ERR_SERVER_NOT_RUNNING") return;
    throw err;
  }
  await once(server, "close");
}

async function captureUpgradeResponse(run: (socket: PassThrough) => void): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");
  run(socket);
  await ended;
  return Buffer.concat(chunks).toString("utf8");
}

function openWebSocket(url: string, protocol: string): Promise<WebSocket> {
  const ws = new WebSocket(url, protocol);
  ws.binaryType = "arraybuffer";
  return new Promise((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve(ws);
    };
    const onError = () => {
      cleanup();
      reject(new Error("WebSocket connection failed"));
    };
    const cleanup = () => {
      ws.removeEventListener("open", onOpen);
      ws.removeEventListener("error", onError);
    };
    ws.addEventListener("open", onOpen);
    ws.addEventListener("error", onError);
  });
}

async function closeWebSocket(ws: WebSocket): Promise<void> {
  if (ws.readyState === WebSocket.CLOSED) return;
  ws.close();
  await new Promise<void>((resolve) => ws.addEventListener("close", () => resolve(), { once: true }));
}

describe("tcpMux route", () => {
  it("rejects overly long request URLs (414)", async () => {
    const req = {
      url: `/tcp-mux?${"a".repeat(9000)}`,
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpMuxUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 414 "));
  });

  it("rejects non-WebSocket requests early (400)", async () => {
    const req = {
      url: "/tcp-mux",
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpMuxUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes("Invalid WebSocket upgrade"));
  });

  it("rejects oversized Sec-WebSocket-Protocol headers (400)", async () => {
    const req = {
      url: "/tcp-mux",
      headers: {
        ...TEST_WS_HANDSHAKE_HEADERS,
        "sec-websocket-protocol": `${TCP_MUX_SUBPROTOCOL}, ${"a".repeat(5_000)}`,
      },
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpMuxUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes("Invalid Sec-WebSocket-Protocol header"));
  });

  it("rejects missing Sec-WebSocket-Protocol (400)", async () => {
    const req = {
      url: "/tcp-mux",
      headers: {
        ...TEST_WS_HANDSHAKE_HEADERS,
      },
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpMuxUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes(`Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`));
  });

  it("closes the WebSocket with 1002 when a frame exceeds maxFramePayloadBytes", async () => {
    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpMuxUpgrade(req, socket, head, { maxFramePayloadBytes: 16 });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    let ws: WebSocket | null = null;
    try {
      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);

      const header = Buffer.alloc(TCP_MUX_HEADER_BYTES);
      header.writeUInt8(TcpMuxMsgType.DATA, 0);
      header.writeUInt32BE(1, 1);
      header.writeUInt32BE(1024, 5);

      const closePromise = new Promise<number>((resolve, reject) => {
        const id = setTimeout(() => reject(new Error("expected close")), 2_000);
        // Avoid keeping the process open just for the timer in case the close
        // event arrives quickly.
        (id as unknown as { unref?: () => void }).unref?.();
        ws.addEventListener(
          "close",
          (event) => {
            clearTimeout(id);
            resolve(event.code);
          },
          { once: true },
        );
      });

      ws.send(header);

      const code = await closePromise;
      assert.equal(code, 1002);
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
    }
  });

  it("multiplexes multiple concurrent streams to an echo server", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpMuxUpgrade(req, socket, head, {
          allowedTargetHosts: ["8.8.8.8"],
          allowedTargetPorts: [echoPort],
          maxStreams: 16,
          createConnection: (() =>
            net.createConnection({ host: "127.0.0.1", port: echoPort, allowHalfOpen: true })) as typeof net.createConnection,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
      assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

      const parser = new TcpMuxFrameParser();
      const receivedLines = new Map<number, string[]>();
      const partialByStream = new Map<number, string>();
      let unexpectedError: Error | undefined;

      function pushLine(streamId: number, line: string): void {
        const lines = receivedLines.get(streamId) ?? [];
        lines.push(line);
        receivedLines.set(streamId, lines);
      }

      ws.addEventListener("message", (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const chunk = Buffer.from(event.data);
        for (const frame of parser.push(chunk)) {
          if (frame.msgType === TcpMuxMsgType.DATA) {
            const previousPartial = partialByStream.get(frame.streamId) ?? "";
            const text = previousPartial + frame.payload.toString("utf8");
            const parts = text.split("\n");
            for (let i = 0; i < parts.length - 1; i++) pushLine(frame.streamId, parts[i]!);
            partialByStream.set(frame.streamId, parts.at(-1) ?? "");
          } else if (frame.msgType === TcpMuxMsgType.ERROR) {
            const { code, message } = decodeTcpMuxErrorPayload(frame.payload);
            unexpectedError = new Error(`unexpected ERROR stream=${frame.streamId} code=${code} message=${message}`);
          }
        }
      });

      const s1 = 1;
      const s2 = 2;

      // Send OPEN frames for both streams (bundled into one WebSocket message).
      ws.send(
        Buffer.concat([
          encodeTcpMuxFrame(TcpMuxMsgType.OPEN, s1, encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort })),
          encodeTcpMuxFrame(TcpMuxMsgType.OPEN, s2, encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort })),
        ]),
      );

      // Interleave DATA frames across streams (and bundle some frames).
      ws.send(
        Buffer.concat([
          encodeTcpMuxFrame(TcpMuxMsgType.DATA, s1, Buffer.from("s1-a\n", "utf8")),
          encodeTcpMuxFrame(TcpMuxMsgType.DATA, s2, Buffer.from("s2-a\n", "utf8")),
          encodeTcpMuxFrame(TcpMuxMsgType.DATA, s1, Buffer.from("s1-b\n", "utf8")),
          encodeTcpMuxFrame(TcpMuxMsgType.DATA, s2, Buffer.from("s2-b\n", "utf8")),
        ]),
      );

      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline) {
        if (unexpectedError) break;
        const a = receivedLines.get(s1) ?? [];
        const b = receivedLines.get(s2) ?? [];
        if (a.includes("s1-a") && a.includes("s1-b") && b.includes("s2-a") && b.includes("s2-b")) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      if (unexpectedError) throw unexpectedError;
      assert.deepEqual(receivedLines.get(s1)?.sort(), ["s1-a", "s1-b"]);
      assert.deepEqual(receivedLines.get(s2)?.sort(), ["s2-a", "s2-b"]);

      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, s1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, s2, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("returns ERROR for a blocked target without closing the mux connection", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpMuxUpgrade(req, socket, head, {
          allowedTargetHosts: ["8.8.8.8"],
          allowedTargetPorts: [echoPort],
          maxStreams: 16,
          createConnection: (() =>
            net.createConnection({ host: "127.0.0.1", port: echoPort, allowHalfOpen: true })) as typeof net.createConnection,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
      assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

      const parser = new TcpMuxFrameParser();
      const errors: Array<{ streamId: number; code: number; message: string }> = [];
      const received = new Map<number, Buffer[]>();

      ws.addEventListener("message", (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const chunk = Buffer.from(event.data);
        for (const frame of parser.push(chunk)) {
          if (frame.msgType === TcpMuxMsgType.ERROR) {
            const { code, message } = decodeTcpMuxErrorPayload(frame.payload);
            errors.push({ streamId: frame.streamId, code, message });
          } else if (frame.msgType === TcpMuxMsgType.DATA) {
            const list = received.get(frame.streamId) ?? [];
            list.push(frame.payload);
            received.set(frame.streamId, list);
          }
        }
      });

      const blockedStream = 10;
      ws.send(
        encodeTcpMuxFrame(
          TcpMuxMsgType.OPEN,
          blockedStream,
          encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort + 1 }),
        ),
      );

      const okStream = 11;
      ws.send(
        encodeTcpMuxFrame(TcpMuxMsgType.OPEN, okStream, encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort })),
      );
      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, okStream, Buffer.from("ok\n", "utf8")));

      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline) {
        if (errors.some((e) => e.streamId === blockedStream)) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      const err = errors.find((e) => e.streamId === blockedStream);
      assert.ok(err, "expected ERROR for blocked stream");
      assert.equal(err.code, TcpMuxErrorCode.POLICY_DENIED);
      assert.equal(ws.readyState, WebSocket.OPEN);

      const deadline2 = Date.now() + 2_000;
      while (Date.now() < deadline2) {
        const chunks = received.get(okStream) ?? [];
        if (chunks.some((c) => c.toString("utf8").includes("ok"))) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      const okData = Buffer.concat(received.get(okStream) ?? []).toString("utf8");
      assert.ok(okData.includes("ok\n"));

      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, okStream, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("allows dialing loopback targets when allowPrivateIps is enabled", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpMuxUpgrade(req, socket, head, {
          allowPrivateIps: true,
          allowedTargetHosts: ["127.0.0.1"],
          allowedTargetPorts: [echoPort],
          maxStreams: 4,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
      assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

      const parser = new TcpMuxFrameParser();
      const received: Buffer[] = [];
      const errors: Array<{ streamId: number; code: number; message: string }> = [];

      ws.addEventListener("message", (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const chunk = Buffer.from(event.data);
        for (const frame of parser.push(chunk)) {
          if (frame.msgType === TcpMuxMsgType.DATA) {
            received.push(frame.payload);
          } else if (frame.msgType === TcpMuxMsgType.ERROR) {
            const { code, message } = decodeTcpMuxErrorPayload(frame.payload);
            errors.push({ streamId: frame.streamId, code, message });
          }
        }
      });

      const streamId = 1;
      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, streamId, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort })));
      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, streamId, Buffer.from("ping", "utf8")));

      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline) {
        if (errors.length > 0) break;
        if (Buffer.concat(received).toString("utf8").includes("ping")) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      assert.deepEqual(errors, []);
      assert.ok(Buffer.concat(received).toString("utf8").includes("ping"));

      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, streamId, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN)));
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("returns ERROR when dialing loopback targets when allowPrivateIps is disabled", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpMuxUpgrade(req, socket, head, {
          allowPrivateIps: false,
          allowedTargetHosts: ["127.0.0.1"],
          allowedTargetPorts: [echoPort],
          maxStreams: 4,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
      assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

      const parser = new TcpMuxFrameParser();
      const errors: Array<{ streamId: number; code: number; message: string }> = [];

      ws.addEventListener("message", (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const chunk = Buffer.from(event.data);
        for (const frame of parser.push(chunk)) {
          if (frame.msgType === TcpMuxMsgType.ERROR) {
            const { code, message } = decodeTcpMuxErrorPayload(frame.payload);
            errors.push({ streamId: frame.streamId, code, message });
          }
        }
      });

      const streamId = 1;
      ws.send(
        encodeTcpMuxFrame(TcpMuxMsgType.OPEN, streamId, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoPort })),
      );

      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline) {
        if (errors.some((e) => e.streamId === streamId)) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      const err = errors.find((e) => e.streamId === streamId);
      assert.ok(err, "expected ERROR for loopback stream");
      assert.equal(err.code, TcpMuxErrorCode.POLICY_DENIED);
      assert.equal(ws.readyState, WebSocket.OPEN);
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("enforces per-session maxConnections across multiplexed streams", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const sessionConnections = new SessionConnectionTracker(1);

    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpMuxUpgrade(req, socket, head, {
          allowedTargetHosts: ["8.8.8.8"],
          allowedTargetPorts: [echoPort],
          maxStreams: 16,
          sessionId: "test-session",
          sessionConnections,
          createConnection: (() =>
            net.createConnection({ host: "127.0.0.1", port: echoPort, allowHalfOpen: true })) as typeof net.createConnection,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
      assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);

      const parser = new TcpMuxFrameParser();
      const errors: Array<{ streamId: number; code: number; message: string }> = [];
      const received = new Map<number, Buffer[]>();

      ws.addEventListener("message", (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const chunk = Buffer.from(event.data);
        for (const frame of parser.push(chunk)) {
          if (frame.msgType === TcpMuxMsgType.ERROR) {
            const { code, message } = decodeTcpMuxErrorPayload(frame.payload);
            errors.push({ streamId: frame.streamId, code, message });
          } else if (frame.msgType === TcpMuxMsgType.DATA) {
            const list = received.get(frame.streamId) ?? [];
            list.push(frame.payload);
            received.set(frame.streamId, list);
          }
        }
      });

      const s1 = 1;
      const s2 = 2;
      ws.send(
        Buffer.concat([
          encodeTcpMuxFrame(TcpMuxMsgType.OPEN, s1, encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort })),
          encodeTcpMuxFrame(TcpMuxMsgType.OPEN, s2, encodeTcpMuxOpenPayload({ host: "8.8.8.8", port: echoPort })),
        ]),
      );

      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, s1, Buffer.from("ok\n", "utf8")));

      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline) {
        const err = errors.find((e) => e.streamId === s2);
        const okData = Buffer.concat(received.get(s1) ?? []).toString("utf8");
        if (err && okData.includes("ok\n")) break;
        await new Promise((r) => setTimeout(r, 10));
      }

      const err = errors.find((e) => e.streamId === s2);
      assert.ok(err, "expected ERROR for second stream");
      assert.equal(err.code, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED);

      const okData = Buffer.concat(received.get(s1) ?? []).toString("utf8");
      assert.ok(okData.includes("ok\n"));

      ws.send(encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, s1, encodeTcpMuxClosePayload(TcpMuxCloseFlags.RST)));
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });
});
