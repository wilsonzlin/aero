import assert from "node:assert/strict";
import test from "node:test";

const gateway = await import(new URL("../backend/aero-gateway/src/httpTokens.ts", import.meta.url));
const tools = await import(new URL("../tools/net-proxy-server/src/httpTokens.js", import.meta.url));
const root = await import(new URL("../src/httpTokens.js", import.meta.url));

const MODULES = [
  { name: "backend/aero-gateway", mod: gateway },
  { name: "tools/net-proxy-server", mod: tools },
  { name: "src", mod: root },
];

function impl(name, mod, key) {
  const fn = mod[key];
  assert.equal(typeof fn, "function", `${name} missing ${key}()`);
  return fn;
}

test("httpTokens: isTchar matches across implementations", () => {
  const expectedByCode = new Map();
  for (const { name, mod } of MODULES) {
    const isTchar = impl(name, mod, "isTchar");
    for (let code = 0; code <= 0xff; code += 1) {
      const key = code;
      const v = isTchar(code);
      if (!expectedByCode.has(key)) expectedByCode.set(key, v);
      assert.equal(v, expectedByCode.get(key), `${name}.isTchar mismatch for 0x${code.toString(16)}`);
    }
  }
});

test("httpTokens: token validation matches across implementations", () => {
  const cases = [
    { token: "", expected: false },
    { token: "a", expected: true },
    { token: "A", expected: true },
    { token: "0", expected: true },
    { token: "-", expected: true },
    { token: ".", expected: true },
    { token: "_", expected: true },
    { token: "~", expected: true },
    { token: "a-b.c_d~", expected: true },
    { token: "a b", expected: false },
    { token: "a\tb", expected: false },
    { token: "a\nb", expected: false },
    { token: "a,b", expected: false },
    { token: "a;b", expected: false },
    { token: "a=b", expected: false },
    { token: "a/b", expected: false },
    { token: "a\\b", expected: false },
    { token: "\u00e9", expected: false }, // non-ASCII
  ];

  for (const { name, mod } of MODULES) {
    const isValidHttpToken = impl(name, mod, "isValidHttpToken");
    const isValidHttpTokenPart = impl(name, mod, "isValidHttpTokenPart");

    for (const { token, expected } of cases) {
      assert.equal(isValidHttpToken(token), expected, `${name}.isValidHttpToken mismatch for ${JSON.stringify(token)}`);
      assert.equal(
        isValidHttpTokenPart(token, 0, token.length),
        expected && token.length > 0,
        `${name}.isValidHttpTokenPart mismatch for ${JSON.stringify(token)}`
      );
    }

    assert.equal(isValidHttpTokenPart("abc", 0, 0), false, `${name}.isValidHttpTokenPart empty range must be false`);
    assert.equal(isValidHttpTokenPart("abc", 1, 1), false, `${name}.isValidHttpTokenPart empty range must be false`);
  }
});

