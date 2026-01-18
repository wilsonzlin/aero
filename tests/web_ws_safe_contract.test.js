import assert from "node:assert/strict";
import test from "node:test";

import { wsCloseSafe, wsIsClosedSafe, wsIsOpenSafe, wsProtocolSafe, wsSendSafe } from "../web/src/net/wsSafe.ts";

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

test("web wsSafe: wsIsOpenSafe returns false on invalid ws input", () => {
  assert.equal(wsIsOpenSafe(null), false);
  assert.equal(wsIsOpenSafe(undefined), false);
});

test("web wsSafe: wsIsOpenSafe returns true only when readyState is OPEN", () => {
  assert.equal(wsIsOpenSafe({ readyState: 0 }), false);
  assert.equal(wsIsOpenSafe({ readyState: 1 }), true);
  assert.equal(wsIsOpenSafe({ readyState: 2 }), false);
  assert.equal(wsIsOpenSafe({ readyState: 3 }), false);
});

test("web wsSafe: wsIsOpenSafe returns false if readyState getter throws", () => {
  const ws = {};
  Object.defineProperty(ws, "readyState", {
    get() {
      throw new Error("nope");
    },
  });
  assert.equal(wsIsOpenSafe(ws), false);
});

test("web wsSafe: wsIsClosedSafe returns true only when readyState is CLOSED", () => {
  assert.equal(wsIsClosedSafe({ readyState: 0 }), false);
  assert.equal(wsIsClosedSafe({ readyState: 1 }), false);
  assert.equal(wsIsClosedSafe({ readyState: 2 }), false);
  assert.equal(wsIsClosedSafe({ readyState: 3 }), true);
});

test("web wsSafe: wsProtocolSafe returns protocol string or null", () => {
  assert.equal(wsProtocolSafe(null), null);
  assert.equal(wsProtocolSafe({ protocol: "aero-v1" }), "aero-v1");
  assert.equal(wsProtocolSafe({ protocol: 123 }), null);
});

test("web wsSafe: wsSendSafe returns false when not open", () => {
  const calls = [];
  const ws = { readyState: 0, send: () => calls.push("send") };
  assert.equal(wsSendSafe(ws, new Uint8Array([1])), false);
  assert.deepEqual(calls, []);
});

test("web wsSafe: wsSendSafe returns true and sends when open", () => {
  const calls = [];
  const ws = { readyState: 1, send: (...args) => calls.push(args) };
  assert.equal(wsSendSafe(ws, "hi"), true);
  assert.equal(calls.length, 1);
});

test("web wsSafe: wsSendSafe returns false if send throws", () => {
  const ws = {
    readyState: 1,
    send: () => {
      throw new Error("boom");
    },
  };
  assert.equal(wsSendSafe(ws, "hi"), false);
});
