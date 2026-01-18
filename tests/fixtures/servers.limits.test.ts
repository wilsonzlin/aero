import test from "node:test";
import assert from "node:assert/strict";

import { startDiskImageServer, startPageServer, buildTestImage } from "./servers";

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
    assert.equal(res.headers.get("allow"), "GET, HEAD, OPTIONS");
    assert.equal(res.headers.get("content-length"), "0");
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

test("startDiskImageServer: rejects non-GET/HEAD methods with 405, Allow, and Content-Length: 0", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: true });
  try {
    const res = await fetch(`${server.origin}/disk.img`, { method: "POST", headers: { Origin: "https://example.com" } });
    assert.equal(res.status, 405);
    assert.equal(res.headers.get("access-control-allow-origin"), "https://example.com");
    assert.equal(res.headers.get("allow"), "GET, HEAD, OPTIONS");
    assert.equal(res.headers.get("content-length"), "0");
  } finally {
    await server.close();
  }
});

test("startDiskImageServer (serveTestPage): GET/HEAD set deterministic Content-Length", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: false, serveTestPage: true });
  try {
    const getRes = await fetch(`${server.origin}/`, { method: "GET" });
    assert.equal(getRes.status, 200);
    assert.equal(getRes.headers.get("content-type"), "text/html; charset=utf-8");
    const getBytes = new Uint8Array(await getRes.arrayBuffer());
    assert.equal(getRes.headers.get("content-length"), String(getBytes.byteLength));

    const headRes = await fetch(`${server.origin}/`, { method: "HEAD" });
    assert.equal(headRes.status, 200);
    assert.equal(headRes.headers.get("content-type"), "text/html; charset=utf-8");
    assert.equal(headRes.headers.get("content-length"), String(getBytes.byteLength));
    const headBytes = new Uint8Array(await headRes.arrayBuffer());
    assert.equal(headBytes.byteLength, 0);
  } finally {
    await server.close();
  }
});

test("startDiskImageServer (serveTestPage): OPTIONS/POST return 204/405 with Allow", async () => {
  const server = await startDiskImageServer({ data: buildTestImage(1024), enableCors: false, serveTestPage: true });
  try {
    const optRes = await fetch(`${server.origin}/`, { method: "OPTIONS" });
    assert.equal(optRes.status, 204);
    assert.equal(optRes.headers.get("allow"), "GET, HEAD, OPTIONS");
    assert.equal(optRes.headers.get("content-length"), "0");

    const postRes = await fetch(`${server.origin}/`, { method: "POST" });
    assert.equal(postRes.status, 405);
    assert.equal(postRes.headers.get("allow"), "GET, HEAD, OPTIONS");
    assert.equal(postRes.headers.get("content-length"), "0");
  } finally {
    await server.close();
  }
});

test("startPageServer: rejects non-GET/HEAD methods with 405 and Allow", async () => {
  const server = await startPageServer();
  try {
    const res = await fetch(`${server.origin}/`, { method: "POST" });
    assert.equal(res.status, 405);
    assert.equal(res.headers.get("allow"), "GET, HEAD, OPTIONS");
  } finally {
    await server.close();
  }
});

test("startPageServer: handles OPTIONS with 204 and Content-Length: 0", async () => {
  const server = await startPageServer();
  try {
    const res = await fetch(`${server.origin}/`, { method: "OPTIONS" });
    assert.equal(res.status, 204);
    assert.equal(res.headers.get("allow"), "GET, HEAD, OPTIONS");
    assert.equal(res.headers.get("content-length"), "0");
  } finally {
    await server.close();
  }
});

