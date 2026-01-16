import assert from "node:assert/strict";
import test from "node:test";

const MODULES = [
  {
    name: "backend/aero-gateway",
    mod: await import(new URL("../backend/aero-gateway/src/util/text.ts", import.meta.url)),
  },
  { name: "server", mod: await import(new URL("../server/src/text.js", import.meta.url)) },
  {
    name: "tools/net-proxy-server",
    mod: await import(new URL("../tools/net-proxy-server/src/text.js", import.meta.url)),
  },
  { name: "src", mod: await import(new URL("../src/text.js", import.meta.url)) },
  { name: "web", mod: await import(new URL("../web/src/text.ts", import.meta.url)) },
];

function impl(name, mod, key) {
  const fn = mod[key];
  assert.equal(typeof fn, "function", `${name} missing ${key}()`);
  return fn;
}

test("text helpers: sanitizeOneLine is consistent", () => {
  const cases = [
    { input: "", expected: "" },
    { input: "  a  ", expected: "a" },
    { input: "a\tb\nc", expected: "a b c" },
    { input: "a\u0000b", expected: "a b" },
    { input: "\u0000", expected: "" },
    { input: "a\u2028b", expected: "a b" },
    { input: "a\u2029b", expected: "a b" },
    { input: "a\u00a0b", expected: "a b" }, // NBSP
  ];

  for (const { name, mod } of MODULES) {
    const sanitize = impl(name, mod, "sanitizeOneLine");
    for (const { input, expected } of cases) {
      assert.equal(sanitize(input), expected, `${name}.sanitizeOneLine mismatch`);
    }
  }
});

test("text helpers: truncateUtf8 is consistent", () => {
  const cases = [
    { input: "hello", maxBytes: 5, expected: "hello" },
    { input: "hello", maxBytes: 4, expected: "hell" },
    { input: "€", maxBytes: 3, expected: "€" },
    { input: "€", maxBytes: 2, expected: "" },
    { input: "🙂", maxBytes: 4, expected: "🙂" },
    { input: "🙂", maxBytes: 3, expected: "" },
    { input: "€a", maxBytes: 3, expected: "€" },
    { input: "a🙂b", maxBytes: 5, expected: "a🙂" },
    { input: "x", maxBytes: -1, expected: "" },
    { input: "x", maxBytes: 1.2, expected: "" },
  ];

  for (const { name, mod } of MODULES) {
    const truncate = impl(name, mod, "truncateUtf8");
    for (const { input, maxBytes, expected } of cases) {
      assert.equal(truncate(input, maxBytes), expected, `${name}.truncateUtf8 mismatch`);
    }
  }
});

test("text helpers: formatOneLineUtf8 composes sanitize+truncate", () => {
  const cases = [
    { input: "a\tb\nc", maxBytes: 512, expected: "a b c" },
    { input: "a\u00a0b", maxBytes: 512, expected: "a b" },
    { input: "🙂", maxBytes: 3, expected: "" },
  ];

  for (const { name, mod } of MODULES) {
    const format = impl(name, mod, "formatOneLineUtf8");
    for (const { input, maxBytes, expected } of cases) {
      assert.equal(format(input, maxBytes), expected, `${name}.formatOneLineUtf8 mismatch`);
    }
  }
});

