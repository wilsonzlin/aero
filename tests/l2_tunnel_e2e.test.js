import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";

import { runL2TunnelProbe } from "./helpers/l2_probe.js";
import { startRustL2Proxy } from "../tools/rust_l2_proxy.js";
import { L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD } from "../web/src/shared/l2TunnelProtocol.ts";

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
  await new Promise((resolve, reject) => {
    const onError = (err) => {
      cleanup();
      reject(err);
    };
    const cleanup = () => {
      server.off("error", onError);
    };
    server.once("error", onError);
    server.listen(0, "127.0.0.1", () => {
      cleanup();
      resolve();
    });
  });
  const address = server.address();
  return {
    port: address.port,
    close: () =>
      new Promise((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      }),
  };
}

async function startAeroL2Proxy({ echoPort }) {
  const testNetIp = "203.0.113.10";

  const proxy = await startRustL2Proxy({
    // The `ws` client used by the probe does not send an Origin header.
    // Enable `open` mode so the production proxy accepts this local test connection.
    AERO_L2_OPEN: "1",
    // Make the test hermetic even if the parent environment has auth enabled.
    AERO_L2_AUTH_MODE: "none",
    // Test-only overrides: map a guest-visible TEST-NET address + DNS entry to localhost.
    AERO_L2_DNS_A: `echo.local=${testNetIp}`,
    AERO_L2_TCP_FORWARD: `${testNetIp}:${echoPort}=127.0.0.1:${echoPort}`,
  });

  return {
    url: `ws://127.0.0.1:${proxy.port}/l2`,
    close: proxy.close,
  };
}

test(
  "Production L2 tunnel proxy: DHCP + ARP + DNS + TCP echo over aero-l2-tunnel-v1",
  // Allow enough time for a cold `cargo build` on CI.
  { timeout: 900_000 },
  async () => {
    const echo = await startTcpEchoServer();
    try {
      const proxy = await startAeroL2Proxy({ echoPort: echo.port });

      try {
        const result = await runL2TunnelProbe({
          url: proxy.url,
          dnsName: "echo.local",
          echoPort: echo.port,
          // Keep payload small enough to fit within the default L2 tunnel max frame payload size.
          // `aero-l2-proxy` reads from the forwarded TCP socket in 16KiB chunks; if we ask for a
          // large echo, the stack may attempt to emit a single oversized Ethernet frame which the
          // tunnel encoder will drop.
          throughputBytes: Math.min(1800, Math.max(1, L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD - 200)),
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
      }
    } finally {
      await echo.close();
    }
  },
);
