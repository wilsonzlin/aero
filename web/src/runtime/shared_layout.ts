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
 * In the real emulator this will likely become a shared WebAssembly.Memory
 * (backed by a SharedArrayBuffer) and be sized up to ~4GiB. The architecture
 * docs mention allocating "5+ GB", but without `memory64` most WebAssembly
 * tooling/browsers are constrained to a 32-bit linear memory index (~4GiB).
 *
 * We intentionally keep control IPC memory separate from guest RAM so that:
 *  - The coordinator can run even if a giant guest allocation fails.
 *  - IPC buffers remain small and cache-friendly.
 *  - We can swap guest RAM implementations (SAB vs WASM memory) without
 *    changing IPC offsets.
 */
export const DEFAULT_GUEST_MEMORY_BYTES = 16 * 1024 * 1024;

export interface SharedMemorySegments {
  control: SharedArrayBuffer;
  guest: SharedArrayBuffer;
}

export interface RingRegions {
  command: { byteOffset: number; byteLength: number };
  event: { byteOffset: number; byteLength: number };
}

export interface SharedMemoryViews {
  segments: SharedMemorySegments;
  status: Int32Array;
  guestU8: Uint8Array;
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
  guestBytes?: number;
}): SharedMemorySegments {
  const guestBytes = options?.guestBytes ?? DEFAULT_GUEST_MEMORY_BYTES;
  return {
    control: new SharedArrayBuffer(CONTROL_BYTES),
    guest: new SharedArrayBuffer(guestBytes),
  };
}

export function createSharedMemoryViews(segments: SharedMemorySegments): SharedMemoryViews {
  const status = new Int32Array(segments.control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  const guestU8 = new Uint8Array(segments.guest);
  return { segments, status, guestU8 };
}

export function checkSharedMemorySupport(): { ok: boolean; reason?: string } {
  if (typeof SharedArrayBuffer === "undefined") {
    return {
      ok: false,
      reason:
        "SharedArrayBuffer is unavailable. Ensure the page is crossOriginIsolated (COOP/COEP headers).",
    };
  }
  if (typeof crossOriginIsolated !== "undefined" && !crossOriginIsolated) {
    return {
      ok: false,
      reason:
        "crossOriginIsolated is false. The server must send COOP/COEP headers for SharedArrayBuffer + Atomics.",
    };
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

