// AeroGPU UMD-private discovery blob (UMDRIVERPRIVATE).
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_umd_private.h`.

export const AEROGPU_UMDPRIV_STRUCT_VERSION_V1 = 1;

export const AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP = 0x41524750; // "ARGP" LE
export const AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU = 0x55504741; // "AGPU" LE

export const AEROGPU_UMDPRIV_MMIO_REG_MAGIC = 0x0000;
export const AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION = 0x0004;
export const AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO = 0x0008;
export const AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI = 0x000c;

export const AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE = 1n << 0n;
export const AEROGPU_UMDPRIV_FEATURE_CURSOR = 1n << 1n;
export const AEROGPU_UMDPRIV_FEATURE_SCANOUT = 1n << 2n;
export const AEROGPU_UMDPRIV_FEATURE_VBLANK = 1n << 3n;
export const AEROGPU_UMDPRIV_FEATURE_TRANSFER = 1n << 4n;

export const AEROGPU_UMDPRIV_FLAG_IS_LEGACY = 1 << 0;
export const AEROGPU_UMDPRIV_FLAG_HAS_VBLANK = 1 << 1;
export const AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE = 1 << 2;

export const AEROGPU_UMD_PRIVATE_V1_SIZE = 64;
export const AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES = 0;
export const AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION = 4;
export const AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC = 8;
export const AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32 = 12;
export const AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES = 20;
export const AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS = 28;

export class AerogpuUmdPrivateError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AerogpuUmdPrivateError";
  }
}

export interface AerogpuUmdPrivateV1 {
  sizeBytes: number;
  structVersion: number;
  deviceMmioMagic: number;
  deviceAbiVersionU32: number;
  deviceFeatures: bigint;
  flags: number;
}

export function decodeUmdPrivateV1(view: DataView, byteOffset = 0): AerogpuUmdPrivateV1 {
  if (view.byteLength < byteOffset + AEROGPU_UMD_PRIVATE_V1_SIZE) {
    throw new AerogpuUmdPrivateError("Buffer too small for aerogpu_umd_private_v1");
  }

  return {
    sizeBytes: view.getUint32(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_SIZE_BYTES, true),
    structVersion: view.getUint32(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_STRUCT_VERSION, true),
    deviceMmioMagic: view.getUint32(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_MMIO_MAGIC, true),
    deviceAbiVersionU32: view.getUint32(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_ABI_VERSION_U32, true),
    deviceFeatures: view.getBigUint64(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_DEVICE_FEATURES, true),
    flags: view.getUint32(byteOffset + AEROGPU_UMD_PRIVATE_V1_OFF_FLAGS, true),
  };
}
