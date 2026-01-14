import { VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID } from "./vm_snapshot_wasm";

/**
 * Reserved `device.<id>` base for legacy IO worker BAR1 VRAM snapshot chunks.
 *
 * Older web builds snapshotted the SharedArrayBuffer-backed BAR1 mapping (WDDM scanout surfaces +
 * AeroGPU allocations) as a series of opaque device blobs so the bytes could be streamed directly
 * to/from OPFS by the IO worker without transferring 64–128MiB through the coordinator.
 *
 * Newer builds snapshot BAR1 VRAM via the canonical `gpu.vram` device blobs instead; this reserved
 * ID range is kept only for restore compatibility.
 *
 * Chunk `i` is stored under:
 *   `device.${IO_WORKER_VRAM_SNAPSHOT_DEVICE_ID_BASE + i}`
 */
export const IO_WORKER_VRAM_SNAPSHOT_DEVICE_ID_BASE = 1_000_000_001;

/**
 * Snapshot format limit: `aero_snapshot::limits::MAX_DEVICE_ENTRY_LEN`.
 *
 * Current max VRAM (128MiB) therefore requires up to 2 chunks.
 */
export const IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES = 64 * 1024 * 1024;

export function ioWorkerVramSnapshotChunkIndexFromDeviceId(id: number): number | null {
  const idU32 = id >>> 0;
  if (idU32 < IO_WORKER_VRAM_SNAPSHOT_DEVICE_ID_BASE) return null;
  // Limit chunk indices to a reasonable range so we don't accidentally interpret unrelated unknown
  // device IDs as VRAM. (The range is reserved, but still keep parsing bounded.)
  const idx = idU32 - IO_WORKER_VRAM_SNAPSHOT_DEVICE_ID_BASE;
  if (idx < 0 || idx > 1024) return null;
  return idx >>> 0;
}

function parseDeviceKindId(kind: string): number | null {
  if (!kind.startsWith(VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID)) return null;
  const rest = kind.slice(VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID.length);
  if (!/^[0-9]+$/.test(rest)) return null;
  const parsed = Number(rest);
  if (!Number.isSafeInteger(parsed) || parsed < 0 || parsed > 0xffff_ffff) return null;
  return parsed >>> 0;
}

export function ioWorkerVramSnapshotChunkIndexFromDeviceKind(kind: string): number | null {
  const id = parseDeviceKindId(kind);
  if (id === null) return null;
  return ioWorkerVramSnapshotChunkIndexFromDeviceId(id);
}
