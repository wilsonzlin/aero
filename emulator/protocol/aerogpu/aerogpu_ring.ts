// AeroGPU ring + submission + fence-page layouts.
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_ring.h`.

import { parseAndValidateAbiVersionU32 } from "./aerogpu_pci.ts";

export const AEROGPU_ALLOC_TABLE_MAGIC = 0x434f4c41; // "ALOC" LE
export const AEROGPU_RING_MAGIC = 0x474e5241; // "ARNG" LE
export const AEROGPU_FENCE_PAGE_MAGIC = 0x434e4546; // "FENC" LE

export const AEROGPU_SUBMIT_FLAG_PRESENT = 1 << 0;
export const AEROGPU_SUBMIT_FLAG_NO_IRQ = 1 << 1;

export const AEROGPU_ENGINE_0 = 0;

export const AEROGPU_ALLOC_FLAG_READONLY = 1 << 0;

export class AerogpuRingError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AerogpuRingError";
  }
}

/* ---------------------------- Allocation table ----------------------------- */

export const AEROGPU_ALLOC_TABLE_HEADER_SIZE = 24;
export const AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC = 0;
export const AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION = 4;
export const AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES = 8;
export const AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT = 12;
export const AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES = 16;

export interface AerogpuAllocTableHeader {
  abiVersion: number;
  sizeBytes: number;
  entryCount: number;
  entryStrideBytes: number;
}

export function decodeAllocTableHeader(
  view: DataView,
  byteOffset = 0,
  maxSizeBytes?: number,
): AerogpuAllocTableHeader {
  if (view.byteLength < byteOffset + AEROGPU_ALLOC_TABLE_HEADER_SIZE) {
    throw new AerogpuRingError("Buffer too small for aerogpu_alloc_table_header");
  }

  const magic = view.getUint32(byteOffset + AEROGPU_ALLOC_TABLE_HEADER_OFF_MAGIC, true);
  if (magic !== AEROGPU_ALLOC_TABLE_MAGIC) {
    throw new AerogpuRingError(`Bad alloc table magic: 0x${magic.toString(16)}`);
  }

  const abiVersion = view.getUint32(byteOffset + AEROGPU_ALLOC_TABLE_HEADER_OFF_ABI_VERSION, true);
  parseAndValidateAbiVersionU32(abiVersion);

  const sizeBytes = view.getUint32(byteOffset + AEROGPU_ALLOC_TABLE_HEADER_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_ALLOC_TABLE_HEADER_SIZE) {
    throw new AerogpuRingError(`alloc_table.size_bytes too small: ${sizeBytes}`);
  }
  if (maxSizeBytes !== undefined && sizeBytes > maxSizeBytes) {
    throw new AerogpuRingError(
      `alloc_table.size_bytes exceeds max size (${sizeBytes} > ${maxSizeBytes})`,
    );
  }

  const entryCount = view.getUint32(byteOffset + AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_COUNT, true);
  const entryStrideBytes = view.getUint32(
    byteOffset + AEROGPU_ALLOC_TABLE_HEADER_OFF_ENTRY_STRIDE_BYTES,
    true,
  );
  if (entryStrideBytes < AEROGPU_ALLOC_ENTRY_SIZE) {
    throw new AerogpuRingError(`alloc_table.entry_stride_bytes too small: ${entryStrideBytes}`);
  }

  const requiredBytes =
    BigInt(AEROGPU_ALLOC_TABLE_HEADER_SIZE) + BigInt(entryCount) * BigInt(entryStrideBytes);
  if (requiredBytes > BigInt(sizeBytes)) {
    throw new AerogpuRingError(`alloc_table.size_bytes too small for layout: ${sizeBytes}`);
  }

  return {
    abiVersion,
    sizeBytes,
    entryCount,
    entryStrideBytes,
  };
}

export const AEROGPU_ALLOC_ENTRY_SIZE = 32;
export const AEROGPU_ALLOC_ENTRY_OFF_ALLOC_ID = 0;
export const AEROGPU_ALLOC_ENTRY_OFF_FLAGS = 4;
export const AEROGPU_ALLOC_ENTRY_OFF_GPA = 8;
export const AEROGPU_ALLOC_ENTRY_OFF_SIZE_BYTES = 16;
export const AEROGPU_ALLOC_ENTRY_OFF_RESERVED0 = 24;

export interface AerogpuAllocEntry {
  allocId: number;
  flags: number;
  gpa: bigint;
  sizeBytes: bigint;
  reserved0: bigint;
}

export function decodeAllocTable(
  view: DataView,
  byteOffset = 0,
): { header: AerogpuAllocTableHeader; entries: AerogpuAllocEntry[] } {
  const header = decodeAllocTableHeader(view, byteOffset);
  if (view.byteLength < byteOffset + header.sizeBytes) {
    throw new AerogpuRingError(
      `Buffer too small for aerogpu_alloc_table: need ${header.sizeBytes} bytes, have ${view.byteLength - byteOffset}`,
    );
  }
  if (header.entryStrideBytes !== AEROGPU_ALLOC_ENTRY_SIZE) {
    throw new AerogpuRingError(`alloc_table.entry_stride_bytes mismatch: ${header.entryStrideBytes}`);
  }

  const entries: AerogpuAllocEntry[] = [];
  const entriesStart = byteOffset + AEROGPU_ALLOC_TABLE_HEADER_SIZE;
  for (let i = 0; i < header.entryCount; i++) {
    const base = entriesStart + i * header.entryStrideBytes;
    entries.push({
      allocId: view.getUint32(base + AEROGPU_ALLOC_ENTRY_OFF_ALLOC_ID, true),
      flags: view.getUint32(base + AEROGPU_ALLOC_ENTRY_OFF_FLAGS, true),
      gpa: view.getBigUint64(base + AEROGPU_ALLOC_ENTRY_OFF_GPA, true),
      sizeBytes: view.getBigUint64(base + AEROGPU_ALLOC_ENTRY_OFF_SIZE_BYTES, true),
      reserved0: view.getBigUint64(base + AEROGPU_ALLOC_ENTRY_OFF_RESERVED0, true),
    });
  }

  return { header, entries };
}

/* ------------------------------- Ring header ------------------------------ */

export const AEROGPU_RING_HEADER_SIZE = 64;
export const AEROGPU_RING_HEADER_OFF_MAGIC = 0;
export const AEROGPU_RING_HEADER_OFF_ABI_VERSION = 4;
export const AEROGPU_RING_HEADER_OFF_SIZE_BYTES = 8;
export const AEROGPU_RING_HEADER_OFF_ENTRY_COUNT = 12;
export const AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES = 16;
export const AEROGPU_RING_HEADER_OFF_FLAGS = 20;
export const AEROGPU_RING_HEADER_OFF_HEAD = 24;
export const AEROGPU_RING_HEADER_OFF_TAIL = 28;

export interface AerogpuRingHeader {
  abiVersion: number;
  sizeBytes: number;
  entryCount: number;
  entryStrideBytes: number;
  flags: number;
  head: number;
  tail: number;
}

export function decodeRingHeader(view: DataView, byteOffset = 0): AerogpuRingHeader {
  if (view.byteLength < byteOffset + AEROGPU_RING_HEADER_SIZE) {
    throw new AerogpuRingError("Buffer too small for aerogpu_ring_header");
  }

  const magic = view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_MAGIC, true);
  if (magic !== AEROGPU_RING_MAGIC) {
    throw new AerogpuRingError(`Bad ring magic: 0x${magic.toString(16)}`);
  }

  const abiVersion = view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_ABI_VERSION, true);
  parseAndValidateAbiVersionU32(abiVersion);

  const sizeBytes = view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_RING_HEADER_SIZE) {
    throw new AerogpuRingError(`ring.size_bytes too small: ${sizeBytes}`);
  }

  const entryCount = view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_ENTRY_COUNT, true);
  if (entryCount === 0 || (entryCount & (entryCount - 1)) !== 0) {
    throw new AerogpuRingError(`ring.entry_count must be a power-of-two: ${entryCount}`);
  }

  const entryStrideBytes = view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_ENTRY_STRIDE_BYTES, true);
  if (entryStrideBytes < AEROGPU_SUBMIT_DESC_SIZE) {
    throw new AerogpuRingError(`ring.entry_stride_bytes too small: ${entryStrideBytes}`);
  }

  const requiredBytes =
    BigInt(AEROGPU_RING_HEADER_SIZE) + BigInt(entryCount) * BigInt(entryStrideBytes);
  if (requiredBytes > BigInt(sizeBytes)) {
    throw new AerogpuRingError(`ring.size_bytes too small for layout: ${sizeBytes}`);
  }

  return {
    abiVersion,
    sizeBytes,
    entryCount,
    entryStrideBytes,
    flags: view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_FLAGS, true),
    head: view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_HEAD, true),
    tail: view.getUint32(byteOffset + AEROGPU_RING_HEADER_OFF_TAIL, true),
  };
}

/* --------------------------- Submission descriptor ------------------------ */

export const AEROGPU_SUBMIT_DESC_SIZE = 64;
export const AEROGPU_SUBMIT_DESC_OFF_DESC_SIZE_BYTES = 0;
export const AEROGPU_SUBMIT_DESC_OFF_FLAGS = 4;
export const AEROGPU_SUBMIT_DESC_OFF_CONTEXT_ID = 8;
export const AEROGPU_SUBMIT_DESC_OFF_ENGINE_ID = 12;
export const AEROGPU_SUBMIT_DESC_OFF_CMD_GPA = 16;
export const AEROGPU_SUBMIT_DESC_OFF_CMD_SIZE_BYTES = 24;
export const AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_GPA = 32;
export const AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_SIZE_BYTES = 40;
export const AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE = 48;

export interface AerogpuSubmitDesc {
  descSizeBytes: number;
  flags: number;
  contextId: number;
  engineId: number;
  cmdGpa: bigint;
  cmdSizeBytes: number;
  allocTableGpa: bigint;
  allocTableSizeBytes: number;
  signalFence: bigint;
}

export function decodeSubmitDesc(view: DataView, byteOffset = 0, maxSizeBytes?: number): AerogpuSubmitDesc {
  if (view.byteLength < byteOffset + AEROGPU_SUBMIT_DESC_SIZE) {
    throw new AerogpuRingError("Buffer too small for aerogpu_submit_desc");
  }

  const descSizeBytes = view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_DESC_SIZE_BYTES, true);
  if (descSizeBytes < AEROGPU_SUBMIT_DESC_SIZE) {
    throw new AerogpuRingError(`submit.desc_size_bytes too small: ${descSizeBytes}`);
  }
  if (maxSizeBytes !== undefined && descSizeBytes > maxSizeBytes) {
    throw new AerogpuRingError(
      `submit.desc_size_bytes exceeds max size (${descSizeBytes} > ${maxSizeBytes})`,
    );
  }

  return {
    descSizeBytes,
    flags: view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_FLAGS, true),
    contextId: view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_CONTEXT_ID, true),
    engineId: view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_ENGINE_ID, true),
    cmdGpa: view.getBigUint64(byteOffset + AEROGPU_SUBMIT_DESC_OFF_CMD_GPA, true),
    cmdSizeBytes: view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_CMD_SIZE_BYTES, true),
    allocTableGpa: view.getBigUint64(byteOffset + AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_GPA, true),
    allocTableSizeBytes: view.getUint32(byteOffset + AEROGPU_SUBMIT_DESC_OFF_ALLOC_TABLE_SIZE_BYTES, true),
    signalFence: view.getBigUint64(byteOffset + AEROGPU_SUBMIT_DESC_OFF_SIGNAL_FENCE, true),
  };
}

/* -------------------------------- Fence page ------------------------------ */

export const AEROGPU_FENCE_PAGE_SIZE = 56;
export const AEROGPU_FENCE_PAGE_OFF_MAGIC = 0;
export const AEROGPU_FENCE_PAGE_OFF_ABI_VERSION = 4;
export const AEROGPU_FENCE_PAGE_OFF_COMPLETED_FENCE = 8;

export function validateFencePageHeader(view: DataView, byteOffset = 0): void {
  if (view.byteLength < byteOffset + AEROGPU_FENCE_PAGE_SIZE) {
    throw new AerogpuRingError("Buffer too small for aerogpu_fence_page");
  }

  const magic = view.getUint32(byteOffset + AEROGPU_FENCE_PAGE_OFF_MAGIC, true);
  if (magic !== AEROGPU_FENCE_PAGE_MAGIC) {
    throw new AerogpuRingError(`Bad fence page magic: 0x${magic.toString(16)}`);
  }

  const abiVersion = view.getUint32(byteOffset + AEROGPU_FENCE_PAGE_OFF_ABI_VERSION, true);
  parseAndValidateAbiVersionU32(abiVersion);
}

export function writeFencePageCompletedFence(
  view: DataView,
  byteOffset: number,
  completedFence: bigint,
): void {
  if (view.byteLength < byteOffset + AEROGPU_FENCE_PAGE_SIZE) {
    throw new AerogpuRingError("Buffer too small for aerogpu_fence_page");
  }

  view.setBigUint64(byteOffset + AEROGPU_FENCE_PAGE_OFF_COMPLETED_FENCE, completedFence, true);
}
