import { RingBuffer } from "./ring_buffer";

export const WORKER_ROLES = ["cpu", "gpu", "io", "jit"] as const;
export type WorkerRole = (typeof WORKER_ROLES)[number];

/**
 * A small, fixed control/status region used for flags and counters.
 *
 * This stays intentionally tiny; large regions (guest RAM, framebuffers, etc.)
 * will live in separate buffers over time.
 */
export const STATUS_INTS = 64;
export const STATUS_BYTES = STATUS_INTS * 4;

export enum StatusIndex {
  HeartbeatCounter = 0,
  StopRequested = 1,

  CpuReady = 8,
  GpuReady = 9,
  IoReady = 10,
  JitReady = 11,
}

export const COMMAND_RING_CAPACITY_BYTES = 32 * 1024;
export const EVENT_RING_CAPACITY_BYTES = 32 * 1024;

/**
 * Guest memory placeholder.
 *
 * In the real emulator this is a shared WebAssembly.Memory (wasm32) so it can be
 * accessed directly from WASM code across worker threads.
 *
 * We intentionally keep control IPC memory separate from guest RAM so that:
 *  - The coordinator can still run even if a large guest allocation fails.
 *  - IPC buffers remain small and cache-friendly.
 *  - Guest RAM can be resized/failed independently of IPC buffers.
 */
export const GUEST_RAM_PRESETS_MIB = [512, 1024, 2048, 3072] as const;
export type GuestRamMiB = (typeof GUEST_RAM_PRESETS_MIB)[number];
export const DEFAULT_GUEST_RAM_MIB: GuestRamMiB = 512;

const WASM_PAGE_BYTES = 64 * 1024;

function mibToBytes(mib: number): number {
  return mib * 1024 * 1024;
}

function bytesToPages(bytes: number): number {
  return Math.ceil(bytes / WASM_PAGE_BYTES);
}

export interface SharedMemorySegments {
  control: SharedArrayBuffer;
  guestMemory: WebAssembly.Memory;
}

export interface RingRegions {
  command: { byteOffset: number; byteLength: number };
  event: { byteOffset: number; byteLength: number };
}

export interface SharedMemoryViews {
  segments: SharedMemorySegments;
  status: Int32Array;
  guestU8: Uint8Array;
  guestI32: Int32Array;
}

function align(value: number, alignment: number): number {
  const mask = alignment - 1;
  return (value + mask) & ~mask;
}

const RING_REGION_BYTES = RingBuffer.byteLengthForCapacity(COMMAND_RING_CAPACITY_BYTES);
const EVENT_RING_REGION_BYTES = RingBuffer.byteLengthForCapacity(EVENT_RING_CAPACITY_BYTES);

const CONTROL_LAYOUT = (() => {
  let offset = 0;
  const statusOffset = offset;
  offset += STATUS_BYTES;
  offset = align(offset, 64);

  const rings: Record<WorkerRole, RingRegions> = {
    cpu: { command: { byteOffset: 0, byteLength: 0 }, event: { byteOffset: 0, byteLength: 0 } },
    gpu: { command: { byteOffset: 0, byteLength: 0 }, event: { byteOffset: 0, byteLength: 0 } },
    io: { command: { byteOffset: 0, byteLength: 0 }, event: { byteOffset: 0, byteLength: 0 } },
    jit: { command: { byteOffset: 0, byteLength: 0 }, event: { byteOffset: 0, byteLength: 0 } },
  };

  for (const role of WORKER_ROLES) {
    rings[role].command = { byteOffset: offset, byteLength: RING_REGION_BYTES };
    offset += RING_REGION_BYTES;
    offset = align(offset, 64);

    rings[role].event = { byteOffset: offset, byteLength: EVENT_RING_REGION_BYTES };
    offset += EVENT_RING_REGION_BYTES;
    offset = align(offset, 64);
  }

  const controlBytes = offset;
  return { controlBytes, statusOffset, rings };
})();

export const CONTROL_BYTES = CONTROL_LAYOUT.controlBytes;

export function ringRegionsForWorker(role: WorkerRole): RingRegions {
  return CONTROL_LAYOUT.rings[role];
}

export function allocateSharedMemorySegments(options?: {
  guestRamMiB?: GuestRamMiB;
}): SharedMemorySegments {
  const guestRamMiB = options?.guestRamMiB ?? DEFAULT_GUEST_RAM_MIB;
  const guestBytes = mibToBytes(guestRamMiB);
  const pages = bytesToPages(guestBytes);
  if (pages > 65536) {
    throw new Error(`guestRamMiB too large for wasm32: ${guestRamMiB} MiB (${pages} pages)`);
  }

  let guestMemory: WebAssembly.Memory;
  try {
    guestMemory = new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(
      `Failed to allocate shared WebAssembly.Memory for guest RAM (${guestRamMiB} MiB). Try a smaller size. Error: ${msg}`,
    );
  }

  if (!(guestMemory.buffer instanceof SharedArrayBuffer)) {
    throw new Error(
      "Shared WebAssembly.Memory is unavailable (memory.buffer is not a SharedArrayBuffer). " +
        "Ensure COOP/COEP headers are set and the browser supports WASM threads.",
    );
  }

  return {
    control: new SharedArrayBuffer(CONTROL_BYTES),
    guestMemory,
  };
}

export function createSharedMemoryViews(segments: SharedMemorySegments): SharedMemoryViews {
  const status = new Int32Array(segments.control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  const guestU8 = new Uint8Array(segments.guestMemory.buffer);
  const guestI32 = new Int32Array(segments.guestMemory.buffer);
  return { segments, status, guestU8, guestI32 };
}

export function checkSharedMemorySupport(): { ok: boolean; reason?: string } {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") {
    return { ok: false, reason: "WebAssembly is unavailable in this environment." };
  }
  if (typeof globalThis.isSecureContext !== "undefined" && !globalThis.isSecureContext) {
    return { ok: false, reason: "SharedArrayBuffer requires a secure context (https:// or localhost)." };
  }
  if (typeof SharedArrayBuffer === "undefined") {
    return {
      ok: false,
      reason:
        "SharedArrayBuffer is unavailable. Ensure the page is crossOriginIsolated (COOP/COEP headers).",
    };
  }
  if (typeof Atomics === "undefined") {
    return { ok: false, reason: "Atomics is unavailable. WASM threads require Atomics." };
  }
  if (typeof crossOriginIsolated !== "undefined" && !crossOriginIsolated) {
    return {
      ok: false,
      reason:
        "crossOriginIsolated is false. The server must send COOP/COEP headers for SharedArrayBuffer + Atomics.",
    };
  }

  try {
    const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    if (!(mem.buffer instanceof SharedArrayBuffer)) {
      return { ok: false, reason: "Shared WebAssembly.Memory is unsupported in this browser configuration." };
    }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    return { ok: false, reason: `Shared WebAssembly.Memory is unavailable: ${msg}` };
  }

  return { ok: true };
}

export function setReadyFlag(status: Int32Array, role: WorkerRole, ready: boolean): void {
  const idx =
    role === "cpu"
      ? StatusIndex.CpuReady
      : role === "gpu"
        ? StatusIndex.GpuReady
        : role === "io"
          ? StatusIndex.IoReady
          : StatusIndex.JitReady;
  Atomics.store(status, idx, ready ? 1 : 0);
}
