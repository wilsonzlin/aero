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
