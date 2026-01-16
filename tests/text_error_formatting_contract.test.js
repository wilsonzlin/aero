import assert from "node:assert/strict";
import test from "node:test";

import { formatOneLineError } from "../src/text.js";

test("text: formatOneLineError returns single-line, byte-bounded messages", () => {
  assert.equal(formatOneLineError(new Error("a\tb\nc"), 512), "a b c");

  // Error-like object (common across runtimes) should use its message.
  assert.equal(formatOneLineError({ message: "x\ny" }, 512), "x y");

  // Throwing `.message` getters must not crash error formatting.
  const throwingMessage = Object.create(null, {
    message: {
      enumerable: true,
      get() {
        throw new Error("boom");
      },
    },
  });
  assert.equal(formatOneLineError(throwingMessage, 512), "Error");

  // Non-Error objects should not stringify by default.
  assert.equal(formatOneLineError({}, 512), "Error");
  assert.equal(formatOneLineError(() => {}, 512), "Error");

  // Primitive values can be safely stringified.
  assert.equal(formatOneLineError(123, 512), "123");
  assert.equal(formatOneLineError(null, 512), "null");

  // Byte cap applies to UTF-8 encoded output; if nothing fits, fall back.
  assert.equal(formatOneLineError("x".repeat(600), 512), "x".repeat(512));
  assert.equal(formatOneLineError("🙂", 3), "Error");
});

