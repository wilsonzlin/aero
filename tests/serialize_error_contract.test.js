import assert from "node:assert/strict";
import test from "node:test";

import { serializeError } from "../src/errors.js";

test("serializeError: does not throw on hostile error-like objects", () => {
  const e = new Error("hello\n\tworld");
  Object.defineProperty(e, "message", {
    configurable: true,
    enumerable: true,
    get() {
      throw new Error("boom");
    },
  });
  Object.defineProperty(e, "name", {
    configurable: true,
    enumerable: true,
    get() {
      throw new Error("boom");
    },
  });
  Object.defineProperty(e, "stack", {
    configurable: true,
    enumerable: true,
    get() {
      throw new Error("boom");
    },
  });

  const out = serializeError(e);
  assert.equal(typeof out, "object");
  assert.equal(out.name, "Error");
  assert.equal(out.message, "Error");
  assert.equal(out.code, "InternalError");
});

test("serializeError: does not throw when instanceof Error would throw (proxy traps)", () => {
  const hostile = new Proxy(
    {},
    {
      getPrototypeOf() {
        throw new Error("boom");
      },
    },
  );
  assert.doesNotThrow(() => serializeError(hostile));
  const out = serializeError(hostile);
  assert.equal(out.message, "Error");
});

test("serializeError: message is single-line and byte-bounded", () => {
  const err = new Error("a\tb\nc");
  const out = serializeError(err);
  assert.equal(out.message, "a b c");
  assert.ok(Buffer.byteLength(out.message, "utf8") <= 512);
  assert.ok(!out.message.includes("\n"));
});

test("serializeError: does not stringify arbitrary objects", () => {
  const obj = {
    toString() {
      throw new Error("boom");
    },
  };
  const out = serializeError(obj);
  assert.equal(out.message, "Error");
});

test("serializeError: stack is UTF-8 byte-truncated", () => {
  const e = new Error("x");
  e.stack = "x".repeat(20 * 1024);
  const out = serializeError(e);
  assert.ok(out.stack);
  assert.ok(Buffer.byteLength(out.stack, "utf8") <= 8 * 1024);
});

