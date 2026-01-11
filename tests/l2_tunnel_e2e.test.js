import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { spawn } from "node:child_process";
import readline from "node:readline";
import { once } from "node:events";

import { runL2TunnelProbe } from "./helpers/l2_probe.js";

async function startTcpEchoServer() {
  const server = net.createServer((socket) => {
    socket.on("data", (data) => socket.write(data));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return {
    port: address.port,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

async function startAeroL2Proxy({ echoPort }) {
  const testNetIp = "203.0.113.10";

  const env = {
    ...process.env,
    CARGO_TERM_COLOR: "never",
    RUST_LOG: "warn",
    // The `ws` client used by the probe does not send an Origin header.
    // Enable `open` mode so the production proxy accepts this local test connection.
    AERO_L2_OPEN: "1",
    // Make the test hermetic even if the parent environment has auth enabled.
    AERO_L2_AUTH_MODE: "none",
    // Test-only overrides: map a guest-visible TEST-NET address + DNS entry to localhost.
    AERO_L2_DNS_A: `echo.local=${testNetIp}`,
    AERO_L2_TCP_FORWARD: `${testNetIp}:${echoPort}=127.0.0.1:${echoPort}`,
  };
  // Some CI environments set `RUSTC_WRAPPER` to `sccache` by default; don't inherit it because
  // it isn't guaranteed to exist in our hermetic test runner.
  delete env.RUSTC_WRAPPER;

  const child = spawn(
    "cargo",
    ["run", "-q", "-p", "aero-l2-proxy", "--", "--bind", "127.0.0.1:0", "--ready-stdout"],
    {
      stdio: ["ignore", "pipe", "pipe"],
      env,
    },
  );

  const stderr = [];
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr.push(chunk);
    if (stderr.length > 50) stderr.shift();
  });

  const rl = readline.createInterface({ input: child.stdout });
  const url = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      child.kill("SIGTERM");
      setTimeout(() => child.kill("SIGKILL"), 2000).unref();
      reject(new Error("Timed out waiting for aero-l2-proxy readiness"));
    }, 180_000);

    function cleanup() {
      clearTimeout(timeout);
      rl.close();
      child.off("exit", onExit);
      child.off("error", onError);
    }

    function onExit(code, signal) {
      cleanup();
      reject(
        new Error(
          `aero-l2-proxy exited before ready (code=${code}, signal=${signal})\n` +
            stderr.join(""),
        ),
      );
    }

    function onError(err) {
      cleanup();
      reject(err);
    }

    child.once("exit", onExit);
    child.once("error", onError);

    rl.on("line", (line) => {
      const m = line.match(/^AERO_L2_PROXY_READY\s+(\S+)\s*$/);
      if (!m) return;
      cleanup();
      resolve(m[1]);
    });
  });

  return {
    url,
    close: async () => {
      child.kill("SIGTERM");
      const exit = await Promise.race([
        once(child, "exit"),
        new Promise((resolve) => setTimeout(resolve, 2000, null)),
      ]);
      if (exit === null) {
        child.kill("SIGKILL");
        await once(child, "exit");
      }
    },
  };
}

test(
  "Production L2 tunnel proxy: DHCP + ARP + DNS + TCP echo over aero-l2-tunnel-v1",
  // Allow enough time for `cargo run` to compile on a cold CI cache.
  { timeout: 300_000 },
  async () => {
    const echo = await startTcpEchoServer();
    try {
      const proxy = await startAeroL2Proxy({ echoPort: echo.port });

      try {
        const result = await runL2TunnelProbe({
          url: proxy.url,
          dnsName: "echo.local",
          echoPort: echo.port,
          // Keep payload small enough to fit within the default L2 tunnel max frame size (2048).
          // `aero-l2-proxy` reads from the forwarded TCP socket in 16KiB chunks; if we ask for a
          // large echo, the stack may attempt to emit a single oversized Ethernet frame which the
          // tunnel encoder will drop.
          throughputBytes: 1800,
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
