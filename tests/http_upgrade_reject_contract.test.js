import assert from "node:assert/strict";
import test from "node:test";
import { PassThrough } from "node:stream";

import { rejectHttpUpgrade } from "../src/http_upgrade_reject.js";

test("http_upgrade_reject: emits a well-formed 4xx response with bounded body", () => {
  const socket = new PassThrough();
  const chunks = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  rejectHttpUpgrade(socket, 400, "Bad\r\n\tRequest");

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 400 Bad Request\r\n"));
  assert.ok(text.includes("\r\nContent-Type: text/plain; charset=utf-8\r\n"));
  assert.ok(text.includes("\r\nCache-Control: no-store\r\n"));
  assert.ok(text.includes("\r\nConnection: close\r\n"));
  assert.ok(text.includes("\r\n\r\nBad Request\n"));
  assert.ok(!text.includes("\r\n\r\n\r\nBad Request\n"));

  const lenMatch = text.match(/\r\nContent-Length: (\d+)\r\n/u);
  assert.ok(lenMatch, "expected Content-Length header");
  assert.equal(Number(lenMatch[1]), Buffer.byteLength("Bad Request\n"));
});

test("http_upgrade_reject: falls back to status text for hostile message values", () => {
  const socket = new PassThrough();
  const chunks = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  const hostile = {
    toString() {
      throw new Error("nope");
    },
  };
  rejectHttpUpgrade(socket, 400, hostile);

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.includes("\r\n\r\nBad Request\n"));
});

test("http_upgrade_reject: destroys socket if response encoding fails", () => {
  let destroyed = false;
  const socket = {
    destroy() {
      destroyed = true;
    },
  };

  rejectHttpUpgrade(socket, 99, "Bad Request");
  assert.equal(destroyed, true);
});

test("http_upgrade_reject: supports 500 Internal Server Error status text", () => {
  const socket = new PassThrough();
  const chunks = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  rejectHttpUpgrade(socket, 500, "WebSocket upgrade failed");

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 500 Internal Server Error\r\n"));
  assert.ok(text.includes("\r\n\r\nWebSocket upgrade failed\n"));
});
