import assert from "node:assert/strict";
import test from "node:test";

import { AerogpuFormat, aerogpuFormatName, aerogpuFormatToString } from "../aerogpu/aerogpu_pci.ts";

test("aerogpuFormatName returns enum variant names for known formats", () => {
  assert.equal(aerogpuFormatName(AerogpuFormat.B8G8R8X8Unorm), "B8G8R8X8Unorm");
  assert.equal(aerogpuFormatName(AerogpuFormat.R8G8B8A8UnormSrgb), "R8G8B8A8UnormSrgb");
});

test("aerogpuFormatName returns null for unknown/invalid values", () => {
  assert.equal(aerogpuFormatName(0xffff_ffff), null);
  assert.equal(aerogpuFormatName(1.5), null);
  assert.equal(aerogpuFormatName(0x1_0000_0000), null);
  assert.equal(aerogpuFormatName(Number.NaN), null);
});

test("aerogpuFormatToString formats known formats as \"Name (u32)\"", () => {
  assert.equal(aerogpuFormatToString(AerogpuFormat.B8G8R8X8Unorm), "B8G8R8X8Unorm (2)");
  assert.equal(aerogpuFormatToString(AerogpuFormat.D32Float), "D32Float (33)");
});

test("aerogpuFormatToString formats unknown formats as the raw u32 value", () => {
  assert.equal(aerogpuFormatToString(1234), "1234");
});

test("aerogpuFormatToString returns \"n/a\" for non-finite values", () => {
  assert.equal(aerogpuFormatToString(Number.NaN), "n/a");
  assert.equal(aerogpuFormatToString(Number.POSITIVE_INFINITY), "n/a");
});

test("aerogpuFormatToString preserves non-integer / out-of-range inputs as raw strings", () => {
  assert.equal(aerogpuFormatToString(1.5), "1.5");
  assert.equal(aerogpuFormatToString(0x1_0000_0000), String(0x1_0000_0000));
});
