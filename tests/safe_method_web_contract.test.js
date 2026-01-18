import assert from "node:assert/strict";
import test from "node:test";

import { callMethodBestEffort, destroyBestEffort, tryGetMethodBestEffort } from "../web/src/safeMethod.ts";

test("safeMethod(web): tryGetMethodBestEffort returns function or null", () => {
  assert.equal(tryGetMethodBestEffort(null, "x"), null);
  assert.equal(tryGetMethodBestEffort(undefined, "x"), null);
  assert.equal(tryGetMethodBestEffort(123, "x"), null);
  assert.equal(tryGetMethodBestEffort({}, "x"), null);

  const obj = { f() {} };
  const fn = tryGetMethodBestEffort(obj, "f");
  assert.equal(typeof fn, "function");
});

test("safeMethod(web): supports symbol keys", () => {
  const sym = Symbol("destroy");
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

test("safeMethod(web): tryGetMethodBestEffort does not throw on hostile getter", () => {
  const hostile = {
    get destroy() {
      throw new Error("boom");
    },
  };
  assert.doesNotThrow(() => tryGetMethodBestEffort(hostile, "destroy"));
  assert.equal(tryGetMethodBestEffort(hostile, "destroy"), null);
});

test("safeMethod(web): callMethodBestEffort returns true on missing method", () => {
  assert.equal(callMethodBestEffort(null, "destroy"), true);
  assert.equal(callMethodBestEffort({}, "destroy"), true);
});

test("safeMethod(web): callMethodBestEffort returns false when method throws", () => {
  const obj = {
    destroy() {
      throw new Error("boom");
    },
  };
  assert.equal(callMethodBestEffort(obj, "destroy"), false);
});

test("safeMethod(web): callMethodBestEffort does not throw on hostile getter", () => {
  const obj = {
    get destroy() {
      throw new Error("boom");
    },
  };
  assert.doesNotThrow(() => callMethodBestEffort(obj, "destroy"));
  assert.equal(callMethodBestEffort(obj, "destroy"), true);
});

test("safeMethod(web): destroyBestEffort calls destroy() when present", () => {
  let called = 0;
  const obj = {
    destroy() {
      called += 1;
    },
  };
  destroyBestEffort(obj);
  assert.equal(called, 1);
});

