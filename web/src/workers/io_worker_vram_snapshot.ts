import { VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID } from "./vm_snapshot_wasm";

/**
 * Reserved `device.<id>` base for IO worker BAR1 VRAM snapshot chunks.
 *
 * The web runtime snapshots the SharedArrayBuffer-backed BAR1 mapping (WDDM scanout surfaces +
 * AeroGPU allocations) as a series of opaque device blobs so the bytes can be streamed directly
 * to/from OPFS by the IO worker without transferring 64–128MiB through the coordinator.
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

export function ioWorkerVramSnapshotDeviceKindForChunkIndex(chunkIndex: number): string {
  const idx = chunkIndex >>> 0;
  return `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}${(IO_WORKER_VRAM_SNAPSHOT_DEVICE_ID_BASE + idx) >>> 0}`;
}

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

export function appendIoWorkerVramSnapshotDeviceBlobs(
  devices: Array<{ kind: string; bytes: Uint8Array }>,
  vramU8: Uint8Array,
  opts?: {
    /**
     * Optional override used by unit tests to exercise chunking without allocating 64MiB arrays.
     *
     * Must be `>0` and `<= IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES`.
     */
    chunkBytes?: number;
  },
): void {
  if (!devices) return;
  if (!(vramU8 instanceof Uint8Array)) return;
  const total = vramU8.byteLength >>> 0;
  if (total === 0) return;

  let chunkBytes = (opts?.chunkBytes ?? IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES) >>> 0;
  if (!Number.isFinite(chunkBytes) || chunkBytes <= 0) {
    chunkBytes = IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES;
  }
  if (chunkBytes > IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES) {
    console.warn(
      `[io.worker] VRAM snapshot chunkBytes=${chunkBytes} exceeds MAX_DEVICE_ENTRY_LEN; clamping to ${IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES}`,
    );
    chunkBytes = IO_WORKER_VRAM_SNAPSHOT_MAX_CHUNK_BYTES;
  }

  let off = 0;
  let chunkIndex = 0;
  while (off < total) {
    const end = Math.min(off + chunkBytes, total);
    devices.push({
      kind: ioWorkerVramSnapshotDeviceKindForChunkIndex(chunkIndex),
      // Must be a view into the SharedArrayBuffer-backed VRAM mapping (no extra copy).
      bytes: vramU8.subarray(off, end),
    });
    off = end;
    chunkIndex++;
  }
}

