import assert from "node:assert/strict";
import test from "node:test";

import { wsCloseSafe } from "../web/src/net/wsSafe.ts";

test("web wsSafe: wsCloseSafe formats and UTF-8 byte-limits the close reason", () => {
  const calls = [];
  const ws = {
    close: (...args) => calls.push(args),
  };

  wsCloseSafe(ws, 1000, "hello\n\r\tworld " + "x".repeat(500));
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], 1000);
  assert.equal(typeof calls[0][1], "string");
  assert.ok(!/[\r\n]/u.test(calls[0][1]));
  assert.ok(Buffer.byteLength(calls[0][1], "utf8") <= 123);
});

test("web wsSafe: wsCloseSafe treats empty reason as absent", () => {
  const calls = [];
  const ws = {
    close: (...args) => calls.push(args),
  };

  wsCloseSafe(ws, 1000, "");
  assert.deepEqual(calls, [[1000]]);
});

test("web wsSafe: wsCloseSafe does not throw on hostile reason inputs", () => {
  const calls = [];
  const ws = {
    close: (...args) => calls.push(args),
  };

  const hostile = { toString: () => { throw new Error("nope"); } };
  wsCloseSafe(ws, 1000, hostile);
  assert.deepEqual(calls, [[1000]]);
});
