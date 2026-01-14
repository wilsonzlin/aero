import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER,
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT,
  AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH,
  packWddmAllocPrivDesc,
  wddmAllocPrivDescFormat,
  wddmAllocPrivDescHeight,
  wddmAllocPrivDescPresent,
  wddmAllocPrivDescWidth,
} from "../aerogpu/aerogpu_wddm_alloc.ts";

test("WDDM alloc priv desc pack/unpack roundtrips and matches bit layout", () => {
  const formatU32 = 0x11223344;
  const widthU32 = 640;
  const heightU32 = 480;

  const expected =
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER |
    (BigInt(formatU32) & 0xffff_ffffn) |
    ((BigInt(widthU32) & 0xffffn) << 32n) |
    ((BigInt(heightU32) & 0x7fffn) << 48n);

  const packed = packWddmAllocPrivDesc(formatU32, widthU32, heightU32);
  assert.equal(packed, expected, "packed descriptor bits");

  assert.equal(wddmAllocPrivDescPresent(packed), true);
  assert.equal(wddmAllocPrivDescFormat(packed), formatU32);
  assert.equal(wddmAllocPrivDescWidth(packed), widthU32);
  assert.equal(wddmAllocPrivDescHeight(packed), heightU32);
});

test("WDDM alloc priv desc present bit is controlled by the marker (bit63)", () => {
  assert.equal(wddmAllocPrivDescPresent(0n), false);
  assert.equal(wddmAllocPrivDescPresent(1n << 62n), false);
  assert.equal(wddmAllocPrivDescPresent(AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER), true);
});

test("WDDM alloc priv desc pack masks width/height to fixed bit ranges", () => {
  const formatU32 = 0xaabbccdd;
  const widthU32 = 0x1_2345; // wider than 16 bits
  const heightU32 = 0xffff; // wider than 15 bits

  const packed = packWddmAllocPrivDesc(formatU32, widthU32, heightU32);
  assert.equal(wddmAllocPrivDescPresent(packed), true);
  assert.equal(wddmAllocPrivDescFormat(packed), formatU32);
  assert.equal(wddmAllocPrivDescWidth(packed), 0x2345);
  assert.equal(wddmAllocPrivDescHeight(packed), 0x7fff);
});

test("WDDM alloc priv desc max width/height constants match the mask ranges", () => {
  assert.equal(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH, 0xffff);
  assert.equal(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT, 0x7fff);
});

