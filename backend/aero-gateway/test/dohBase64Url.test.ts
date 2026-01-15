import assert from "node:assert/strict";
import test from "node:test";

import { decodeBase64UrlToBuffer } from "../src/base64url.js";

test("decodeBase64UrlToBuffer decodes base64url without padding", () => {
  assert.equal(decodeBase64UrlToBuffer("aGk").toString("utf8"), "hi");
});

test("decodeBase64UrlToBuffer rejects invalid characters", () => {
  assert.throws(() => decodeBase64UrlToBuffer("!!"), /invalid base64url/i);
});

test("decodeBase64UrlToBuffer rejects invalid length (mod 4 == 1)", () => {
  assert.throws(() => decodeBase64UrlToBuffer("a"), /length/i);
});

