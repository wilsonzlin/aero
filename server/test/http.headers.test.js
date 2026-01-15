import test from "node:test";
import assert from "node:assert/strict";

import { createAeroServer } from "../src/server.js";
import { resolveConfig } from "../src/config.js";

async function listen(server) {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return address.port;
}

async function closeServer(server) {
  await new Promise((resolve) => server.close(resolve));
}

test("serves static files with COOP/COEP headers", async () => {
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: ["test-token"],
  });
  const { httpServer } = createAeroServer(config);

  const port = await listen(httpServer);
  try {
    const res = await fetch(`http://127.0.0.1:${port}/`);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("cross-origin-opener-policy"), "same-origin");
    assert.equal(res.headers.get("cross-origin-embedder-policy"), "require-corp");
    assert.equal(res.headers.get("origin-agent-cluster"), "?1");
    const text = await res.text();
    assert.match(text, /Aero backend server/);
  } finally {
    await closeServer(httpServer);
  }
});

test("rejects overly long request URLs with 414 (and still applies COOP/COEP headers)", async () => {
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: ["test-token"],
  });
  const { httpServer } = createAeroServer(config);

  const port = await listen(httpServer);
  try {
    const qs = "a".repeat(9_000);
    const res = await fetch(`http://127.0.0.1:${port}/?${qs}`);
    assert.equal(res.status, 414);
    assert.equal(res.headers.get("cross-origin-opener-policy"), "same-origin");
    assert.equal(res.headers.get("cross-origin-embedder-policy"), "require-corp");
    assert.equal(res.headers.get("origin-agent-cluster"), "?1");
  } finally {
    await closeServer(httpServer);
  }
});

