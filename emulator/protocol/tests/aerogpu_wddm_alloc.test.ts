import assert from "node:assert/strict";
import test from "node:test";

import {
  AerogpuWddmAllocKind,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED,
  AEROGPU_WDDM_ALLOC_PRIV_MAGIC,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES,
  AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION,
  AEROGPU_WDDM_ALLOC_PRIV_SIZE,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1,
  AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH,
  AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE,
  AEROGPU_WDDM_ALLOC_PRIV_VERSION,
  AEROGPU_WDDM_ALLOC_PRIV_VERSION_2,
  decodeWddmAllocPriv,
  decodeWddmAllocPrivV1,
  decodeWddmAllocPrivV2,
} from "../aerogpu/aerogpu_wddm_alloc.ts";

test("decodeWddmAllocPrivV1 decodes the expected byte layout", () => {
  const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_MAGIC, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, AEROGPU_WDDM_ALLOC_PRIV_VERSION, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID, 0x11223344, true);
  view.setUint32(
    AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS,
    AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED | AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
    true,
  );
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN, 0x0102030405060708n, true);
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES, 0x1111222233334444n, true);
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0, 0x5555666677778888n, true);

  const decoded = decodeWddmAllocPrivV1(view);
  assert.equal(decoded.magic, AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
  assert.equal(decoded.version, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
  assert.equal(decoded.allocId, 0x11223344);
  assert.equal(
    decoded.flags,
    AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED | AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
  );
  assert.equal(decoded.shareToken, 0x0102030405060708n);
  assert.equal(decoded.sizeBytes, 0x1111222233334444n);
  assert.equal(decoded.reserved0, 0x5555666677778888n);
});

test("decodeWddmAllocPrivV2 decodes the expected byte layout", () => {
  const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_MAGIC, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID, 0x99aabbcc, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS, AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED, true);
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN, 0x1020304050607080n, true);
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES, 0x1000n, true);
  view.setBigUint64(AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0, 0n, true);

  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND, AerogpuWddmAllocKind.Texture2d, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH, 1920, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT, 1080, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT, 87, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES, 1920 * 4, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1, 0, true);

  const decoded = decodeWddmAllocPrivV2(view);
  assert.equal(decoded.version, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
  assert.equal(decoded.kind, AerogpuWddmAllocKind.Texture2d);
  assert.equal(decoded.width, 1920);
  assert.equal(decoded.height, 1080);
  assert.equal(decoded.format, 87);
  assert.equal(decoded.rowPitchBytes, 1920 * 4);
  assert.equal(decoded.reserved1, 0);
});

test("decodeWddmAllocPriv selects the correct struct version", () => {
  // V1.
  {
    const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_SIZE);
    const view = new DataView(buf);
    view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_MAGIC, true);
    view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, AEROGPU_WDDM_ALLOC_PRIV_VERSION, true);
    const decoded = decodeWddmAllocPriv(view);
    assert.equal(decoded.version, AEROGPU_WDDM_ALLOC_PRIV_VERSION);
  }

  // V2.
  {
    const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE);
    const view = new DataView(buf);
    view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_MAGIC, true);
    view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2, true);
    const decoded = decodeWddmAllocPriv(view);
    assert.equal(decoded.version, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
    assert.equal((decoded as { kind: number }).kind, AerogpuWddmAllocKind.Unknown);
  }
});

test("decodeWddmAllocPriv rejects unknown versions", () => {
  const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_SIZE);
  const view = new DataView(buf);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, AEROGPU_WDDM_ALLOC_PRIV_MAGIC, true);
  view.setUint32(AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, 999, true);
  assert.throws(() => decodeWddmAllocPriv(view), /Unknown aerogpu_wddm_alloc_priv version: 999/);
});

test("decodeWddmAllocPrivV1 rejects too-small buffers", () => {
  const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_SIZE - 1);
  const view = new DataView(buf);
  assert.throws(() => decodeWddmAllocPrivV1(view), /Buffer too small/);
});

test("decodeWddmAllocPrivV2 rejects too-small buffers", () => {
  const buf = new ArrayBuffer(AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE - 1);
  const view = new DataView(buf);
  assert.throws(() => decodeWddmAllocPrivV2(view), /Buffer too small/);
});

