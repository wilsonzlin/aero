import test from "node:test";
import assert from "node:assert/strict";

import {
  commaSeparatedTokenListHasToken,
  contentEncodingIsIdentity,
} from "../web/src/storage/http_headers";

test("commaSeparatedTokenListHasToken: matches no-transform among Cache-Control directives", () => {
  assert.equal(commaSeparatedTokenListHasToken("no-transform", "no-transform", { maxLen: 128 }), true);
  assert.equal(commaSeparatedTokenListHasToken("public, no-transform, max-age=60", "no-transform", { maxLen: 128 }), true);
  assert.equal(commaSeparatedTokenListHasToken("public,max-age=60", "no-transform", { maxLen: 128 }), false);
  assert.equal(commaSeparatedTokenListHasToken("public, NO-TRANSFORM", "no-transform", { maxLen: 128 }), true);
});

test("commaSeparatedTokenListHasToken: rejects oversized header values", () => {
  const huge = "no-transform," + "a".repeat(1024);
  assert.equal(commaSeparatedTokenListHasToken(huge, "no-transform", { maxLen: 32 }), false);
});

test("contentEncodingIsIdentity: accepts empty/identity and rejects others (bounded)", () => {
  assert.equal(contentEncodingIsIdentity("", { maxLen: 16 }), true);
  assert.equal(contentEncodingIsIdentity("  ", { maxLen: 16 }), true);
  assert.equal(contentEncodingIsIdentity("identity", { maxLen: 16 }), true);
  assert.equal(contentEncodingIsIdentity(" Identity ", { maxLen: 16 }), true);
  assert.equal(contentEncodingIsIdentity("gzip", { maxLen: 16 }), false);
  assert.equal(contentEncodingIsIdentity("identity, gzip", { maxLen: 64 }), false);
  assert.equal(contentEncodingIsIdentity("a".repeat(17), { maxLen: 16 }), false);
});

