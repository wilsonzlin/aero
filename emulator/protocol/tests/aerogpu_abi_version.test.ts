import assert from "node:assert/strict";
import test from "node:test";

import { AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, parseAndValidateAbiVersionU32 } from "../aerogpu/aerogpu_pci.ts";

test("parseAndValidateAbiVersionU32 rejects unsupported major versions", () => {
  const unsupportedMajor = AEROGPU_ABI_MAJOR + 1;
  const versionU32 = (unsupportedMajor << 16) | AEROGPU_ABI_MINOR;
  assert.throws(() => parseAndValidateAbiVersionU32(versionU32), /Unsupported major/i);
});

test("parseAndValidateAbiVersionU32 accepts unknown minor versions (forward-compatible)", () => {
  const forwardCompatMinor = AEROGPU_ABI_MINOR + 1;
  const versionU32 = (AEROGPU_ABI_MAJOR << 16) | forwardCompatMinor;

  assert.doesNotThrow(() => {
    const parsed = parseAndValidateAbiVersionU32(versionU32);
    assert.equal(parsed.major, AEROGPU_ABI_MAJOR);
    assert.equal(parsed.minor, forwardCompatMinor);
  });
});

