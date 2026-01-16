import assert from "node:assert/strict";
import test from "node:test";

const src = await import(new URL("../src/text.js", import.meta.url));
const pub = await import(new URL("../web/public/_shared/text_one_line.js", import.meta.url));

test("web/public text_one_line: formatOneLineUtf8 matches src/text", () => {
  const cases = [
    { input: "a\tb\nc", maxBytes: 512 },
    { input: "a\u00a0b", maxBytes: 512 }, // NBSP
    { input: "🙂", maxBytes: 3 },
    { input: "x".repeat(600), maxBytes: 512 },
    { input: "", maxBytes: 0 },
  ];

  for (const { input, maxBytes } of cases) {
    assert.equal(pub.formatOneLineUtf8(input, maxBytes), src.formatOneLineUtf8(input, maxBytes));
  }
});

test("web/public text_one_line: formatOneLineError default matches src/text", () => {
  const cases = [
    { err: new Error("a\tb\nc"), maxBytes: 512 },
    { err: { message: "x\ny" }, maxBytes: 512 },
    { err: {}, maxBytes: 512 },
    { err: () => {}, maxBytes: 512 },
    { err: 123, maxBytes: 512 },
    { err: null, maxBytes: 512 },
    { err: "🙂", maxBytes: 3 },
  ];
  for (const { err, maxBytes } of cases) {
    assert.equal(pub.formatOneLineError(err, maxBytes), src.formatOneLineError(err, maxBytes));
  }
});

test("web/public text_one_line: includeNameFallback modes behave as intended", () => {
  assert.equal(pub.formatOneLineError({ name: "Boom" }, 512, { includeNameFallback: false }), "Error");
  assert.equal(pub.formatOneLineError({ name: "Boom" }, 512, { includeNameFallback: "missing" }), "Boom");
  assert.equal(pub.formatOneLineError({ message: "", name: "Boom" }, 512, { includeNameFallback: "missing" }), "Error");
  assert.equal(pub.formatOneLineError({ message: "", name: "Boom" }, 512, { includeNameFallback: true }), "Boom");
});

