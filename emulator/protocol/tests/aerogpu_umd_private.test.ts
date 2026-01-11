import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
  AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES,
  AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC,
  AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS,
  AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES,
  AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION,
  AEROGPU_UMD_PRIVATE_V1_SIZE,
  decodeUmdPrivateV1,
} from "../aerogpu/aerogpu_umd_private.ts";

test("decodeUmdPrivateV1 accepts extended size_bytes", () => {
  const buf = new ArrayBuffer(AEROGPU_UMD_PRIVATE_V1_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES, AEROGPU_UMD_PRIVATE_V1_SIZE + 16, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION, AEROGPU_UMDPRIV_STRUCT_VERSION_V1, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC, AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32, 0x12345678, true);
  view.setBigUint64(AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES, 0x11n, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS, 0x22, true);

  const decoded = decodeUmdPrivateV1(view);
  assert.equal(decoded.sizeBytes, AEROGPU_UMD_PRIVATE_V1_SIZE + 16);
  assert.equal(decoded.structVersion, AEROGPU_UMDPRIV_STRUCT_VERSION_V1);
  assert.equal(decoded.deviceMmioMagic, AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU);
  assert.equal(decoded.deviceAbiVersionU32, 0x12345678);
  assert.equal(decoded.deviceFeatures, 0x11n);
  assert.equal(decoded.flags, 0x22);
});

test("decodeUmdPrivateV1 rejects too-small size_bytes", () => {
  const buf = new ArrayBuffer(AEROGPU_UMD_PRIVATE_V1_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES, AEROGPU_UMD_PRIVATE_V1_SIZE - 1, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION, AEROGPU_UMDPRIV_STRUCT_VERSION_V1, true);

  assert.throws(() => decodeUmdPrivateV1(view), /size_bytes too small/);
});

test("decodeUmdPrivateV1 rejects unsupported struct_version", () => {
  const buf = new ArrayBuffer(AEROGPU_UMD_PRIVATE_V1_SIZE);
  const view = new DataView(buf);

  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES, AEROGPU_UMD_PRIVATE_V1_SIZE, true);
  view.setUint32(AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION, 999, true);

  assert.throws(() => decodeUmdPrivateV1(view), /struct_version unsupported/);
});

