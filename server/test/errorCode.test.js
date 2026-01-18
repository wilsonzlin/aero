import test from "node:test";
import assert from "node:assert/strict";

import { tryGetErrorCode } from "../src/errorCode.js";

test("tryGetErrorCode: returns string error codes", () => {
  assert.equal(tryGetErrorCode({ code: "ECONNRESET" }), "ECONNRESET");
  assert.equal(tryGetErrorCode({ code: "" }), "");
});

test("tryGetErrorCode: returns undefined for non-string codes", () => {
  assert.equal(tryGetErrorCode({ code: 123 }), undefined);
  assert.equal(tryGetErrorCode({}), undefined);
  assert.equal(tryGetErrorCode(null), undefined);
});

test("tryGetErrorCode: does not throw on hostile code getter", () => {
  const hostile = {};
  Object.defineProperty(hostile, "code", {
    get() {
      throw new Error("boom");
    },
  });

  assert.doesNotThrow(() => tryGetErrorCode(hostile));
  assert.equal(tryGetErrorCode(hostile), undefined);
});
