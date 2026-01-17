import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";

import { startProxyServer } from "../prototype/nt-arch-rfc/proxy-server.js";
import { runNetworkingProbe } from "../prototype/nt-arch-rfc/client.js";

async function startTcpEchoServer() {
  const server = net.createServer((socket) => {
    socket.on("data", (data) => {
      try {
        socket.write(data);
      } catch {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return {
    port: address.port,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

test("Networking architecture RFC prototype: ARP + DNS + TCP echo over L2 tunnel", async () => {
  const echo = await startTcpEchoServer();
  const proxy = await startProxyServer({
    tcpForward: {
      [echo.port]: { host: "127.0.0.1", port: echo.port },
    },
    dnsA: { "echo.local": "203.0.113.10" },
  });

  try {
    const result = await runNetworkingProbe({
      url: proxy.url,
      dnsName: "echo.local",
      echoPort: echo.port,
      throughputBytes: 128 * 1024,
    });

    assert.equal(result.dns.name, "echo.local");
    assert.equal(result.dns.ip, "203.0.113.10");
    assert.ok(result.dns.rttMs > 0);

    assert.equal(result.tcp.remotePort, echo.port);
    assert.ok(result.tcp.connectRttMs > 0);
    assert.ok(result.tcp.throughputMbps > 0);
    assert.equal(result.tcp.ok, true);
  } finally {
    await proxy.close();
    await echo.close();
  }
});
