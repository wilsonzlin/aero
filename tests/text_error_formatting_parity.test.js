import assert from "node:assert/strict";
import test from "node:test";

const MODULES = [
  { name: "backend/aero-gateway", mod: await import(new URL("../backend/aero-gateway/src/util/text.ts", import.meta.url)) },
  { name: "server", mod: await import(new URL("../server/src/text.js", import.meta.url)) },
  { name: "tools/net-proxy-server", mod: await import(new URL("../tools/net-proxy-server/src/text.js", import.meta.url)) },
  { name: "src", mod: await import(new URL("../src/text.js", import.meta.url)) },
  { name: "web", mod: await import(new URL("../web/src/text.ts", import.meta.url)) },
];

function impl(name, mod, key) {
  const fn = mod[key];
  assert.equal(typeof fn, "function", `${name} missing ${key}()`);
  return fn;
}

test("text helpers: formatOneLineError is consistent", () => {
  const src = MODULES.find((m) => m.name === "src");
  assert.ok(src);
  const formatSrc = impl(src.name, src.mod, "formatOneLineError");

  const throwingMessage = Object.create(null, {
    message: {
      enumerable: true,
      get() {
        throw new Error("boom");
      },
    },
  });

  const cases = [
    { err: new Error("a\tb\nc"), maxBytes: 512 },
    { err: { message: "x\ny" }, maxBytes: 512 },
    { err: throwingMessage, maxBytes: 512 },
    { err: {}, maxBytes: 512 },
    { err: () => {}, maxBytes: 512 },
    { err: 123, maxBytes: 512 },
    { err: null, maxBytes: 512 },
    { err: "x".repeat(600), maxBytes: 512 },
    { err: "🙂", maxBytes: 3 },
  ];

  for (const { err, maxBytes } of cases) {
    const expected = formatSrc(err, maxBytes);
    for (const { name, mod } of MODULES) {
      const format = impl(name, mod, "formatOneLineError");
      assert.equal(format(err, maxBytes), expected, `${name}.formatOneLineError mismatch`);
    }
  }
});

