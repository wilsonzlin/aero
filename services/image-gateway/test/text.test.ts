import { describe, expect, it } from "vitest";

import { formatOneLineError, formatOneLineUtf8, sanitizeOneLine, truncateUtf8 } from "../src/text";
import * as rootText from "../../../src/text.js";

describe("text", () => {
  it("matches repo-root text helpers (parity)", () => {
    const cases = [
      { input: "", maxBytes: 512 },
      { input: "  a  ", maxBytes: 512 },
      { input: "a\tb\nc", maxBytes: 512 },
      { input: "a\u0000b", maxBytes: 512 },
      { input: "\u0000", maxBytes: 512 },
      { input: "a\u2028b", maxBytes: 512 },
      { input: "a\u2029b", maxBytes: 512 },
      { input: "a\u00a0b", maxBytes: 512 }, // NBSP
      { input: "🙂", maxBytes: 3 },
      { input: "x".repeat(600), maxBytes: 512 },
    ];

    for (const { input, maxBytes } of cases) {
      expect(sanitizeOneLine(input)).toBe(rootText.sanitizeOneLine(input));
      expect(truncateUtf8(input, maxBytes)).toBe(rootText.truncateUtf8(input, maxBytes));
      expect(formatOneLineUtf8(input, maxBytes)).toBe(rootText.formatOneLineUtf8(input, maxBytes));
      expect(formatOneLineError(input, maxBytes)).toBe(rootText.formatOneLineError(input, maxBytes));
    }
  });

  it("sanitizeOneLine collapses whitespace and removes control chars", () => {
    expect(sanitizeOneLine("")).toBe("");
    expect(sanitizeOneLine("  a  ")).toBe("a");
    expect(sanitizeOneLine("a\tb\nc")).toBe("a b c");
    expect(sanitizeOneLine("a\u0000b")).toBe("a b");
    expect(sanitizeOneLine("\u0000")).toBe("");
    expect(sanitizeOneLine("a\u2028b")).toBe("a b");
    expect(sanitizeOneLine("a\u2029b")).toBe("a b");
    expect(sanitizeOneLine("a\u00a0b")).toBe("a b"); // NBSP
  });

  it("truncateUtf8 is safe and byte-bounded", () => {
    expect(truncateUtf8("hello", 5)).toBe("hello");
    expect(truncateUtf8("hello", 4)).toBe("hell");

    expect(truncateUtf8("€", 3)).toBe("€");
    expect(truncateUtf8("€", 2)).toBe("");

    expect(truncateUtf8("🙂", 4)).toBe("🙂");
    expect(truncateUtf8("🙂", 3)).toBe("");

    expect(truncateUtf8("€a", 3)).toBe("€");
    expect(truncateUtf8("a🙂b", 5)).toBe("a🙂");

    expect(truncateUtf8("x", -1)).toBe("");
    expect(truncateUtf8("x", 1.2)).toBe("");
  });

  it("formatOneLineUtf8 composes sanitizeOneLine + truncateUtf8", () => {
    expect(formatOneLineUtf8("a\tb\nc", 512)).toBe("a b c");
    expect(formatOneLineUtf8("a\u00a0b", 512)).toBe("a b");
    expect(formatOneLineUtf8("🙂", 3)).toBe("");
  });

  it("formatOneLineError is safe and byte-bounded", () => {
    const throwingMessage = Object.create(null, {
      message: {
        enumerable: true,
        get() {
          throw new Error("boom");
        },
      },
    });

    expect(formatOneLineError(new Error("a\tb\nc"), 512)).toBe("a b c");
    expect(formatOneLineError({ message: "x\ny" }, 512)).toBe("x y");
    expect(formatOneLineError(throwingMessage, 512)).toBe("Error");
    expect(formatOneLineError({}, 512)).toBe("Error");
    expect(formatOneLineError(() => {}, 512)).toBe("Error");
    expect(formatOneLineError(123, 512)).toBe("123");
    expect(formatOneLineError(null, 512)).toBe("null");
    expect(formatOneLineError("🙂", 3)).toBe("Error");

    const long = formatOneLineError("x".repeat(600), 512);
    expect(long.includes("\n")).toBe(false);
    expect(Buffer.byteLength(long, "utf8")).toBeLessThanOrEqual(512);
  });
});

