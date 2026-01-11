// AeroGPU WDDM allocation private-driver-data contract (Win7 WDDM 1.1).
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`.

export const AEROGPU_WDDM_ALLOC_PRIV_MAGIC = 0x414c4c4f; // "ALLO" LE
export const AEROGPU_WDDM_ALLOC_PRIV_VERSION = 1;
export const AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 = 2;

// Backwards-compat aliases (older code used *_PRIVATE_DATA_* names).
export const AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
export const AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION = AEROGPU_WDDM_ALLOC_PRIV_VERSION;

export const AEROGPU_WDDM_ALLOC_ID_UMD_MAX = 0x7fffffff;
export const AEROGPU_WDDM_ALLOC_ID_KMD_MIN = 0x80000000;

export const AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE = 0;
export const AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED = 1 << 0;
export const AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE = 1 << 1;
export const AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING = 1 << 2;

// Backwards-compat alias for AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED.
export const AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;

export const AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER = 0x8000000000000000n;
export const AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH = 0xffff;
export const AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT = 0x7fff;

export function packWddmAllocPrivDesc(formatU32: number, widthU32: number, heightU32: number): bigint {
  return (
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER |
    (BigInt(formatU32) & 0xffff_ffffn) |
    ((BigInt(widthU32) & 0xffffn) << 32n) |
    ((BigInt(heightU32) & 0x7fffn) << 48n)
  );
}

export function wddmAllocPrivDescPresent(desc: bigint): boolean {
  return (desc & AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER) !== 0n;
}

export function wddmAllocPrivDescFormat(desc: bigint): number {
  return Number(desc & 0xffff_ffffn);
}

export function wddmAllocPrivDescWidth(desc: bigint): number {
  return Number((desc >> 32n) & 0xffffn);
}

export function wddmAllocPrivDescHeight(desc: bigint): number {
  return Number((desc >> 48n) & 0x7fffn);
}

export const AerogpuWddmAllocKind = {
  Unknown: 0,
  Buffer: 1,
  Texture2d: 2,
} as const;

export type AerogpuWddmAllocKind = (typeof AerogpuWddmAllocKind)[keyof typeof AerogpuWddmAllocKind];

export const AEROGPU_WDDM_ALLOC_PRIV_SIZE = 40;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC = 0;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION = 4;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID = 8;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS = 12;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN = 16;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES = 24;
export const AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0 = 32;

export interface AerogpuWddmAllocPrivV1 {
  magic: number;
  version: number;
  allocId: number;
  flags: number;
  shareToken: bigint;
  sizeBytes: bigint;
  reserved0: bigint;
}

export const AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE = 64;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_MAGIC = 0;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_VERSION = 4;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ALLOC_ID = 8;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FLAGS = 12;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SHARE_TOKEN = 16;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_SIZE_BYTES = 24;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED0 = 32;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND = 40;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH = 44;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT = 48;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT = 52;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES = 56;
export const AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1 = 60;

export interface AerogpuWddmAllocPrivV2 extends AerogpuWddmAllocPrivV1 {
  kind: number;
  width: number;
  height: number;
  format: number;
  rowPitchBytes: number;
  reserved1: number;
}

export class AerogpuWddmAllocPrivError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AerogpuWddmAllocPrivError";
  }
}

export function decodeWddmAllocPrivV1(view: DataView, byteOffset = 0): AerogpuWddmAllocPrivV1 {
  if (view.byteLength < byteOffset + AEROGPU_WDDM_ALLOC_PRIV_SIZE) {
    throw new AerogpuWddmAllocPrivError("Buffer too small for aerogpu_wddm_alloc_priv");
  }

  return {
    magic: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_MAGIC, true),
    version: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_VERSION, true),
    allocId: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_ALLOC_ID, true),
    flags: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_FLAGS, true),
    shareToken: view.getBigUint64(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_SHARE_TOKEN, true),
    sizeBytes: view.getBigUint64(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_SIZE_BYTES, true),
    reserved0: view.getBigUint64(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_OFF_RESERVED0, true),
  };
}

export function decodeWddmAllocPrivV2(view: DataView, byteOffset = 0): AerogpuWddmAllocPrivV2 {
  if (view.byteLength < byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_SIZE) {
    throw new AerogpuWddmAllocPrivError("Buffer too small for aerogpu_wddm_alloc_priv_v2");
  }

  const base = decodeWddmAllocPrivV1(view, byteOffset);
  return {
    ...base,
    kind: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_KIND, true),
    width: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_WIDTH, true),
    height: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_HEIGHT, true),
    format: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_FORMAT, true),
    rowPitchBytes: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_ROW_PITCH_BYTES, true),
    reserved1: view.getUint32(byteOffset + AEROGPU_WDDM_ALLOC_PRIV_V2_OFF_RESERVED1, true),
  };
}

export function decodeWddmAllocPriv(view: DataView, byteOffset = 0): AerogpuWddmAllocPrivV1 | AerogpuWddmAllocPrivV2 {
  const header = decodeWddmAllocPrivV1(view, byteOffset);
  switch (header.version) {
    case AEROGPU_WDDM_ALLOC_PRIV_VERSION:
      return header;
    case AEROGPU_WDDM_ALLOC_PRIV_VERSION_2:
      return decodeWddmAllocPrivV2(view, byteOffset);
    default:
      throw new AerogpuWddmAllocPrivError(`Unknown aerogpu_wddm_alloc_priv version: ${header.version}`);
  }
}
