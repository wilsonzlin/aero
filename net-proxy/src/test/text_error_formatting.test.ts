import assert from "node:assert/strict";
import test from "node:test";

import { formatOneLineError } from "../text";

test("text: formatOneLineError returns single-line, byte-bounded messages", () => {
  assert.equal(formatOneLineError(new Error("a\tb\nc"), 512), "a b c");

  // Error-like object (common across runtimes) should use its message.
  assert.equal(formatOneLineError({ message: "x\ny" }, 512), "x y");

  const throwingMessage = Object.create(null, {
    message: {
      enumerable: true,
      get() {
        throw new Error("boom");
      },
    },
  });

  // Throwing `.message` getters must not crash error formatting.
  assert.equal(formatOneLineError(throwingMessage, 512), "Error");

  // Non-Error objects should not stringify by default.
  assert.equal(formatOneLineError({}, 512), "Error");
  assert.equal(formatOneLineError(() => {}, 512), "Error");

  // Primitive values can be safely stringified.
  assert.equal(formatOneLineError(123, 512), "123");
  assert.equal(formatOneLineError(null, 512), "null");

  // Byte cap applies to UTF-8 encoded output; if nothing fits, fall back.
  const capped = formatOneLineError("x".repeat(600), 512);
  assert.equal(capped, "x".repeat(512));
  assert.ok(Buffer.byteLength(capped, "utf8") <= 512);
  assert.equal(formatOneLineError("🙂", 3), "Error");
});

