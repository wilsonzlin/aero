import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { startStaticServer } from "./playwright_static_server.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;

function makeOversized(maxLen: number): string {
  return "a".repeat(maxLen + 1);
}

test("playwright static server: rejects overly long request targets with 414", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer(dir, { defaultPath: "/index.html" });
  try {
    const longUrl = `${server.baseUrl}/index.html?x=${makeOversized(MAX_REQUEST_URL_LEN)}`;
    const res = await fetch(longUrl);
    assert.equal(res.status, 414);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("playwright static server: rejects invalid percent-encoding with 400", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer(dir, { defaultPath: "/index.html" });
  try {
    const res = await fetch(`${server.baseUrl}/%E0%A4%A`); // invalid sequence => decodeURIComponent throws
    assert.equal(res.status, 400);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("playwright static server: rejects NUL bytes in the path with 400", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer(dir, { defaultPath: "/index.html" });
  try {
    const res = await fetch(`${server.baseUrl}/%00index.html`);
    assert.equal(res.status, 400);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("playwright static server: blocks path traversal with 403", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-static-server-"));
  fs.writeFileSync(path.join(dir, "index.html"), "<!doctype html><title>ok</title>", "utf8");

  const server = await startStaticServer(dir, { defaultPath: "/index.html" });
  try {
    // Avoid WHATWG URL path normalization by encoding the slash too.
    const res = await fetch(`${server.baseUrl}/%2e%2e%2fsecret.txt`);
    assert.equal(res.status, 403);
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

