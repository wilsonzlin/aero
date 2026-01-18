import test from "node:test";
import assert from "node:assert/strict";

import { closeBestEffort, destroyBestEffort, endCaptureErrorBestEffort } from "../socketSafe";

test("destroyBestEffort does not throw if destroy getter throws", () => {
  const obj = {};
  Object.defineProperty(obj, "destroy", {
    get() {
      throw new Error("boom");
    }
  });
  assert.doesNotThrow(() => destroyBestEffort(obj));
});

test("closeBestEffort does not throw if close getter throws", () => {
  const obj = {};
  Object.defineProperty(obj, "close", {
    get() {
      throw new Error("boom");
    }
  });
  assert.doesNotThrow(() => closeBestEffort(obj));
});

test("endCaptureErrorBestEffort returns null when end succeeds", () => {
  const obj = {
    end() {
      // ok
    }
  };
  assert.equal(endCaptureErrorBestEffort(obj), null);
});

test("endCaptureErrorBestEffort returns thrown error when end throws", () => {
  const boom = new Error("boom");
  const obj = {
    end() {
      throw boom;
    }
  };
  assert.equal(endCaptureErrorBestEffort(obj), boom);
});

test("endCaptureErrorBestEffort does not throw if end getter throws", () => {
  const obj = {};
  Object.defineProperty(obj, "end", {
    get() {
      throw new Error("boom");
    }
  });
  assert.doesNotThrow(() => {
    const err = endCaptureErrorBestEffort(obj);
    assert.ok(err instanceof Error);
  });
});

test("endCaptureErrorBestEffort returns Error when end is missing", () => {
  const err = endCaptureErrorBestEffort({});
  assert.ok(err instanceof Error);
});

