import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import { rejectWsUpgrade } from "../wsUpgradeHttp";

test("rejectWsUpgrade emits a single blank line before the body", async () => {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));

  rejectWsUpgrade(socket, 400, "Bad\r\n\tRequest");

  const text = Buffer.concat(chunks).toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 400 Bad Request\r\n"));
  assert.ok(text.includes("\r\nCache-Control: no-store\r\n"));
  assert.ok(text.includes("\r\nConnection: close\r\n"));
  assert.ok(text.includes("\r\n\r\nBad Request\n"));
  assert.ok(!text.includes("\r\n\r\n\r\nBad Request\n"));

  const lenMatch = text.match(/\r\nContent-Length: (\d+)\r\n/u);
  assert.ok(lenMatch, "expected Content-Length header");
  assert.equal(Number(lenMatch[1]), Buffer.byteLength("Bad Request\n"));
});

test("rejectWsUpgrade destroys the socket if end throws", () => {
  let destroyed = false;
  const socket = {
    end() {
      throw new Error("boom");
    },
    destroy() {
      destroyed = true;
    },
  } as unknown as import("node:stream").Duplex;

  rejectWsUpgrade(socket, 400, "Bad Request");
  assert.equal(destroyed, true);
});
