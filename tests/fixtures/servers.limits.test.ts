import test from "node:test";
import assert from "node:assert/strict";

import { startDiskImageServer, buildTestImage } from "./servers";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_CORS_REQUEST_HEADERS_LEN = 4 * 1024;
const MAX_RANGE_HEADER_LEN = 16 * 1024;

function makeOversized(prefixLen: number): string {
  return "a".repeat(prefixLen + 1);
}

test("startDiskImageServer: rejects oversized Range headers with 413 (and still sets CORS headers)", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: true });
  try {
    const res = await fetch(`${server.origin}/disk.img`, {
      method: "GET",
      headers: {
        Origin: "https://example.com",
        Range: `bytes=0-0, ${"0".repeat(MAX_RANGE_HEADER_LEN)}`,
      },
    });

    assert.equal(res.status, 413);
    assert.equal(res.headers.get("access-control-allow-origin"), "https://example.com");
    assert.equal(res.headers.get("vary"), "Origin");
  } finally {
    await server.close();
  }
});

test("startDiskImageServer: does not reflect oversized Access-Control-Request-Headers", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: true });
  try {
    const oversized = makeOversized(MAX_CORS_REQUEST_HEADERS_LEN);
    const res = await fetch(`${server.origin}/disk.img`, {
      method: "OPTIONS",
      headers: {
        Origin: "https://example.com",
        "Access-Control-Request-Method": "GET",
        "Access-Control-Request-Headers": oversized,
      },
    });

    assert.equal(res.status, 204);
    assert.equal(res.headers.get("access-control-allow-origin"), "https://example.com");
    assert.equal(res.headers.get("access-control-allow-headers"), "Range");
  } finally {
    await server.close();
  }
});

test("startDiskImageServer: rejects overly long request targets with 414", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: true });
  try {
    const longUrl = `${server.origin}/disk.img?x=${makeOversized(MAX_REQUEST_URL_LEN)}`;
    const res = await fetch(longUrl, { method: "GET", headers: { Origin: "https://example.com" } });
    assert.equal(res.status, 414);
    assert.equal(res.headers.get("access-control-allow-origin"), "https://example.com");
  } finally {
    await server.close();
  }
});

