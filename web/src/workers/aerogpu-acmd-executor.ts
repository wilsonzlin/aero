// AeroGPU ACMD executor used by the browser GPU worker.
//
// This module is intentionally kept free of Vite-only imports (e.g. `?raw`) so it
// can be imported from Node-based protocol tests.

import {
  AEROGPU_CMD_COPY_BUFFER_SIZE,
  AEROGPU_CMD_COPY_TEXTURE2D_SIZE,
  AEROGPU_CMD_CREATE_BUFFER_SIZE,
  AEROGPU_CMD_CREATE_SAMPLER_SIZE,
  AEROGPU_CMD_CREATE_TEXTURE2D_SIZE,
  AEROGPU_CMD_DESTROY_RESOURCE_SIZE,
  AEROGPU_CMD_DESTROY_SAMPLER_SIZE,
  AEROGPU_CMD_DISPATCH_SIZE,
  AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_FLUSH_SIZE,
  AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_PRESENT_EX_SIZE,
  AEROGPU_CMD_PRESENT_SIZE,
  AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE,
  AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE,
  AEROGPU_CMD_SET_RENDER_TARGETS_SIZE,
  AEROGPU_CMD_SET_SAMPLERS_SIZE,
  AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE,
  AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE,
  AEROGPU_CMD_UPLOAD_RESOURCE_SIZE,
  AEROGPU_CONSTANT_BUFFER_BINDING_SIZE,
  AEROGPU_COPY_FLAG_WRITEBACK_DST,
  AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE,
  AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdStreamIter,
} from "../../../emulator/protocol/aerogpu/aerogpu_cmd.ts";
import { AerogpuFormat, aerogpuFormatToString, parseAndValidateAbiVersionU32 } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";
import {
  AEROGPU_ALLOC_ENTRY_SIZE as AEROGPU_ALLOC_ENTRY_BYTES,
  AEROGPU_ALLOC_FLAG_READONLY,
  AEROGPU_ALLOC_TABLE_HEADER_SIZE as AEROGPU_ALLOC_TABLE_HEADER_BYTES,
  AEROGPU_ALLOC_TABLE_MAGIC,
} from "../../../emulator/protocol/aerogpu/aerogpu_ring.ts";
import { PCI_MMIO_BASE } from "../arch/guest_phys.ts";
import { guestPaddrToRamOffset, guestRangeInBounds } from "../arch/guest_ram_translate.ts";
import { formatOneLineError } from "../text";

/**
 * Returns whether the lightweight TypeScript CPU executor can handle the given opcode without
 * requiring the wasm/wgpu-backed executor.
 *
 * This is used by the GPU worker to decide which executor to use for a submission.
 *
 * Note: Unknown opcodes return `false` so newer command streams naturally fall back to the more
 * complete executor implementation.
 */
export const aerogpuCpuExecutorSupportsOpcode = (opcode: number): boolean => {
  switch (opcode) {
    case AerogpuCmdOpcode.Nop:
    case AerogpuCmdOpcode.DebugMarker:

    case AerogpuCmdOpcode.CreateBuffer:
    case AerogpuCmdOpcode.CreateTexture2d:
    case AerogpuCmdOpcode.DestroyResource:
    case AerogpuCmdOpcode.ResourceDirtyRange:
    case AerogpuCmdOpcode.UploadResource:
    case AerogpuCmdOpcode.CopyBuffer:
    case AerogpuCmdOpcode.CopyTexture2d:

    case AerogpuCmdOpcode.CreateSampler:
    case AerogpuCmdOpcode.DestroySampler:
    case AerogpuCmdOpcode.SetSamplers:
    case AerogpuCmdOpcode.SetConstantBuffers:
    case AerogpuCmdOpcode.SetShaderResourceBuffers:
    case AerogpuCmdOpcode.SetUnorderedAccessBuffers:

    case AerogpuCmdOpcode.SetRenderTargets:
    case AerogpuCmdOpcode.Dispatch:
    case AerogpuCmdOpcode.Present:
    case AerogpuCmdOpcode.PresentEx:

    case AerogpuCmdOpcode.ExportSharedSurface:
    case AerogpuCmdOpcode.ImportSharedSurface:
    case AerogpuCmdOpcode.ReleaseSharedSurface:
    case AerogpuCmdOpcode.Flush:
      return true;
    default:
      return false;
  }
};

export type AeroGpuCpuTexture = {
  width: number;
  height: number;
  mipLevels: number;
  arrayLayers: number;
  usageFlags: number;
  format: number;
  /// Total packed backing size in bytes for the full mip+array chain, using the canonical
  /// `(array_layer, mip)` packing rules documented in `drivers/aerogpu/protocol/allocation-table.md`.
  totalBackingSizeBytes: number;
  rowPitchBytes: number;
  // Internal representation is always RGBA8, tightly packed.
  data: Uint8Array;
  /// Per-subresource RGBA8 data in D3D subresource order:
  /// `subresource = mip + array_layer * mipLevels`.
  ///
  /// `data` is always an alias of `subresources[0]` (mip0/layer0) for backwards compatibility.
  subresources: Uint8Array[];
  /// Per-subresource guest backing layout for the packed chain, expressed relative to the start of
  /// the resource (i.e. `backing.offsetBytes` is added separately).
  subresourceLayouts: AeroGpuTextureSubresourceLayout[];
  /// Raw packed texture bytes as last established via `UPLOAD_RESOURCE`.
  ///
  /// This stores the guest UMD's canonical packed `(array_layer, mip_level)` layout for the full
  /// mip+array chain. `UPLOAD_RESOURCE` patches are applied into this buffer, and then any affected
  /// subresources are decoded into the RGBA8 `subresources[]` views.
  uploadShadow?: Uint8Array;
  backing?: { allocId: number; offsetBytes: number };
};

export type AeroGpuCpuBuffer = {
  sizeBytes: number;
  usageFlags: number;
  data: Uint8Array;
  backing?: { allocId: number; offsetBytes: number };
};

export type AeroGpuAllocTableEntry = { gpa: number; sizeBytes: number; flags: number };
export type AeroGpuAllocTable = Map<number, AeroGpuAllocTableEntry>;

export type AeroGpuTextureSubresourceLayout = {
  mipLevel: number;
  arrayLayer: number;
  width: number;
  height: number;
  offsetBytes: number;
  rowPitchBytes: number;
  sizeBytes: number;
};

const MAX_U64 = 0xffff_ffff_ffff_ffffn;

export type AerogpuCpuSharedSurfaceState = {
  // share_token -> underlying handle
  byToken: Map<bigint, number>;
  // share_token values that were released and cannot be reused
  retiredTokens: Set<bigint>;
  // handle -> underlying handle (original handles stored as handle->handle; aliases store alias->underlying)
  handles: Map<number, number>;
  // underlying handle -> refcount (original + aliases)
  refcounts: Map<number, number>;
};

export type AerogpuCpuExecutorState = {
  textures: Map<number, AeroGpuCpuTexture>;
  buffers: Map<number, AeroGpuCpuBuffer>;
  currentRenderTarget: number | null;
  presentCount: bigint;
  lastPresentedFrame: { width: number; height: number; rgba8: ArrayBuffer } | null;
  sharedSurfaces: AerogpuCpuSharedSurfaceState;
};

export const createAerogpuCpuExecutorState = (): AerogpuCpuExecutorState => ({
  textures: new Map(),
  buffers: new Map(),
  currentRenderTarget: null,
  presentCount: 0n,
  lastPresentedFrame: null,
  sharedSurfaces: {
    byToken: new Map(),
    retiredTokens: new Set(),
    handles: new Map(),
    refcounts: new Map(),
  },
});

export const resetAerogpuCpuExecutorState = (state: AerogpuCpuExecutorState): void => {
  state.textures.clear();
  state.buffers.clear();
  state.currentRenderTarget = null;
  state.presentCount = 0n;
  state.lastPresentedFrame = null;
  state.sharedSurfaces.byToken.clear();
  state.sharedSurfaces.retiredTokens.clear();
  state.sharedSurfaces.handles.clear();
  state.sharedSurfaces.refcounts.clear();
};

const readU32LeChecked = (dv: DataView, offset: number, limit: number, label: string): number => {
  if (offset < 0 || offset + 4 > limit) {
    throw new Error(`aerogpu: truncated u32 (${label}) at offset ${offset}`);
  }
  return dv.getUint32(offset, true);
};

const readU64LeChecked = (dv: DataView, offset: number, limit: number, label: string): bigint => {
  if (offset < 0 || offset + 8 > limit) {
    throw new Error(`aerogpu: truncated u64 (${label}) at offset ${offset}`);
  }
  return dv.getBigUint64(offset, true);
};

const checkedU64ToNumber = (value: bigint, label: string): number => {
  if (value < 0n) throw new Error(`aerogpu: negative u64 (${label})`);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`aerogpu: ${label} too large for JS number (${value} > ${Number.MAX_SAFE_INTEGER})`);
  }
  return Number(value);
};

export const decodeAerogpuAllocTable = (buf: ArrayBuffer): AeroGpuAllocTable => {
  const dv = new DataView(buf);
  const bufLen = dv.byteLength;
  if (bufLen < AEROGPU_ALLOC_TABLE_HEADER_BYTES) {
    throw new Error(`aerogpu: alloc table too small (${bufLen} bytes)`);
  }

  const magic = dv.getUint32(0, true);
  if (magic !== AEROGPU_ALLOC_TABLE_MAGIC) {
    throw new Error(
      `aerogpu: bad alloc table magic 0x${magic.toString(16)} (expected 0x${AEROGPU_ALLOC_TABLE_MAGIC.toString(16)})`,
    );
  }

  const abiVersion = dv.getUint32(4, true);
  try {
    parseAndValidateAbiVersionU32(abiVersion);
  } catch (err) {
    const message = formatOneLineError(err, 512);
    throw new Error(`aerogpu: unsupported alloc table abi_version=0x${abiVersion.toString(16)} (${message})`);
  }

  const sizeBytes = dv.getUint32(8, true);
  if (sizeBytes < AEROGPU_ALLOC_TABLE_HEADER_BYTES || sizeBytes > bufLen) {
    throw new Error(`aerogpu: invalid alloc table size_bytes=${sizeBytes} (buffer_len=${bufLen})`);
  }

  const entryCount = dv.getUint32(12, true);
  const entryStrideBytes = dv.getUint32(16, true);
  // Forward-compat: newer guests may extend `aerogpu_alloc_entry` and increase the declared
  // stride. We only read the entry prefix we understand.
  if (entryStrideBytes < AEROGPU_ALLOC_ENTRY_BYTES) {
    throw new Error(
      `aerogpu: invalid alloc table entry_stride_bytes=${entryStrideBytes} (expected at least ${AEROGPU_ALLOC_ENTRY_BYTES})`,
    );
  }

  const requiredBytes = BigInt(AEROGPU_ALLOC_TABLE_HEADER_BYTES) + BigInt(entryCount) * BigInt(entryStrideBytes);
  if (requiredBytes > BigInt(sizeBytes)) {
    throw new Error(`aerogpu: alloc table size_bytes too small for layout (${sizeBytes} < ${requiredBytes})`);
  }

  const out: AeroGpuAllocTable = new Map();
  for (let i = 0; i < entryCount; i += 1) {
    const base = AEROGPU_ALLOC_TABLE_HEADER_BYTES + i * entryStrideBytes;
    if (base + AEROGPU_ALLOC_ENTRY_BYTES > sizeBytes) {
      throw new Error(`aerogpu: alloc table entry ${i} out of bounds`);
    }

    const allocId = dv.getUint32(base + 0, true);
    if (allocId === 0) {
      throw new Error(`aerogpu: alloc table entry ${i} has alloc_id=0`);
    }
    const flags = dv.getUint32(base + 4, true);
    const gpaU64 = dv.getBigUint64(base + 8, true);
    const allocSizeBytesU64 = dv.getBigUint64(base + 16, true);

    if (allocSizeBytesU64 === 0n) {
      throw new Error(`aerogpu: alloc table entry ${i} has size_bytes=0`);
    }
    if (gpaU64 + allocSizeBytesU64 > MAX_U64) {
      throw new Error(`aerogpu: alloc table entry ${i} gpa+size overflow`);
    }

    const gpa = checkedU64ToNumber(gpaU64, `allocs[${i}].gpa`);
    const allocSizeBytes = checkedU64ToNumber(allocSizeBytesU64, `allocs[${i}].size_bytes`);
    if (out.has(allocId)) {
      throw new Error(`aerogpu: duplicate alloc_id ${allocId} in alloc table`);
    }
    out.set(allocId, { gpa, sizeBytes: allocSizeBytes, flags });
  }

  return out;
};

const requireGuestU8 = (guestU8: Uint8Array | null | undefined): Uint8Array => {
  if (!guestU8) throw new Error("aerogpu: guest memory is not available (missing WorkerInitMessage)");
  return guestU8;
};

const requireAllocTable = (allocTable: AeroGpuAllocTable | null | undefined): AeroGpuAllocTable => {
  if (!allocTable) throw new Error("aerogpu: alloc table is required for backing_alloc_id resources");
  return allocTable;
};

const toU64 = (value: number, label: string): number => {
  if (!Number.isFinite(value)) throw new Error(`aerogpu: invalid ${label}: expected a finite integer, got ${String(value)}`);
  const int = Math.trunc(value);
  if (!Number.isSafeInteger(int) || int < 0) throw new Error(`aerogpu: invalid ${label}: expected u64, got ${String(value)}`);
  return int;
};

type GuestPhysMapping = {
  guestU8?: Uint8Array | null;
  vramU8?: Uint8Array | null;
  /// Base guest physical address of the `vramU8` BAR1 aperture.
  ///
  /// When omitted, defaults to `PCI_MMIO_BASE` (0xE000_0000) by contract.
  vramBasePaddr?: number;
};

const sliceGuestChecked = (mem: GuestPhysMapping, gpa: number, len: number, label: string): Uint8Array => {
  const addr = toU64(gpa, "guest gpa");
  const length = toU64(len, "len");

  // BAR1 VRAM mapping: treat `[vramBasePaddr..vramBasePaddr+vramLen)` as a flat aperture.
  const vram = mem.vramU8;
  if (vram) {
    const vramBase = toU64(mem.vramBasePaddr ?? PCI_MMIO_BASE, "vram_base_paddr");
    const vramLen = toU64(vram.byteLength, "vram_len");
    const vramEnd = vramBase + vramLen;
    if (vramEnd < vramBase) throw new Error("aerogpu: vram aperture size overflow");

    if (length === 0) {
      if (addr >= vramBase && addr <= vramEnd) {
        const off = addr - vramBase;
        return vram.subarray(off, off);
      }
    } else if (addr >= vramBase && addr <= vramEnd) {
      if (addr < vramEnd && length <= vramEnd - addr) {
        const off = addr - vramBase;
        return vram.subarray(off, off + length);
      }
      throw new Error(
        `aerogpu: vram out of bounds for ${label} (gpa=0x${addr.toString(16)}, len=0x${length.toString(16)}, vram_base=0x${vramBase.toString(16)}, vram_len=0x${vramLen.toString(16)})`,
      );
    }
  }

  // Guest RAM mapping (PC/Q35 low/high RAM with a PCI/MMIO hole).
  const guest = requireGuestU8(mem.guestU8);
  const ramBytes = toU64(guest.byteLength, "guest_len");

  if (!guestRangeInBounds(ramBytes, addr, length)) {
    throw new Error(
      `aerogpu: guest memory out of bounds for ${label} (gpa=0x${addr.toString(16)}, len=0x${length.toString(16)}, guest_len=0x${ramBytes.toString(16)})`,
    );
  }

  const start = guestPaddrToRamOffset(ramBytes, addr);
  if (start === null) {
    // Ranges can be "in bounds" for len=0 at segment boundaries. Treat as a no-op and
    // return an empty view (caller must still tolerate zero-length operations).
    if (length === 0) return guest.subarray(0, 0);
    throw new Error(`aerogpu: guest gpa is not backed by RAM for ${label}: 0x${addr.toString(16)}`);
  }

  const end = start + length;
  if (end < start || end > ramBytes) {
    throw new Error(
      `aerogpu: guest memory out of bounds for ${label} after paddr translation (gpa=0x${addr.toString(16)} ram_off=0x${start.toString(16)} len=0x${length.toString(16)} guest_len=0x${ramBytes.toString(16)})`,
    );
  }

  return guest.subarray(start, end);
};

// -----------------------------------------------------------------------------
// Shared surface bookkeeping (EXPORT_SHARED_SURFACE / IMPORT_SHARED_SURFACE)
// -----------------------------------------------------------------------------

const resolveSharedHandle = (state: AerogpuCpuExecutorState, handle: number): number => {
  return state.sharedSurfaces.handles.get(handle) ?? handle;
};

// Resolves a handle coming from an AeroGPU command stream.
//
// This differs from `resolveSharedHandle()` by treating "reserved underlying IDs" as invalid:
// if an original handle has been destroyed while shared-surface aliases still exist, the
// underlying numeric ID is kept alive in `refcounts` to prevent handle reuse/collision, but the
// original handle value must not be used for subsequent commands.
const resolveSharedCmdHandle = (state: AerogpuCpuExecutorState, handle: number, op: string): number => {
  if (handle === 0) return 0;
  if (state.sharedSurfaces.handles.has(handle)) return resolveSharedHandle(state, handle);
  if (state.sharedSurfaces.refcounts.has(handle)) {
    throw new Error(
      `aerogpu: ${op} shared surface handle ${handle} was destroyed (underlying id kept alive by shared surface aliases)`,
    );
  }
  return handle;
};

const registerSharedHandle = (state: AerogpuCpuExecutorState, handle: number): void => {
  if (handle === 0) return;
  const existing = state.sharedSurfaces.handles.get(handle);
  if (existing != null) {
    if (existing !== handle) {
      throw new Error(`aerogpu: shared surface handle ${handle} is already an alias (underlying=${existing})`);
    }
    return;
  }
  state.sharedSurfaces.handles.set(handle, handle);
  const prev = state.sharedSurfaces.refcounts.get(handle) ?? 0;
  state.sharedSurfaces.refcounts.set(handle, prev + 1);
};

const retireTokensForUnderlying = (state: AerogpuCpuExecutorState, underlying: number): void => {
  const toRetire: bigint[] = [];
  for (const [token, h] of state.sharedSurfaces.byToken) {
    if (h === underlying) toRetire.push(token);
  }
  for (const token of toRetire) {
    state.sharedSurfaces.byToken.delete(token);
    state.sharedSurfaces.retiredTokens.add(token);
  }
};

const exportSharedSurface = (state: AerogpuCpuExecutorState, resourceHandle: number, shareToken: bigint): void => {
  if (resourceHandle === 0) throw new Error("aerogpu: EXPORT_SHARED_SURFACE invalid resource_handle 0");
  if (shareToken === 0n) throw new Error("aerogpu: EXPORT_SHARED_SURFACE invalid share_token 0");
  if (state.sharedSurfaces.retiredTokens.has(shareToken)) {
    throw new Error(`aerogpu: EXPORT_SHARED_SURFACE share_token 0x${shareToken.toString(16)} was previously released`);
  }

  const underlying = state.sharedSurfaces.handles.get(resourceHandle);
  if (underlying == null) {
    throw new Error(`aerogpu: EXPORT_SHARED_SURFACE unknown resource handle ${resourceHandle}`);
  }

  const existing = state.sharedSurfaces.byToken.get(shareToken);
  if (existing != null) {
    if (existing !== underlying) {
      throw new Error(
        `aerogpu: EXPORT_SHARED_SURFACE share_token 0x${shareToken.toString(16)} already exported (existing=${existing} new=${underlying})`,
      );
    }
    return;
  }

  state.sharedSurfaces.byToken.set(shareToken, underlying);
};

const importSharedSurface = (state: AerogpuCpuExecutorState, outHandle: number, shareToken: bigint): void => {
  if (outHandle === 0) throw new Error("aerogpu: IMPORT_SHARED_SURFACE invalid out_resource_handle 0");
  if (shareToken === 0n) throw new Error("aerogpu: IMPORT_SHARED_SURFACE invalid share_token 0");

  const underlying = state.sharedSurfaces.byToken.get(shareToken);
  if (underlying == null) {
    throw new Error(`aerogpu: IMPORT_SHARED_SURFACE unknown share_token 0x${shareToken.toString(16)} (not exported)`);
  }
  if (!state.sharedSurfaces.refcounts.has(underlying)) {
    throw new Error(
      `aerogpu: IMPORT_SHARED_SURFACE share_token 0x${shareToken.toString(16)} refers to destroyed handle ${underlying}`,
    );
  }

  const existing = state.sharedSurfaces.handles.get(outHandle);
  if (existing != null) {
    if (existing !== underlying) {
      throw new Error(
        `aerogpu: IMPORT_SHARED_SURFACE out_resource_handle ${outHandle} already bound (existing=${existing} new=${underlying})`,
      );
    }
    return;
  }

  // Underlying handles remain reserved while aliases still reference them. If an original
  // handle was destroyed, it must not be reused as a new alias handle until the underlying
  // resource is fully released.
  if (state.sharedSurfaces.refcounts.has(outHandle)) {
    throw new Error(`aerogpu: IMPORT_SHARED_SURFACE out_resource_handle ${outHandle} is still in use`);
  }

  // Do not allow aliasing a handle that is already bound to a real resource.
  if (state.textures.has(outHandle) || state.buffers.has(outHandle)) {
    throw new Error(`aerogpu: IMPORT_SHARED_SURFACE out_resource_handle ${outHandle} collides with an existing resource`);
  }

  state.sharedSurfaces.handles.set(outHandle, underlying);
  const prev = state.sharedSurfaces.refcounts.get(underlying) ?? 0;
  state.sharedSurfaces.refcounts.set(underlying, prev + 1);
};

const releaseSharedSurface = (state: AerogpuCpuExecutorState, shareToken: bigint): void => {
  if (shareToken === 0n) return;
  // Idempotent: unknown tokens are a no-op (see `aerogpu_cmd.h` contract).
  //
  // Only retire tokens that were actually exported (present in `byToken`), or that are already
  // retired.
  if (state.sharedSurfaces.byToken.delete(shareToken)) {
    state.sharedSurfaces.retiredTokens.add(shareToken);
  }
};

const destroySharedHandle = (
  state: AerogpuCpuExecutorState,
  handle: number,
): { underlying: number; lastRef: boolean } | null => {
  if (handle === 0) return null;
  const underlying = state.sharedSurfaces.handles.get(handle);
  if (underlying == null) {
    // If the original handle has already been destroyed (removed from `handles`) but the
    // underlying resource is still alive due to aliases, treat duplicate destroys as an
    // idempotent no-op.
    if (state.sharedSurfaces.refcounts.has(handle)) {
      return { underlying: handle, lastRef: false };
    }
    return null;
  }
  state.sharedSurfaces.handles.delete(handle);

  const count = state.sharedSurfaces.refcounts.get(underlying);
  if (count == null) {
    // Table invariant broken (handle tracked but no refcount entry). Treat as last-ref so we don't leak.
    retireTokensForUnderlying(state, underlying);
    return { underlying, lastRef: true };
  }

  const next = Math.max(0, count - 1);
  if (next !== 0) {
    state.sharedSurfaces.refcounts.set(underlying, next);
    return { underlying, lastRef: false };
  }

  state.sharedSurfaces.refcounts.delete(underlying);
  retireTokensForUnderlying(state, underlying);
  return { underlying, lastRef: true };
};

const AEROGPU_CMD_CREATE_BUFFER = AerogpuCmdOpcode.CreateBuffer;
const AEROGPU_CMD_CREATE_TEXTURE2D = AerogpuCmdOpcode.CreateTexture2d;
const AEROGPU_CMD_DESTROY_RESOURCE = AerogpuCmdOpcode.DestroyResource;
const AEROGPU_CMD_UPLOAD_RESOURCE = AerogpuCmdOpcode.UploadResource;
const AEROGPU_CMD_RESOURCE_DIRTY_RANGE = AerogpuCmdOpcode.ResourceDirtyRange;
const AEROGPU_CMD_COPY_BUFFER = AerogpuCmdOpcode.CopyBuffer;
const AEROGPU_CMD_COPY_TEXTURE2D = AerogpuCmdOpcode.CopyTexture2d;
const AEROGPU_CMD_SET_RENDER_TARGETS = AerogpuCmdOpcode.SetRenderTargets;
const AEROGPU_CMD_PRESENT = AerogpuCmdOpcode.Present;
const AEROGPU_CMD_PRESENT_EX = AerogpuCmdOpcode.PresentEx;
const AEROGPU_CMD_EXPORT_SHARED_SURFACE = AerogpuCmdOpcode.ExportSharedSurface;
const AEROGPU_CMD_IMPORT_SHARED_SURFACE = AerogpuCmdOpcode.ImportSharedSurface;
const AEROGPU_CMD_RELEASE_SHARED_SURFACE = AerogpuCmdOpcode.ReleaseSharedSurface;
const AEROGPU_CMD_FLUSH = AerogpuCmdOpcode.Flush;
const AEROGPU_CMD_CREATE_SAMPLER = AerogpuCmdOpcode.CreateSampler;
const AEROGPU_CMD_DESTROY_SAMPLER = AerogpuCmdOpcode.DestroySampler;
const AEROGPU_CMD_SET_SAMPLERS = AerogpuCmdOpcode.SetSamplers;
const AEROGPU_CMD_SET_CONSTANT_BUFFERS = AerogpuCmdOpcode.SetConstantBuffers;
const AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS = AerogpuCmdOpcode.SetShaderResourceBuffers;
const AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS = AerogpuCmdOpcode.SetUnorderedAccessBuffers;
const AEROGPU_CMD_DISPATCH = AerogpuCmdOpcode.Dispatch;

const AEROGPU_FORMAT_B8G8R8A8_UNORM = AerogpuFormat.B8G8R8A8Unorm;
const AEROGPU_FORMAT_B8G8R8X8_UNORM = AerogpuFormat.B8G8R8X8Unorm;
const AEROGPU_FORMAT_R8G8B8A8_UNORM = AerogpuFormat.R8G8B8A8Unorm;
const AEROGPU_FORMAT_R8G8B8X8_UNORM = AerogpuFormat.R8G8B8X8Unorm;

// ABI 1.2+ may introduce additional `AerogpuFormat` enum values (sRGB + BC compressed formats).
// Use a dynamic lookup so this executor can be imported by older tests/builds without requiring
// those enum members to exist at compile-time.
const getOptionalAerogpuFormat = (key: string): number => {
  const value = (AerogpuFormat as Record<string, number>)[key];
  return typeof value === "number" ? value : -1;
};

// sRGB variants are treated identically to UNORM by the CPU executor (no color-space conversion).
const AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB = getOptionalAerogpuFormat("B8G8R8A8UnormSrgb");
const AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB = getOptionalAerogpuFormat("B8G8R8X8UnormSrgb");
const AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB = getOptionalAerogpuFormat("R8G8B8A8UnormSrgb");
const AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB = getOptionalAerogpuFormat("R8G8B8X8UnormSrgb");

const readTexelIntoRgba = (format: number, src: Uint8Array, srcOff: number, dst: Uint8Array, dstOff: number): void => {
  const c0 = src[srcOff + 0]!;
  const c1 = src[srcOff + 1]!;
  const c2 = src[srcOff + 2]!;
  const c3 = src[srcOff + 3]!;

  switch (format) {
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB: {
      dst[dstOff + 0] = c0;
      dst[dstOff + 1] = c1;
      dst[dstOff + 2] = c2;
      dst[dstOff + 3] = c3;
      break;
    }
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB: {
      dst[dstOff + 0] = c0;
      dst[dstOff + 1] = c1;
      dst[dstOff + 2] = c2;
      dst[dstOff + 3] = 255;
      break;
    }
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB: {
      dst[dstOff + 0] = c2;
      dst[dstOff + 1] = c1;
      dst[dstOff + 2] = c0;
      dst[dstOff + 3] = c3;
      break;
    }
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB: {
      dst[dstOff + 0] = c2;
      dst[dstOff + 1] = c1;
      dst[dstOff + 2] = c0;
      dst[dstOff + 3] = 255;
      break;
    }
    default:
      throw new Error(
        `aerogpu: unsupported texture format ${aerogpuFormatToString(format)} (BC formats require GPU backend)`,
      );
  }
};

const writeTexelFromRgba = (format: number, src: Uint8Array, srcOff: number, dst: Uint8Array, dstOff: number): void => {
  const r = src[srcOff + 0]!;
  const g = src[srcOff + 1]!;
  const b = src[srcOff + 2]!;
  const a = src[srcOff + 3]!;

  switch (format) {
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB: {
      dst[dstOff + 0] = r;
      dst[dstOff + 1] = g;
      dst[dstOff + 2] = b;
      dst[dstOff + 3] = a;
      break;
    }
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB: {
      dst[dstOff + 0] = r;
      dst[dstOff + 1] = g;
      dst[dstOff + 2] = b;
      dst[dstOff + 3] = 255;
      break;
    }
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB: {
      dst[dstOff + 0] = b;
      dst[dstOff + 1] = g;
      dst[dstOff + 2] = r;
      dst[dstOff + 3] = a;
      break;
    }
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB: {
      dst[dstOff + 0] = b;
      dst[dstOff + 1] = g;
      dst[dstOff + 2] = r;
      dst[dstOff + 3] = 255;
      break;
    }
    default:
      throw new Error(
        `aerogpu: unsupported texture format ${aerogpuFormatToString(format)} (BC formats require GPU backend)`,
      );
  }
};

const isX8Format = (format: number): boolean =>
  format === AEROGPU_FORMAT_R8G8B8X8_UNORM ||
  format === AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB ||
  format === AEROGPU_FORMAT_B8G8R8X8_UNORM ||
  format === AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB;

const mipDim = (base: number, mipLevel: number): number => Math.max(1, base >>> mipLevel);

const subresourceIndex = (mipLevel: number, arrayLayer: number, mipLevels: number): number => mipLevel + arrayLayer * mipLevels;

const forceOpaqueAlphaRgba8 = (rgba8: Uint8Array): void => {
  for (let i = 3; i < rgba8.length; i += 4) rgba8[i] = 255;
};

const buildTexture2dSubresourceLayouts = (
  width: number,
  height: number,
  mipLevels: number,
  arrayLayers: number,
  mip0RowPitchBytes: number,
): { layouts: AeroGpuTextureSubresourceLayout[]; totalSizeBytes: number } => {
  if (width === 0 || height === 0) {
    throw new Error(`aerogpu: CREATE_TEXTURE2D invalid dimensions ${width}x${height}`);
  }
  if (mipLevels === 0 || arrayLayers === 0) {
    throw new Error(`aerogpu: CREATE_TEXTURE2D invalid mip_levels/array_layers ${mipLevels}/${arrayLayers}`);
  }

  const layouts: AeroGpuTextureSubresourceLayout[] = [];
  let offsetBytes = 0;

  for (let layer = 0; layer < arrayLayers; layer += 1) {
    for (let mip = 0; mip < mipLevels; mip += 1) {
      const mipW = mipDim(width, mip);
      const mipH = mipDim(height, mip);
      const rowBytes = mipW * 4;
      const rowPitchBytes = mip === 0 ? mip0RowPitchBytes : rowBytes;
      if (rowPitchBytes < rowBytes) {
        throw new Error(
          `aerogpu: CREATE_TEXTURE2D mip${mip} row_pitch_bytes too small (${rowPitchBytes} < ${rowBytes})`,
        );
      }
      if (rowPitchBytes % 4 !== 0) {
        throw new Error(`aerogpu: CREATE_TEXTURE2D row_pitch_bytes must be a multiple of 4 (got ${rowPitchBytes})`);
      }

      const sizeBytes = rowPitchBytes * mipH;
      layouts.push({
        mipLevel: mip,
        arrayLayer: layer,
        width: mipW,
        height: mipH,
        offsetBytes,
        rowPitchBytes,
        sizeBytes,
      });
      offsetBytes += sizeBytes;
    }
  }

  return { layouts, totalSizeBytes: offsetBytes };
};

const uploadTextureSubresourceFromGuest = (
  handle: number,
  format: number,
  layout: AeroGpuTextureSubresourceLayout,
  dstRgba8: Uint8Array,
  mem: GuestPhysMapping,
  baseGpa: number,
): void => {
  const rowBytes = layout.width * 4;
  for (let y = 0; y < layout.height; y += 1) {
    const srcRow = sliceGuestChecked(
      mem,
      baseGpa + layout.offsetBytes + y * layout.rowPitchBytes,
      rowBytes,
      `texture ${handle} subresource upload`,
    );
    const dstRowOff = y * rowBytes;
    if (format === AEROGPU_FORMAT_R8G8B8A8_UNORM || format === AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB) {
      dstRgba8.set(srcRow, dstRowOff);
    } else {
      for (let x = 0; x < rowBytes; x += 4) {
        readTexelIntoRgba(format, srcRow, x, dstRgba8, dstRowOff + x);
      }
    }
  }
};

export type ExecuteAerogpuCmdStreamOptions = {
  allocTable: AeroGpuAllocTable | null;
  guestU8: Uint8Array | null;
  /**
   * Optional BAR1 VRAM aperture backing store.
   *
   * When provided, guest-physical addresses in the PCI/MMIO hole starting at
   * `vramBasePaddr` (default: `PCI_MMIO_BASE`) will be resolved into this buffer
   * instead of hard-failing as "not backed by RAM".
   */
  vramU8?: Uint8Array | null;
  /**
   * Base guest physical address of `vramU8`.
   *
   * Defaults to `PCI_MMIO_BASE` (0xE000_0000).
   */
  vramBasePaddr?: number;
  presentTexture?: (tex: AeroGpuCpuTexture) => void;
};

export const executeAerogpuCmdStream = (
  state: AerogpuCpuExecutorState,
  cmdStream: ArrayBuffer,
  opts: ExecuteAerogpuCmdStreamOptions,
): bigint => {
  const iter = new AerogpuCmdStreamIter(cmdStream);
  const dv = iter.view;
  let presentDelta = 0n;

  for (const packet of iter) {
    const offset = packet.offsetBytes;
    const end = packet.endBytes;
    const opcode = packet.hdr.opcode;
    const cmdSizeBytes = packet.hdr.sizeBytes;

    switch (opcode) {
      case AerogpuCmdOpcode.Nop: {
        // No-op packet (explicitly accepted for forward-compat).
        break;
      }
      case AerogpuCmdOpcode.DebugMarker: {
        // Debug-only packet: payload is a UTF-8 marker string. The CPU executor ignores it, but
        // still accepts it so command streams produced by debug builds remain valid.
        break;
      }
      case AEROGPU_CMD_CREATE_BUFFER: {
        if (cmdSizeBytes < AEROGPU_CMD_CREATE_BUFFER_SIZE) {
          throw new Error(`aerogpu: CREATE_BUFFER packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "buffer_handle");
        const usageFlags = readU32LeChecked(dv, offset + 12, end, "usage_flags");
        const sizeBytesU64 = readU64LeChecked(dv, offset + 16, end, "size_bytes");
        const sizeBytes = checkedU64ToNumber(sizeBytesU64, "size_bytes");
        const backingAllocId = readU32LeChecked(dv, offset + 24, end, "backing_alloc_id");
        const backingOffsetBytes = readU32LeChecked(dv, offset + 28, end, "backing_offset_bytes");
        if (handle === 0) throw new Error("aerogpu: CREATE_BUFFER invalid handle 0");
        const shared = state.sharedSurfaces.handles.get(handle);
        if (shared != null && shared !== handle) {
          throw new Error(`aerogpu: CREATE_BUFFER handle ${handle} is already an alias (underlying=${shared})`);
        }
        if (shared == null && state.sharedSurfaces.refcounts.has(handle)) {
          throw new Error(
            `aerogpu: CREATE_BUFFER handle ${handle} is still in use (underlying id kept alive by shared surface aliases)`,
          );
        }

        if (state.textures.has(handle)) {
          throw new Error(`aerogpu: CREATE_BUFFER handle ${handle} is already bound to a texture`);
        }

        const backing = (() => {
          if (backingAllocId === 0) return undefined;
          const table = requireAllocTable(opts.allocTable);
          const alloc = table.get(backingAllocId);
          if (!alloc) {
            throw new Error(`aerogpu: CREATE_BUFFER unknown alloc_id ${backingAllocId} for buffer ${handle}`);
          }
          const endBytes = BigInt(backingOffsetBytes) + BigInt(sizeBytes);
          if (endBytes > BigInt(alloc.sizeBytes)) {
            throw new Error(
              `aerogpu: CREATE_BUFFER backing out of bounds (alloc_id=${backingAllocId}, offset=${backingOffsetBytes}, size=${sizeBytes}, allocBytes=${alloc.sizeBytes})`,
            );
          }
          return { allocId: backingAllocId, offsetBytes: backingOffsetBytes };
        })();

        const existing = state.buffers.get(handle);
        if (existing) {
          if (existing.sizeBytes !== sizeBytes || existing.usageFlags !== usageFlags) {
            throw new Error(
              `aerogpu: CREATE_BUFFER rebind mismatch for handle ${handle} (expected size=${existing.sizeBytes} usage=0x${existing.usageFlags.toString(16)}, got size=${sizeBytes} usage=0x${usageFlags.toString(16)})`,
            );
          }
          existing.backing = backing;
          registerSharedHandle(state, handle);
          break;
        }

        if (state.currentRenderTarget === handle) state.currentRenderTarget = null;
        const buf: AeroGpuCpuBuffer = { sizeBytes, usageFlags, data: new Uint8Array(sizeBytes) };
        buf.backing = backing;
        state.buffers.set(handle, buf);
        registerSharedHandle(state, handle);
        break;
      }

      case AEROGPU_CMD_CREATE_SAMPLER: {
        if (cmdSizeBytes < AEROGPU_CMD_CREATE_SAMPLER_SIZE) {
          throw new Error(`aerogpu: CREATE_SAMPLER packet too small (size_bytes=${cmdSizeBytes})`);
        }
        // Sampler objects are currently ignored by the CPU executor. Still validate packet sizing
        // so corrupted streams fail fast.
        break;
      }

      case AEROGPU_CMD_DESTROY_SAMPLER: {
        if (cmdSizeBytes < AEROGPU_CMD_DESTROY_SAMPLER_SIZE) {
          throw new Error(`aerogpu: DESTROY_SAMPLER packet too small (size_bytes=${cmdSizeBytes})`);
        }
        break;
      }

      case AEROGPU_CMD_SET_SAMPLERS: {
        if (cmdSizeBytes < AEROGPU_CMD_SET_SAMPLERS_SIZE) {
          throw new Error(`aerogpu: SET_SAMPLERS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const samplerCount = readU32LeChecked(dv, offset + 16, end, "sampler_count");
        const expectedBytes = AEROGPU_CMD_SET_SAMPLERS_SIZE + samplerCount * 4;
        if (expectedBytes > cmdSizeBytes) {
          throw new Error(
            `aerogpu: SET_SAMPLERS payload overruns packet (expected=${expectedBytes}, size_bytes=${cmdSizeBytes})`,
          );
        }
        break;
      }

      case AEROGPU_CMD_SET_CONSTANT_BUFFERS: {
        if (cmdSizeBytes < AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE) {
          throw new Error(`aerogpu: SET_CONSTANT_BUFFERS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const bufferCount = readU32LeChecked(dv, offset + 16, end, "buffer_count");
        const expectedBytes = AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + bufferCount * AEROGPU_CONSTANT_BUFFER_BINDING_SIZE;
        if (expectedBytes > cmdSizeBytes) {
          throw new Error(
            `aerogpu: SET_CONSTANT_BUFFERS payload overruns packet (expected=${expectedBytes}, size_bytes=${cmdSizeBytes})`,
          );
        }
        break;
      }

      case AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS: {
        if (cmdSizeBytes < AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE) {
          throw new Error(`aerogpu: SET_SHADER_RESOURCE_BUFFERS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const bufferCount = readU32LeChecked(dv, offset + 16, end, "buffer_count");
        const expectedBytes =
          AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + bufferCount * AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE;
        if (expectedBytes > cmdSizeBytes) {
          throw new Error(
            `aerogpu: SET_SHADER_RESOURCE_BUFFERS payload overruns packet (expected=${expectedBytes}, size_bytes=${cmdSizeBytes})`,
          );
        }
        break;
      }

      case AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS: {
        if (cmdSizeBytes < AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE) {
          throw new Error(`aerogpu: SET_UNORDERED_ACCESS_BUFFERS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const uavCount = readU32LeChecked(dv, offset + 16, end, "uav_count");
        const expectedBytes =
          AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + uavCount * AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE;
        if (expectedBytes > cmdSizeBytes) {
          throw new Error(
            `aerogpu: SET_UNORDERED_ACCESS_BUFFERS payload overruns packet (expected=${expectedBytes}, size_bytes=${cmdSizeBytes})`,
          );
        }
        break;
      }

      case AEROGPU_CMD_CREATE_TEXTURE2D: {
        if (cmdSizeBytes < AEROGPU_CMD_CREATE_TEXTURE2D_SIZE) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "texture_handle");
        const usageFlags = readU32LeChecked(dv, offset + 12, end, "usage_flags");
        const format = readU32LeChecked(dv, offset + 16, end, "format");
        const width = readU32LeChecked(dv, offset + 20, end, "width");
        const height = readU32LeChecked(dv, offset + 24, end, "height");
        const mipLevels = readU32LeChecked(dv, offset + 28, end, "mip_levels");
        const arrayLayers = readU32LeChecked(dv, offset + 32, end, "array_layers");
        const rowPitchBytesRaw = readU32LeChecked(dv, offset + 36, end, "row_pitch_bytes");
        const backingAllocId = readU32LeChecked(dv, offset + 40, end, "backing_alloc_id");
        const backingOffsetBytes = readU32LeChecked(dv, offset + 44, end, "backing_offset_bytes");

        if (handle === 0) throw new Error("aerogpu: CREATE_TEXTURE2D invalid handle 0");
        const shared = state.sharedSurfaces.handles.get(handle);
        if (shared != null && shared !== handle) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D handle ${handle} is already an alias (underlying=${shared})`);
        }
        if (shared == null && state.sharedSurfaces.refcounts.has(handle)) {
          throw new Error(
            `aerogpu: CREATE_TEXTURE2D handle ${handle} is still in use (underlying id kept alive by shared surface aliases)`,
          );
        }
        if (state.buffers.has(handle)) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D handle ${handle} is already bound to a buffer`);
        }
        if (width === 0 || height === 0) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D invalid dimensions ${width}x${height}`);
        }
        if (mipLevels === 0 || arrayLayers === 0) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D invalid mip_levels/array_layers ${mipLevels}/${arrayLayers}`);
        }
        if (backingAllocId !== 0 && rowPitchBytesRaw === 0) {
          throw new Error("aerogpu: CREATE_TEXTURE2D backing_alloc_id requires non-zero row_pitch_bytes");
        }
        if (
          format !== AEROGPU_FORMAT_R8G8B8A8_UNORM &&
          format !== AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB &&
          format !== AEROGPU_FORMAT_R8G8B8X8_UNORM &&
          format !== AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB &&
          format !== AEROGPU_FORMAT_B8G8R8A8_UNORM &&
          format !== AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB &&
          format !== AEROGPU_FORMAT_B8G8R8X8_UNORM &&
          format !== AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
        ) {
          throw new Error(
            `aerogpu: CREATE_TEXTURE2D unsupported format ${aerogpuFormatToString(format)} (BC formats require GPU backend)`,
          );
        }

        const rowBytes = width * 4;
        const rowPitchBytes = rowPitchBytesRaw !== 0 ? rowPitchBytesRaw : rowBytes;
        if (rowPitchBytes < rowBytes) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D row_pitch_bytes too small (${rowPitchBytes} < ${rowBytes})`);
        }
        if (rowPitchBytes % 4 !== 0) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D row_pitch_bytes must be a multiple of 4 (got ${rowPitchBytes})`);
        }

        const { layouts: subresourceLayouts, totalSizeBytes: totalBackingSizeBytes } = buildTexture2dSubresourceLayouts(
          width,
          height,
          mipLevels,
          arrayLayers,
          rowPitchBytes,
        );

        const backing = (() => {
          if (backingAllocId === 0) return undefined;
          const table = requireAllocTable(opts.allocTable);
          const alloc = table.get(backingAllocId);
          if (!alloc) {
            throw new Error(`aerogpu: CREATE_TEXTURE2D unknown alloc_id ${backingAllocId} for texture ${handle}`);
          }
          const endBytes = BigInt(backingOffsetBytes) + BigInt(totalBackingSizeBytes);
          if (endBytes > BigInt(alloc.sizeBytes)) {
            throw new Error(
              `aerogpu: CREATE_TEXTURE2D backing out of bounds (alloc_id=${backingAllocId}, offset=${backingOffsetBytes}, requiredBytes=${totalBackingSizeBytes}, allocBytes=${alloc.sizeBytes})`,
            );
          }
          return { allocId: backingAllocId, offsetBytes: backingOffsetBytes };
        })();

        const existing = state.textures.get(handle);
        if (existing) {
          if (
            existing.width !== width ||
            existing.height !== height ||
            existing.mipLevels !== mipLevels ||
            existing.arrayLayers !== arrayLayers ||
            existing.usageFlags !== usageFlags ||
            existing.format !== format ||
            existing.rowPitchBytes !== rowPitchBytes
          ) {
            throw new Error(
              `aerogpu: CREATE_TEXTURE2D rebind mismatch for handle ${handle} (expected ${existing.width}x${existing.height} mipLevels=${existing.mipLevels} arrayLayers=${existing.arrayLayers} fmt=${aerogpuFormatToString(existing.format)} usage=0x${existing.usageFlags.toString(16)} rowPitch=${existing.rowPitchBytes}, got ${width}x${height} mipLevels=${mipLevels} arrayLayers=${arrayLayers} fmt=${aerogpuFormatToString(format)} usage=0x${usageFlags.toString(16)} rowPitch=${rowPitchBytes})`,
            );
          }
          existing.backing = backing;
          registerSharedHandle(state, handle);
          break;
        }

        const subresourceCount = mipLevels * arrayLayers;
        const subresources = new Array<Uint8Array>(subresourceCount);
        let totalInternalBytes = 0n;
        for (let i = 0; i < subresourceLayouts.length; i += 1) {
          const layout = subresourceLayouts[i]!;
          const byteLenBig = BigInt(layout.width) * BigInt(layout.height) * 4n;
          if (byteLenBig > BigInt(Number.MAX_SAFE_INTEGER)) {
            throw new Error(`aerogpu: CREATE_TEXTURE2D texture too large for JS (subresource bytes=${byteLenBig})`);
          }
          const bytes = new Uint8Array(Number(byteLenBig));
          if (isX8Format(format)) {
            forceOpaqueAlphaRgba8(bytes);
          }
          subresources[i] = bytes;
          totalInternalBytes += byteLenBig;
        }
        if (totalInternalBytes > BigInt(Number.MAX_SAFE_INTEGER)) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D texture too large for JS (total bytes=${totalInternalBytes})`);
        }
        const data = subresources[0]!;
        const tex: AeroGpuCpuTexture = {
          width,
          height,
          mipLevels,
          arrayLayers,
          usageFlags,
          format,
          totalBackingSizeBytes: totalBackingSizeBytes,
          rowPitchBytes,
          data,
          subresources,
          subresourceLayouts,
        };
        tex.backing = backing;
        state.textures.set(handle, tex);
        registerSharedHandle(state, handle);
        break;
      }

      case AEROGPU_CMD_DESTROY_RESOURCE: {
        if (cmdSizeBytes < AEROGPU_CMD_DESTROY_RESOURCE_SIZE) {
          throw new Error(`aerogpu: DESTROY_RESOURCE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        // Shared surfaces: alias handles are reference-counted and resolve to an underlying
        // resource. Only destroy the underlying resource once the final handle is released.
        const shared = destroySharedHandle(state, handle);
        if (shared) {
          if (shared.lastRef) {
            state.textures.delete(shared.underlying);
            state.buffers.delete(shared.underlying);
            if (state.currentRenderTarget === shared.underlying) state.currentRenderTarget = null;
          }
          break;
        }

        state.textures.delete(handle);
        state.buffers.delete(handle);
        if (state.currentRenderTarget === handle) state.currentRenderTarget = null;
        break;
      }

      case AEROGPU_CMD_RESOURCE_DIRTY_RANGE: {
        if (cmdSizeBytes < AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE) {
          throw new Error(`aerogpu: RESOURCE_DIRTY_RANGE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handleRaw = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        const handle = resolveSharedCmdHandle(state, handleRaw, "RESOURCE_DIRTY_RANGE");
        const dirtyOffsetBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 16, end, "offset_bytes"), "offset_bytes");
        const dirtySizeBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 24, end, "size_bytes"), "size_bytes");
        if (dirtySizeBytes === 0) break;

        const table = requireAllocTable(opts.allocTable);

        const buf = state.buffers.get(handle);
        if (buf) {
          const backing = buf.backing;
          if (!backing) {
            throw new Error(`aerogpu: RESOURCE_DIRTY_RANGE buffer ${handle} has no backing_alloc_id`);
          }
          if (dirtyOffsetBytes + dirtySizeBytes > buf.sizeBytes) {
            throw new Error(
              `aerogpu: RESOURCE_DIRTY_RANGE out of bounds for buffer ${handle} (offset=${dirtyOffsetBytes}, size=${dirtySizeBytes}, bufBytes=${buf.sizeBytes})`,
            );
          }
          const alloc = table.get(backing.allocId);
          if (!alloc) throw new Error(`aerogpu: unknown alloc_id ${backing.allocId} for buffer ${handle}`);
          if (dirtyOffsetBytes + dirtySizeBytes > alloc.sizeBytes - backing.offsetBytes) {
            throw new Error(
              `aerogpu: RESOURCE_DIRTY_RANGE out of bounds for alloc ${backing.allocId} (offset=${dirtyOffsetBytes}, size=${dirtySizeBytes}, allocBytes=${alloc.sizeBytes})`,
            );
          }

          const baseGpa = alloc.gpa + backing.offsetBytes;
          buf.data.set(
            sliceGuestChecked(opts, baseGpa + dirtyOffsetBytes, dirtySizeBytes, `buffer ${handle}`),
            dirtyOffsetBytes,
          );
          break;
        }

        const tex = state.textures.get(handle);
        if (!tex) {
          throw new Error(`aerogpu: RESOURCE_DIRTY_RANGE references unknown resource handle ${handleRaw} (resolved=${handle})`);
        }

        const backing = tex.backing;
        if (!backing) {
          throw new Error(`aerogpu: RESOURCE_DIRTY_RANGE texture ${handle} has no backing_alloc_id`);
        }
        const alloc = table.get(backing.allocId);
        if (!alloc) throw new Error(`aerogpu: unknown alloc_id ${backing.allocId} for texture ${handle}`);

        const textureBytes = tex.totalBackingSizeBytes;
        if (dirtyOffsetBytes + dirtySizeBytes > textureBytes) {
          throw new Error(
            `aerogpu: RESOURCE_DIRTY_RANGE out of bounds for texture ${handle} (offset=${dirtyOffsetBytes}, size=${dirtySizeBytes}, texBytes=${textureBytes})`,
          );
        }
        if (dirtyOffsetBytes + dirtySizeBytes > alloc.sizeBytes - backing.offsetBytes) {
          throw new Error(
            `aerogpu: RESOURCE_DIRTY_RANGE out of bounds for alloc ${backing.allocId} (offset=${dirtyOffsetBytes}, size=${dirtySizeBytes}, allocBytes=${alloc.sizeBytes})`,
          );
        }

        const dirtyStart = dirtyOffsetBytes;
        const dirtyEnd = dirtyOffsetBytes + dirtySizeBytes;

        const baseGpa = alloc.gpa + backing.offsetBytes;
        for (let i = 0; i < tex.subresourceLayouts.length; i += 1) {
          const layout = tex.subresourceLayouts[i]!;
          const subStart = layout.offsetBytes;
          const subEnd = subStart + layout.sizeBytes;
          if (subEnd <= dirtyStart || subStart >= dirtyEnd) continue;
          uploadTextureSubresourceFromGuest(handle, tex.format, layout, tex.subresources[i]!, opts, baseGpa);
        }
        break;
      }

      case AEROGPU_CMD_UPLOAD_RESOURCE: {
        if (cmdSizeBytes < AEROGPU_CMD_UPLOAD_RESOURCE_SIZE) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handleRaw = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        const handle = resolveSharedCmdHandle(state, handleRaw, "UPLOAD_RESOURCE");
        const offsetBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 16, end, "offset_bytes"), "offset_bytes");
        const sizeBytesU64 = readU64LeChecked(dv, offset + 24, end, "size_bytes");
        const uploadBytes = checkedU64ToNumber(sizeBytesU64, "size_bytes");

        const dataStart = offset + AEROGPU_CMD_UPLOAD_RESOURCE_SIZE;
        const dataEnd = dataStart + uploadBytes;
        if (dataEnd > end) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE payload overruns packet (dataEnd=${dataEnd}, end=${end})`);
        }

        const srcBytes = new Uint8Array(cmdStream, dataStart, uploadBytes);

        const buf = state.buffers.get(handle);
        if (buf) {
          if (offsetBytes + uploadBytes > buf.data.byteLength) {
            throw new Error(
              `aerogpu: UPLOAD_RESOURCE out of bounds (offset=${offsetBytes}, size=${uploadBytes}, bufBytes=${buf.data.byteLength})`,
            );
          }
          buf.data.set(srcBytes, offsetBytes);
          break;
        }

        const tex = state.textures.get(handle);
        if (!tex) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE references unknown resource handle ${handleRaw} (resolved=${handle})`);
        }

        const totalBackingBytes = tex.totalBackingSizeBytes;
        if (offsetBytes + uploadBytes > totalBackingBytes) {
          throw new Error(
            `aerogpu: UPLOAD_RESOURCE out of bounds (offset=${offsetBytes}, size=${uploadBytes}, texBytes=${totalBackingBytes})`,
          );
        }
        if (
          tex.format !== AEROGPU_FORMAT_R8G8B8A8_UNORM &&
          tex.format !== AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB &&
          (offsetBytes % 4 !== 0 || uploadBytes % 4 !== 0)
        ) {
          throw new Error("aerogpu: UPLOAD_RESOURCE texture uploads must be 4-byte aligned");
        }

        if (!tex.uploadShadow || tex.uploadShadow.byteLength !== totalBackingBytes) {
          tex.uploadShadow = new Uint8Array(totalBackingBytes);
        }
        tex.uploadShadow.set(srcBytes, offsetBytes);

        const uploadStart = offsetBytes;
        const uploadEnd = offsetBytes + uploadBytes;
        for (let i = 0; i < tex.subresourceLayouts.length; i += 1) {
          const layout = tex.subresourceLayouts[i]!;
          const subStart = layout.offsetBytes;
          const subEnd = subStart + layout.sizeBytes;
          if (subEnd <= uploadStart || subStart >= uploadEnd) continue;

          const shadow = tex.uploadShadow.subarray(subStart, subEnd);
          const dstRgba8 = tex.subresources[i]!;
          const rowBytes = layout.width * 4;
          for (let y = 0; y < layout.height; y += 1) {
            const srcRowOff = y * layout.rowPitchBytes;
            const dstRowOff = y * rowBytes;
            if (tex.format === AEROGPU_FORMAT_R8G8B8A8_UNORM || tex.format === AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB) {
              dstRgba8.set(shadow.subarray(srcRowOff, srcRowOff + rowBytes), dstRowOff);
              continue;
            }
            for (let x = 0; x < rowBytes; x += 4) {
              readTexelIntoRgba(tex.format, shadow, srcRowOff + x, dstRgba8, dstRowOff + x);
            }
          }
        }
        break;
      }

      case AEROGPU_CMD_COPY_BUFFER: {
        if (cmdSizeBytes < AEROGPU_CMD_COPY_BUFFER_SIZE) {
          throw new Error(`aerogpu: COPY_BUFFER packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const dstBufferRaw = readU32LeChecked(dv, offset + 8, end, "dst_buffer");
        const srcBufferRaw = readU32LeChecked(dv, offset + 12, end, "src_buffer");
        const dstBuffer = resolveSharedCmdHandle(state, dstBufferRaw, "COPY_BUFFER");
        const srcBuffer = resolveSharedCmdHandle(state, srcBufferRaw, "COPY_BUFFER");
        const dstOffsetBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 16, end, "dst_offset_bytes"), "dst_offset_bytes");
        const srcOffsetBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 24, end, "src_offset_bytes"), "src_offset_bytes");
        const sizeBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 32, end, "size_bytes"), "size_bytes");
        const flags = readU32LeChecked(dv, offset + 40, end, "flags");

        if (flags !== 0 && flags !== AEROGPU_COPY_FLAG_WRITEBACK_DST) {
          throw new Error(`aerogpu: COPY_BUFFER unsupported flags 0x${flags.toString(16)}`);
        }
        if (sizeBytes === 0) break;
        if (dstBuffer === 0 || srcBuffer === 0) {
          throw new Error("aerogpu: COPY_BUFFER resource handles must be non-zero");
        }
        if (dstBuffer === srcBuffer) {
          throw new Error("aerogpu: COPY_BUFFER src==dst is not supported");
        }

        const src = state.buffers.get(srcBuffer);
        if (!src) throw new Error(`aerogpu: COPY_BUFFER unknown src buffer ${srcBufferRaw} (resolved=${srcBuffer})`);
        const dst = state.buffers.get(dstBuffer);
        if (!dst) throw new Error(`aerogpu: COPY_BUFFER unknown dst buffer ${dstBufferRaw} (resolved=${dstBuffer})`);

        if (srcOffsetBytes + sizeBytes > src.data.byteLength || dstOffsetBytes + sizeBytes > dst.data.byteLength) {
          throw new Error("aerogpu: COPY_BUFFER out of bounds");
        }

        type CopyBufferWriteback = { gpa: number };
        let writeback: CopyBufferWriteback | null = null;
        if (flags === AEROGPU_COPY_FLAG_WRITEBACK_DST) {
          const backing = dst.backing;
          if (!backing) {
            throw new Error(`aerogpu: COPY_BUFFER writeback requires dst buffer ${dstBuffer} to have backing_alloc_id`);
          }

          const table = requireAllocTable(opts.allocTable);
          const alloc = table.get(backing.allocId);
          if (!alloc) throw new Error(`aerogpu: unknown alloc_id ${backing.allocId} for buffer ${dstBuffer}`);
          if ((alloc.flags & AEROGPU_ALLOC_FLAG_READONLY) !== 0) {
            throw new Error(`aerogpu: COPY_BUFFER writeback denied: alloc_id ${backing.allocId} is READONLY`);
          }
          if (dstOffsetBytes + sizeBytes > alloc.sizeBytes - backing.offsetBytes) {
            throw new Error(
              `aerogpu: COPY_BUFFER writeback out of bounds for alloc ${backing.allocId} (dst_offset=${dstOffsetBytes}, size=${sizeBytes}, allocBytes=${alloc.sizeBytes})`,
            );
          }

          writeback = { gpa: alloc.gpa + backing.offsetBytes + dstOffsetBytes };
        }

        const tmp = src.data.slice(srcOffsetBytes, srcOffsetBytes + sizeBytes);
        dst.data.set(tmp, dstOffsetBytes);

        if (writeback) {
          sliceGuestChecked(opts, writeback.gpa, sizeBytes, `buffer ${dstBuffer} writeback`).set(tmp);
        }
        break;
      }

      case AEROGPU_CMD_COPY_TEXTURE2D: {
        if (cmdSizeBytes < AEROGPU_CMD_COPY_TEXTURE2D_SIZE) {
          throw new Error(`aerogpu: COPY_TEXTURE2D packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const dstTextureRaw = readU32LeChecked(dv, offset + 8, end, "dst_texture");
        const srcTextureRaw = readU32LeChecked(dv, offset + 12, end, "src_texture");
        const dstTexture = resolveSharedCmdHandle(state, dstTextureRaw, "COPY_TEXTURE2D");
        const srcTexture = resolveSharedCmdHandle(state, srcTextureRaw, "COPY_TEXTURE2D");
        const dstMipLevel = readU32LeChecked(dv, offset + 16, end, "dst_mip_level");
        const dstArrayLayer = readU32LeChecked(dv, offset + 20, end, "dst_array_layer");
        const srcMipLevel = readU32LeChecked(dv, offset + 24, end, "src_mip_level");
        const srcArrayLayer = readU32LeChecked(dv, offset + 28, end, "src_array_layer");
        const dstX = readU32LeChecked(dv, offset + 32, end, "dst_x");
        const dstY = readU32LeChecked(dv, offset + 36, end, "dst_y");
        const srcX = readU32LeChecked(dv, offset + 40, end, "src_x");
        const srcY = readU32LeChecked(dv, offset + 44, end, "src_y");
        const width = readU32LeChecked(dv, offset + 48, end, "width");
        const height = readU32LeChecked(dv, offset + 52, end, "height");
        const flags = readU32LeChecked(dv, offset + 56, end, "flags");

        if (flags !== 0 && flags !== AEROGPU_COPY_FLAG_WRITEBACK_DST) {
          throw new Error(`aerogpu: COPY_TEXTURE2D unsupported flags 0x${flags.toString(16)}`);
        }
        if (width === 0 || height === 0) break;
        if (dstTexture === 0 || srcTexture === 0) {
          throw new Error("aerogpu: COPY_TEXTURE2D resource handles must be non-zero");
        }

        const src = state.textures.get(srcTexture);
        if (!src) throw new Error(`aerogpu: COPY_TEXTURE2D unknown src texture ${srcTextureRaw} (resolved=${srcTexture})`);
        const dst = state.textures.get(dstTexture);
        if (!dst) throw new Error(`aerogpu: COPY_TEXTURE2D unknown dst texture ${dstTextureRaw} (resolved=${dstTexture})`);
        if (src.format !== dst.format) {
          throw new Error(
            `aerogpu: COPY_TEXTURE2D format mismatch (src=${aerogpuFormatToString(src.format)}, dst=${aerogpuFormatToString(dst.format)})`,
          );
        }
        if (srcMipLevel >= src.mipLevels || dstMipLevel >= dst.mipLevels) {
          throw new Error("aerogpu: COPY_TEXTURE2D mip_level out of bounds");
        }
        if (srcArrayLayer >= src.arrayLayers || dstArrayLayer >= dst.arrayLayers) {
          throw new Error("aerogpu: COPY_TEXTURE2D array_layer out of bounds");
        }

        const srcW = mipDim(src.width, srcMipLevel);
        const srcH = mipDim(src.height, srcMipLevel);
        const dstW = mipDim(dst.width, dstMipLevel);
        const dstH = mipDim(dst.height, dstMipLevel);
        if (srcX + width > srcW || srcY + height > srcH) {
          throw new Error("aerogpu: COPY_TEXTURE2D src rect out of bounds");
        }
        if (dstX + width > dstW || dstY + height > dstH) {
          throw new Error("aerogpu: COPY_TEXTURE2D dst rect out of bounds");
        }

        const srcSub = subresourceIndex(srcMipLevel, srcArrayLayer, src.mipLevels);
        const dstSub = subresourceIndex(dstMipLevel, dstArrayLayer, dst.mipLevels);
        const srcBytes = src.subresources[srcSub];
        if (!srcBytes) throw new Error("aerogpu: COPY_TEXTURE2D missing src subresource bytes");
        const dstBytes = dst.subresources[dstSub];
        if (!dstBytes) throw new Error("aerogpu: COPY_TEXTURE2D missing dst subresource bytes");

        const rowBytes = width * 4;
        const tmp = new Uint8Array(rowBytes * height);
        for (let row = 0; row < height; row += 1) {
          const srcOff = ((srcY + row) * srcW + srcX) * 4;
          tmp.set(srcBytes.subarray(srcOff, srcOff + rowBytes), row * rowBytes);
        }

        type CopyTexture2dWriteback = { baseGpa: number };
        let writeback: CopyTexture2dWriteback | null = null;
        if (flags === AEROGPU_COPY_FLAG_WRITEBACK_DST) {
          const backing = dst.backing;
          if (!backing) {
            throw new Error(`aerogpu: COPY_TEXTURE2D writeback requires dst texture ${dstTexture} to have backing_alloc_id`);
          }

          const table = requireAllocTable(opts.allocTable);
          const alloc = table.get(backing.allocId);
          if (!alloc) throw new Error(`aerogpu: unknown alloc_id ${backing.allocId} for texture ${dstTexture}`);
          if ((alloc.flags & AEROGPU_ALLOC_FLAG_READONLY) !== 0) {
            throw new Error(`aerogpu: COPY_TEXTURE2D writeback denied: alloc_id ${backing.allocId} is READONLY`);
          }

          // Bounds check against destination backing allocation.
          const lastRow = dstY + height - 1;
          const dstLayout = dst.subresourceLayouts[dstSub];
          if (!dstLayout) {
            throw new Error(`aerogpu: COPY_TEXTURE2D missing dst layout for subresource ${dstSub}`);
          }
          const endInBacking = dstLayout.offsetBytes + lastRow * dstLayout.rowPitchBytes + dstX * 4 + rowBytes;
          if (endInBacking > alloc.sizeBytes - backing.offsetBytes) {
            throw new Error(
              `aerogpu: COPY_TEXTURE2D writeback out of bounds for alloc ${backing.allocId} (end=${endInBacking}, allocBytes=${alloc.sizeBytes})`,
            );
          }

          writeback = { baseGpa: alloc.gpa + backing.offsetBytes };
        }

        for (let row = 0; row < height; row += 1) {
          const dstOff = ((dstY + row) * dstW + dstX) * 4;
          dstBytes.set(tmp.subarray(row * rowBytes, (row + 1) * rowBytes), dstOff);
        }

        if (writeback) {
          const dstLayout = dst.subresourceLayouts[dstSub]!;
          for (let row = 0; row < height; row += 1) {
            const dstBackingOff = dstLayout.offsetBytes + (dstY + row) * dstLayout.rowPitchBytes + dstX * 4;
            const tmpOff = row * rowBytes;
            const dstRowBytes = sliceGuestChecked(
              opts,
              writeback.baseGpa + dstBackingOff,
              rowBytes,
              `texture ${dstTexture} writeback`,
            );

            if (dst.format === AEROGPU_FORMAT_R8G8B8A8_UNORM || dst.format === AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB) {
              dstRowBytes.set(tmp.subarray(tmpOff, tmpOff + rowBytes));
              continue;
            }

            for (let i = 0; i < rowBytes; i += 4) {
              writeTexelFromRgba(dst.format, tmp, tmpOff + i, dstRowBytes, i);
            }
          }
        }
        break;
      }

      case AEROGPU_CMD_EXPORT_SHARED_SURFACE: {
        if (cmdSizeBytes < AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE) {
          throw new Error(`aerogpu: EXPORT_SHARED_SURFACE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const resourceHandle = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        const shareToken = readU64LeChecked(dv, offset + 16, end, "share_token");
        exportSharedSurface(state, resourceHandle, shareToken);
        break;
      }

      case AEROGPU_CMD_IMPORT_SHARED_SURFACE: {
        if (cmdSizeBytes < AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE) {
          throw new Error(`aerogpu: IMPORT_SHARED_SURFACE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const outResourceHandle = readU32LeChecked(dv, offset + 8, end, "out_resource_handle");
        const shareToken = readU64LeChecked(dv, offset + 16, end, "share_token");
        importSharedSurface(state, outResourceHandle, shareToken);
        break;
      }

      case AEROGPU_CMD_RELEASE_SHARED_SURFACE: {
        if (cmdSizeBytes < AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE) {
          throw new Error(`aerogpu: RELEASE_SHARED_SURFACE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const shareToken = readU64LeChecked(dv, offset + 8, end, "share_token");
        releaseSharedSurface(state, shareToken);
        break;
      }

      case AEROGPU_CMD_SET_RENDER_TARGETS: {
        if (cmdSizeBytes < AEROGPU_CMD_SET_RENDER_TARGETS_SIZE) {
          throw new Error(`aerogpu: SET_RENDER_TARGETS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const colorCount = readU32LeChecked(dv, offset + 8, end, "color_count");
        let rt0: number | null = null;
        if (colorCount > 0) {
          const rt0Raw = readU32LeChecked(dv, offset + 16, end, "colors[0]");
          rt0 = resolveSharedCmdHandle(state, rt0Raw, "SET_RENDER_TARGETS");
        }
        state.currentRenderTarget = rt0;
        break;
      }

      case AEROGPU_CMD_PRESENT: {
        if (cmdSizeBytes < AEROGPU_CMD_PRESENT_SIZE) {
          throw new Error(`aerogpu: PRESENT packet too small (size_bytes=${cmdSizeBytes})`);
        }
        state.presentCount += 1n;
        presentDelta += 1n;

        const rt = state.currentRenderTarget;
        if (rt != null && rt !== 0) {
          const resolvedRt = resolveSharedHandle(state, rt);
          const tex = state.textures.get(resolvedRt);
          if (!tex) {
            throw new Error(`aerogpu: PRESENT references missing render target handle ${rt} (resolved=${resolvedRt})`);
          }
          state.lastPresentedFrame = { width: tex.width, height: tex.height, rgba8: tex.data.slice().buffer };
          opts.presentTexture?.(tex);
        }
        break;
      }

      case AEROGPU_CMD_PRESENT_EX: {
        if (cmdSizeBytes < AEROGPU_CMD_PRESENT_EX_SIZE) {
          throw new Error(`aerogpu: PRESENT_EX packet too small (size_bytes=${cmdSizeBytes})`);
        }
        state.presentCount += 1n;
        presentDelta += 1n;

        const rt = state.currentRenderTarget;
        if (rt != null && rt !== 0) {
          const resolvedRt = resolveSharedHandle(state, rt);
          const tex = state.textures.get(resolvedRt);
          if (!tex) {
            throw new Error(`aerogpu: PRESENT_EX references missing render target handle ${rt} (resolved=${resolvedRt})`);
          }
          state.lastPresentedFrame = { width: tex.width, height: tex.height, rgba8: tex.data.slice().buffer };
          opts.presentTexture?.(tex);
        }
        break;
      }

      case AEROGPU_CMD_FLUSH: {
        if (cmdSizeBytes < AEROGPU_CMD_FLUSH_SIZE) {
          throw new Error(`aerogpu: FLUSH packet too small (size_bytes=${cmdSizeBytes})`);
        }
        // `FLUSH` exists mainly to model D3D9Ex semantics. For the lightweight browser CPU
        // executor (used for protocol tests + simple copy/present flows) it is a no-op.
        break;
      }

      case AEROGPU_CMD_DISPATCH: {
        if (cmdSizeBytes < AEROGPU_CMD_DISPATCH_SIZE) {
          throw new Error(`aerogpu: DISPATCH packet too small (size_bytes=${cmdSizeBytes})`);
        }
        // Compute is not implemented by the CPU executor; treat as a validated no-op.
        break;
      }

      default:
        // Unknown opcodes are skipped (forward-compat).
        break;
    }
  }

  return presentDelta;
};
