import assert from "node:assert/strict";
import test from "node:test";

import { isValidHttpToken, isValidHttpTokenPart, isTchar } from "../httpTokens";

test("httpTokens: isTchar matches RFC7230 tchar set (spot checks)", () => {
  assert.equal(isTchar("a".charCodeAt(0)), true);
  assert.equal(isTchar("Z".charCodeAt(0)), true);
  assert.equal(isTchar("0".charCodeAt(0)), true);
  assert.equal(isTchar("!".charCodeAt(0)), true);
  assert.equal(isTchar("~".charCodeAt(0)), true);

  assert.equal(isTchar(" ".charCodeAt(0)), false);
  assert.equal(isTchar(",".charCodeAt(0)), false);
  assert.equal(isTchar(";".charCodeAt(0)), false);
  assert.equal(isTchar("=".charCodeAt(0)), false);
});

test("httpTokens: isValidHttpToken / isValidHttpTokenPart validate tokens", () => {
  assert.equal(isValidHttpToken(""), false);
  assert.equal(isValidHttpToken("a"), true);
  assert.equal(isValidHttpToken("a-b.c_d~"), true);
  assert.equal(isValidHttpToken("a b"), false);
  assert.equal(isValidHttpToken("a,b"), false);
  assert.equal(isValidHttpToken("é"), false);

  assert.equal(isValidHttpTokenPart("abc", 0, 0), false);
  assert.equal(isValidHttpTokenPart("abc", 1, 1), false);
  assert.equal(isValidHttpTokenPart("abc", 0, 3), true);
  assert.equal(isValidHttpTokenPart("a b", 0, 3), false);
});

