import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { startStaticServer } from "../bench/server.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;

function makeOversized(maxLen) {
  return "a".repeat(maxLen + 1);
}

test("bench static server: rejects overly long request targets with 414", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-bench-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer({ rootDir: dir });
  try {
    const longUrl = `${server.baseUrl}index.html?x=${makeOversized(MAX_REQUEST_URL_LEN)}`;
    const res = await fetch(longUrl);
    assert.equal(res.status, 414);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("bench static server: rejects invalid percent-encoding with 400", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-bench-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer({ rootDir: dir });
  try {
    const res = await fetch(`${server.baseUrl}%E0%A4%A`);
    assert.equal(res.status, 400);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("bench static server: rejects NUL bytes in the path with 400", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-bench-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer({ rootDir: dir });
  try {
    const res = await fetch(`${server.baseUrl}%00index.html`);
    assert.equal(res.status, 400);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("bench static server: blocks path traversal with 403", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-bench-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer({ rootDir: dir });
  try {
    const res = await fetch(`${server.baseUrl}%2e%2e%2fsecret.txt`);
    assert.equal(res.status, 403);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("bench static server: rejects non-GET/HEAD methods with 405 and Allow header", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-bench-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer({ rootDir: dir });
  try {
    const res = await fetch(`${server.baseUrl}index.html`, { method: "POST" });
    assert.equal(res.status, 405);
    assert.equal(res.headers.get("allow"), "GET, HEAD, OPTIONS");
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

