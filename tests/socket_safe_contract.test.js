import assert from "node:assert/strict";
import test from "node:test";

import {
  callMethodCaptureErrorBestEffort,
  callMethodBestEffort,
  destroyBestEffort,
  destroyWithErrorBestEffort,
  endCaptureErrorBestEffort,
  endRequired,
  pauseRequired,
  setNoDelayRequired,
  tryGetMethodBestEffort,
  writeCaptureErrorBestEffort,
} from "../src/socket_safe.js";

test("socket_safe: does not throw on hostile method getters", () => {
  const hostile = new Proxy(
    {},
    {
      get(_t, prop) {
        if (prop === "destroy") throw new Error("boom");
        if (prop === "end") throw new Error("boom");
        if (prop === "pause") throw new Error("boom");
        if (prop === "setNoDelay") throw new Error("boom");
        if (prop === "write") throw new Error("boom");
        return undefined;
      },
    },
  );

  assert.doesNotThrow(() => destroyBestEffort(hostile));
  assert.doesNotThrow(() => destroyWithErrorBestEffort(hostile, new Error("x")));
  assert.doesNotThrow(() => callMethodCaptureErrorBestEffort(hostile, "pause"));
  assert.doesNotThrow(() => tryGetMethodBestEffort(hostile, "pause"));
  assert.doesNotThrow(() => callMethodBestEffort(hostile, "pause"));
  assert.doesNotThrow(() => endCaptureErrorBestEffort(hostile));
  assert.doesNotThrow(() => writeCaptureErrorBestEffort(hostile, Buffer.from("x")));
  assert.equal(endRequired(hostile), false);
  assert.equal(pauseRequired(hostile), false);
  assert.equal(setNoDelayRequired(hostile, true), false);
});

test("socket_safe: endCaptureErrorBestEffort reports missing end()", () => {
  const obj = {};
  const err = endCaptureErrorBestEffort(obj);
  assert.ok(err instanceof Error);
  assert.ok(err.message.includes("Missing required method: end"));
});

test("socket_safe: callMethodCaptureErrorBestEffort reports missing methods", () => {
  const obj = {};
  const err = callMethodCaptureErrorBestEffort(obj, "write");
  assert.ok(err instanceof Error);
  assert.ok(err.message.includes("Missing required method: write"));
});

test("socket_safe: tryGetMethodBestEffort returns a function or null", () => {
  const obj = {
    write() {},
  };
  assert.equal(typeof tryGetMethodBestEffort(obj, "write"), "function");
  assert.equal(tryGetMethodBestEffort(obj, "missing"), null);
});

test("socket_safe: supports symbol keys", () => {
  const sym = Symbol("boom");
  let called = 0;
  const obj = {
    [sym]() {
      called += 1;
    },
  };

  assert.equal(typeof tryGetMethodBestEffort(obj, sym), "function");
  assert.equal(callMethodBestEffort(obj, sym), true);
  assert.equal(called, 1);
});

test("socket_safe: callMethodBestEffort returns true on missing method", () => {
  assert.equal(callMethodBestEffort({}, "missing"), true);
});

test("socket_safe: callMethodBestEffort returns false when method throws", () => {
  const obj = {
    boom() {
      throw new Error("boom");
    },
  };
  assert.equal(callMethodBestEffort(obj, "boom"), false);
});

test("socket_safe: writeCaptureErrorBestEffort reports missing write()", () => {
  const res = writeCaptureErrorBestEffort({}, Buffer.from("x"));
  assert.equal(res.ok, false);
  assert.ok(res.err instanceof Error);
  assert.ok(res.err.message.includes("Missing required method: write"));
});

test("socket_safe: writeCaptureErrorBestEffort returns ok=false when write() returns false", () => {
  const obj = {
    write() {
      return false;
    },
  };
  const res = writeCaptureErrorBestEffort(obj, Buffer.from("x"));
  assert.equal(res.ok, false);
  assert.equal(res.err, null);
});

test("socket_safe: writeCaptureErrorBestEffort returns ok=true when write() returns true", () => {
  const obj = {
    write() {
      return true;
    },
  };
  const res = writeCaptureErrorBestEffort(obj, Buffer.from("x"));
  assert.equal(res.ok, true);
  assert.equal(res.err, null);
});

test("socket_safe: writeCaptureErrorBestEffort returns err when write() throws", () => {
  const obj = {
    write() {
      throw new Error("boom");
    },
  };
  const res = writeCaptureErrorBestEffort(obj, Buffer.from("x"));
  assert.equal(res.ok, false);
  assert.ok(res.err instanceof Error);
});

