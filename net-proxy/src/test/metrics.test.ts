import test from "node:test";
import assert from "node:assert/strict";
import { startProxyServer } from "../server";

async function fetchText(url: string, timeoutMs = 2_000): Promise<{ status: number; contentType: string | null; body: string }> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  timeout.unref();
  try {
    const res = await fetch(url, { signal: controller.signal });
    return { status: res.status, contentType: res.headers.get("content-type"), body: await res.text() };
  } finally {
    clearTimeout(timeout);
  }
}

test("proxy exposes /metrics with expected metric names", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { status, contentType, body } = await fetchText(`${origin}/metrics`);
    assert.equal(status, 200);
    assert.ok(contentType?.includes("text/plain"), `expected text/plain content-type, got ${contentType ?? "null"}`);

    for (const name of [
      "net_proxy_connections_active",
      "net_proxy_tcp_connections_active",
      "net_proxy_udp_bindings_active",
      "net_proxy_bytes_in_total",
      "net_proxy_bytes_out_total",
      "net_proxy_connection_errors_total"
    ]) {
      assert.ok(body.includes(name), `missing metric ${name}`);
    }
  } finally {
    await proxy.close();
  }
});
