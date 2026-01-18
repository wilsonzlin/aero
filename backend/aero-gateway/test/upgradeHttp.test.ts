import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import { enforceUpgradeRequestUrlLimit, parseUpgradeRequestUrl, respondUpgradeHttp } from "../src/routes/upgradeHttp.js";

test("respondUpgradeHttp emits a single blank line before the body", async () => {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  respondUpgradeHttp(socket, 400, "bad");

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.includes("\r\nCache-Control: no-store\r\n"));
  assert.ok(text.includes("\r\n\r\nbad\n"));
  assert.ok(!text.includes("\r\n\r\n\r\nbad\n"));
});

test("enforceUpgradeRequestUrlLimit rejects overly long URLs with 414", async () => {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  const ok = enforceUpgradeRequestUrlLimit(`/tcp?${"a".repeat(9000)}`, socket);
  assert.equal(ok, false);

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 414 "));
});

test("parseUpgradeRequestUrl rejects invalid URLs with 400", async () => {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  const url = parseUpgradeRequestUrl("http://[::1", socket, { invalidUrlMessage: "Invalid request URL" });
  assert.equal(url, null);

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 400 "));
  assert.ok(text.includes("Invalid request URL\n"));
});

