import test from "node:test";
import assert from "node:assert/strict";
import dgram from "node:dgram";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";
import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "../udpRelayProtocol";

async function startUdpEchoServer(type: "udp4" | "udp6", host: string): Promise<{ port: number; close: () => Promise<void> }> {
  const server = dgram.createSocket(type);

  server.on("message", (msg, rinfo) => {
    server.send(msg, rinfo.port, rinfo.address);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.bind(0, host, () => resolve());
  });
  const addr = server.address();
  assert.ok(typeof addr !== "string");

  return {
    port: addr.port,
    close: async () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      })
  };
}

async function startUdpReplyFromDifferentPortServer(
  host: string
): Promise<{ port: number; senderPort: number; received: Promise<void>; close: () => Promise<void> }> {
  const server = dgram.createSocket("udp4");
  let sender: dgram.Socket | null = null;
  let receivedResolve: (() => void) | null = null;
  const received = new Promise<void>((resolve) => {
    receivedResolve = resolve;
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.bind(0, host, () => resolve());
  });
  const addr = server.address();
  assert.ok(typeof addr !== "string");

  for (let attempt = 0; attempt < 5; attempt++) {
    const s = dgram.createSocket("udp4");
    try {
      await new Promise<void>((resolve, reject) => {
        s.once("error", reject);
        s.bind(0, host, () => resolve());
      });
    } catch (err) {
      try {
        s.close();
      } catch {
        // ignore
      }
      throw err;
    }

    const sAddr = s.address();
    assert.ok(typeof sAddr !== "string");
    if (sAddr.port !== addr.port) {
      sender = s;
      break;
    }
    s.close();
  }

  if (!sender) {
    server.close();
    throw new Error("failed to allocate distinct UDP sender port");
  }

  const senderAddr = sender.address();
  assert.ok(typeof senderAddr !== "string");

  server.on("message", (msg, rinfo) => {
    receivedResolve?.();
    sender?.send(msg, rinfo.port, rinfo.address);
  });

  return {
    port: addr.port,
    senderPort: senderAddr.port,
    received,
    close: async () =>
      new Promise<void>((resolve) => {
        let pending = 2;
        const done = () => {
          pending--;
          if (pending === 0) resolve();
        };
        try {
          server.close(() => done());
        } catch {
          done();
        }
        try {
          sender?.close(() => done());
        } catch {
          done();
        }
      })
  };
}

async function openWebSocket(url: string): Promise<WebSocket> {
  const ws = new WebSocket(url);
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

async function waitForBinaryMessage(ws: WebSocket, timeoutMs = 2_000): Promise<Buffer> {
  return new Promise<Buffer>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for message")), timeoutMs);
    timeout.unref();
    ws.once("message", (data, isBinary) => {
      clearTimeout(timeout);
      assert.equal(isBinary, true);
      resolve(Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer));
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

async function assertNoMessage(ws: WebSocket, timeoutMs = 250): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const onMessage = () => {
      cleanup();
      reject(new Error("unexpected websocket message"));
    };
    const onError = (err: unknown) => {
      cleanup();
      reject(err);
    };

    const cleanup = () => {
      clearTimeout(timeout);
      ws.off("message", onMessage);
      ws.off("error", onError);
    };

    const timeout = setTimeout(() => {
      cleanup();
      resolve();
    }, timeoutMs);
    timeout.unref();

    ws.on("message", onMessage);
    ws.on("error", onError);
  });
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

test("udp multiplexed relay: echoes framed datagrams (v1 IPv4)", async () => {
  const udpServer = await startUdpEchoServer("udp4", "127.0.0.1");
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    const payload = Buffer.from([9, 8, 7, 6]);
    const guestPort = 54321;
    const frame = encodeUdpRelayV1Datagram({
      guestPort,
      remoteIpv4: [127, 0, 0, 1],
      remotePort: udpServer.port,
      payload
    });

    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(frame);

    const received = await receivedPromise;
    const decoded = decodeUdpRelayFrame(received);
    assert.equal(decoded.version, 1);
    assert.equal(decoded.guestPort, guestPort);
    assert.deepEqual(decoded.remoteIpv4, [127, 0, 0, 1]);
    assert.equal(decoded.remotePort, udpServer.port);
    assert.deepEqual(decoded.payload, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
    await udpServer.close();
  }
});

test("udp multiplexed relay: can emit v2 frames for IPv4 once the client sends v2", async () => {
  const udpServer = await startUdpEchoServer("udp4", "127.0.0.1");
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    udpRelayPreferV2: true
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    const payload = Buffer.from([9, 9, 9]);
    const guestPort = 23456;
    const remoteIp = Buffer.from([127, 0, 0, 1]);
    const frame = encodeUdpRelayV2Datagram({
      guestPort,
      remoteIp,
      remotePort: udpServer.port,
      payload
    });

    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(frame);

    const received = await receivedPromise;
    const decoded = decodeUdpRelayFrame(received);
    assert.equal(decoded.version, 2);
    assert.equal(decoded.addressFamily, 4);
    assert.equal(decoded.guestPort, guestPort);
    assert.deepEqual(Buffer.from(decoded.remoteIp), remoteIp);
    assert.equal(decoded.remotePort, udpServer.port);
    assert.deepEqual(decoded.payload, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
    await udpServer.close();
  }
});

test("udp multiplexed relay: drops packets from unexpected remote ports", async () => {
  const server = await startUdpReplyFromDifferentPortServer("127.0.0.1");
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    const payload = Buffer.from([0xaa, 0xbb, 0xcc]);
    const guestPort = 12345;
    const frame = encodeUdpRelayV1Datagram({
      guestPort,
      remoteIpv4: [127, 0, 0, 1],
      remotePort: server.port,
      payload
    });

    const noMessagePromise = assertNoMessage(ws, 400);
    ws.send(frame);
    await Promise.race([
      server.received,
      new Promise<void>((_resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for UDP server")), 1_000);
        timeout.unref();
      })
    ]);
    await noMessagePromise;

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
    await server.close();
  }
});

test("udp multiplexed relay: inbound filter mode any accepts packets from unexpected remote ports", async () => {
  const server = await startUdpReplyFromDifferentPortServer("127.0.0.1");
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    udpRelayInboundFilterMode: "any"
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    const payload = Buffer.from([0x11, 0x22, 0x33]);
    const guestPort = 12345;
    const frame = encodeUdpRelayV1Datagram({
      guestPort,
      remoteIpv4: [127, 0, 0, 1],
      remotePort: server.port,
      payload
    });

    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(frame);

    await server.received;
    const received = await receivedPromise;
    const decoded = decodeUdpRelayFrame(received);
    assert.equal(decoded.version, 1);
    assert.equal(decoded.guestPort, guestPort);
    assert.deepEqual(decoded.remoteIpv4, [127, 0, 0, 1]);
    assert.equal(decoded.remotePort, server.senderPort);
    assert.deepEqual(decoded.payload, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
    await server.close();
  }
});

test("udp multiplexed relay: supports IPv6 via v2 framing", async (t) => {
  let udpServer: { port: number; close: () => Promise<void> };
  try {
    udpServer = await startUdpEchoServer("udp6", "::1");
  } catch (err) {
    t.skip(`udp6 echo server failed to start: ${String(err)}`);
    return;
  }

  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    const payload = Buffer.from([1, 2, 3, 4]);
    const guestPort = 12345;
    const remoteIp = Buffer.alloc(16);
    remoteIp[15] = 1; // ::1

    const frame = encodeUdpRelayV2Datagram({
      guestPort,
      remoteIp,
      remotePort: udpServer.port,
      payload
    });

    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(frame);

    const received = await receivedPromise;
    const decoded = decodeUdpRelayFrame(received);
    assert.equal(decoded.version, 2);
    assert.equal(decoded.addressFamily, 6);
    assert.equal(decoded.guestPort, guestPort);
    assert.deepEqual(decoded.remoteIp, remoteIp);
    assert.equal(decoded.remotePort, udpServer.port);
    assert.deepEqual(decoded.payload, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
    await udpServer.close();
  }
});

test("udp multiplexed relay: closes with 1011 if UDP socket creation throws", async (t) => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  let ws: WebSocket | null = null;

  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp`);

    t.mock.method(dgram, "createSocket", () => {
      throw new Error("boom");
    });

    const frame = encodeUdpRelayV1Datagram({
      guestPort: 12345,
      remoteIpv4: [127, 0, 0, 1],
      remotePort: 9,
      payload: Buffer.from([1])
    });

    try {
      ws.send(frame);
    } catch {
      // ignore close races
    }

    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1011);
    ws = null;
  } finally {
    ws?.terminate();
    await proxy.close();
  }
});
