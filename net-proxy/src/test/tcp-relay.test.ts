import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";

test("tcp relay echoes bytes roundtrip", async () => {
  const originalOpen = process.env.AERO_PROXY_OPEN;
  process.env.AERO_PROXY_OPEN = "1";

  const echoServer = net.createServer((socket) => {
    socket.on("error", () => {
      // Ignore socket errors for test shutdown.
    });
    socket.pipe(socket);
  });

  await new Promise<void>((resolve) => echoServer.listen(0, "127.0.0.1", resolve));
  const echoAddr = echoServer.address();
  assert.ok(echoAddr && typeof echoAddr !== "string");

  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0 });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  const ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?host=127.0.0.1&port=${echoAddr.port}`);

  await new Promise<void>((resolve, reject) => {
    ws.once("open", () => resolve());
    ws.once("error", reject);
  });

  const payload = Buffer.from([0, 1, 2, 3, 4, 5, 255]);
  const receivedPromise = new Promise<Buffer>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for echo")), 2_000);
    timeout.unref();
    ws.once("message", (data, isBinary) => {
      clearTimeout(timeout);
      assert.equal(isBinary, true);
      resolve(Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer));
    });
    ws.once("error", reject);
  });

  ws.send(payload);

  const received = await receivedPromise;
  assert.deepEqual(received, payload);

  await new Promise<void>((resolve) => {
    ws.once("close", () => resolve());
    ws.close(1000, "done");
  });

  await proxy.close();
  await new Promise<void>((resolve, reject) => echoServer.close((err) => (err ? reject(err) : resolve())));

  if (originalOpen === undefined) {
    delete process.env.AERO_PROXY_OPEN;
  } else {
    process.env.AERO_PROXY_OPEN = originalOpen;
  }
});

