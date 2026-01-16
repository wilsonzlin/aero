import assert from "node:assert/strict";
import test from "node:test";

import { formatOneLineUtf8, sanitizeOneLine, truncateUtf8 } from "../text";

test("text: sanitizeOneLine collapses whitespace and removes control chars", () => {
  assert.equal(sanitizeOneLine(""), "");
  assert.equal(sanitizeOneLine("  a  "), "a");
  assert.equal(sanitizeOneLine("a\tb\nc"), "a b c");
  assert.equal(sanitizeOneLine("a\u0000b"), "a b");
  assert.equal(sanitizeOneLine("\u0000"), "");
  assert.equal(sanitizeOneLine("a\u2028b"), "a b");
  assert.equal(sanitizeOneLine("a\u2029b"), "a b");
  assert.equal(sanitizeOneLine("a\u00a0b"), "a b"); // NBSP
});

test("text: truncateUtf8 is safe and byte-bounded", () => {
  assert.equal(truncateUtf8("hello", 5), "hello");
  assert.equal(truncateUtf8("hello", 4), "hell");

  assert.equal(truncateUtf8("€", 3), "€");
  assert.equal(truncateUtf8("€", 2), "");

  assert.equal(truncateUtf8("🙂", 4), "🙂");
  assert.equal(truncateUtf8("🙂", 3), "");

  assert.equal(truncateUtf8("€a", 3), "€");
  assert.equal(truncateUtf8("a🙂b", 5), "a🙂");

  assert.equal(truncateUtf8("x", -1), "");
  assert.equal(truncateUtf8("x", 1.2), "");
});

test("text: formatOneLineUtf8 composes sanitizeOneLine + truncateUtf8", () => {
  assert.equal(formatOneLineUtf8("a\tb\nc", 512), "a b c");
  assert.equal(formatOneLineUtf8("a\u00a0b", 512), "a b");
  assert.equal(formatOneLineUtf8("🙂", 3), "");
});

