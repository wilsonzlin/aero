import assert from "node:assert/strict";
import test from "node:test";

import * as esm from "../src/socket_safe.js";
import * as cjs from "../src/socket_safe.cjs";

test("socket_safe: ESM/CJS parity for basic no-op behavior", () => {
  const obj = {};

  assert.equal(typeof esm.callMethodCaptureErrorBestEffort(obj, "write"), "object");
  assert.equal(typeof cjs.callMethodCaptureErrorBestEffort(obj, "write"), "object");

  assert.equal(esm.tryGetMethodBestEffort(obj, "write"), null);
  assert.equal(cjs.tryGetMethodBestEffort(obj, "write"), null);

  assert.equal(esm.callMethodBestEffort(obj, "missing"), true);
  assert.equal(cjs.callMethodBestEffort(obj, "missing"), true);

  const sym = Symbol("symMethod");
  const objWithSym = {
    [sym]() {
      return 123;
    },
  };
  assert.equal(typeof esm.tryGetMethodBestEffort(objWithSym, sym), "function");
  assert.equal(typeof cjs.tryGetMethodBestEffort(objWithSym, sym), "function");
  assert.equal(esm.callMethodBestEffort(objWithSym, sym), true);
  assert.equal(cjs.callMethodBestEffort(objWithSym, sym), true);

  assert.doesNotThrow(() => esm.destroyBestEffort(obj));
  assert.doesNotThrow(() => cjs.destroyBestEffort(obj));

  assert.equal(typeof esm.endCaptureErrorBestEffort(obj), "object");
  assert.equal(typeof cjs.endCaptureErrorBestEffort(obj), "object");

  assert.equal(esm.writeCaptureErrorBestEffort(obj, Buffer.from("x")).ok, false);
  assert.equal(cjs.writeCaptureErrorBestEffort(obj, Buffer.from("x")).ok, false);
  assert.ok(esm.writeCaptureErrorBestEffort(obj, Buffer.from("x")).err instanceof Error);
  assert.ok(cjs.writeCaptureErrorBestEffort(obj, Buffer.from("x")).err instanceof Error);

  assert.equal(esm.pauseRequired(obj), false);
  assert.equal(cjs.pauseRequired(obj), false);

  assert.equal(esm.setNoDelayRequired(obj, true), false);
  assert.equal(cjs.setNoDelayRequired(obj, true), false);
});

