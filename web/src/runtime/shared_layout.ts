import { RingBuffer } from "./ring_buffer";
import { requiredFramebufferBytes } from "../display/framebuffer_protocol";
import { createIpcBuffer } from "../ipc/ipc";

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

  // I/O worker input telemetry (optional; used by tests and perf instrumentation).
  IoInputBatchCounter = 2,
  IoInputEventCounter = 3,

  // Audio telemetry (producer-side). Values are updated by the worker producing
  // audio for the AudioWorklet ring buffer.
  AudioBufferLevelFrames = 4,
  AudioUnderrunCount = 5,
  AudioOverrunCount = 6,

  CpuReady = 8,
  GpuReady = 9,
  IoReady = 10,
  JitReady = 11,

  /**
   * Guest RAM layout contract (in bytes, stored as u32).
   *
   * `guest_base` is the byte offset within `WebAssembly.Memory` where guest
   * physical address 0 begins.
   *
   * This is written once by the coordinator when allocating the shared memory
   * segments and then treated as immutable for the life of that VM instance.
   */
  GuestBase = 16,
  GuestSize = 17,
  RuntimeReserved = 18,
}

export const COMMAND_RING_CAPACITY_BYTES = 32 * 1024;
export const EVENT_RING_CAPACITY_BYTES = 32 * 1024;

// CPU<->I/O AIPC queues used for high-frequency device operations (disk I/O, etc).
//
// These queues use the AIPC layout/protocol defined in `web/src/ipc` /
// `crates/aero-ipc` and are separate from the runtime START/STOP rings.
export const IO_IPC_CMD_QUEUE_KIND = 0;
export const IO_IPC_EVT_QUEUE_KIND = 1;
export const IO_IPC_RING_CAPACITY_BYTES = 32 * 1024;

/**
 * Guest memory placeholder.
 *
 * In the real emulator this is a shared WebAssembly.Memory (wasm32) so it can be
 * accessed directly from WASM code across worker threads.
 *
 * Note on sizing:
 * - Early architecture drafts described allocating a single 5+ GiB shared region
 *   (guest RAM + queues + metadata). In practice, wasm32 linear memory is limited
 *   to 2^32 bytes (~4 GiB), so that monolithic layout is not implementable today.
 * - Without `memory64`, wasm32 linear memory is limited to 2^32 bytes (~4 GiB),
 *   i.e. 65,536 64KiB pages.
 *
 * Therefore we deliberately use multiple shared memory segments:
 * - `control`: a small SharedArrayBuffer for runtime IPC (status + START/STOP rings).
 * - `ioIpc`: a SharedArrayBuffer carrying the high-frequency AIPC command/event queues
 *   (disk I/O, device access, etc).
 * - `guestMemory`: a shared WebAssembly.Memory for guest RAM.
 * - `vgaFramebuffer`: a shared framebuffer region for early VGA/VBE display.
 *
 * This keeps IPC cache-friendly and avoids tying worker bring-up to a massive
 * monolithic allocation.
 *
 * We intentionally keep control IPC memory separate from guest RAM so that:
 *  - The coordinator can still run even if a large guest allocation fails.
 *  - IPC buffers remain small and cache-friendly.
 *  - Guest RAM can be resized/failed independently of IPC buffers.
 */
export const GUEST_RAM_PRESETS_MIB = [256, 512, 1024, 2048, 3072, 4096] as const;
export type GuestRamMiB = number;
export const DEFAULT_GUEST_RAM_MIB: GuestRamMiB = 512;

const WASM_PAGE_BYTES = 64 * 1024;
const MAX_WASM32_PAGES = 65536;

/**
 * Fixed low-address region reserved for the Rust/WASM runtime (stack, heap,
 * static data, wasm-bindgen metadata, etc.).
 *
 * Guest RAM starts at `guest_base = align_up(RUNTIME_RESERVED_BYTES, 64KiB)`.
 *
 * NOTE: Keep this in sync with the WASM-exported `guest_ram_layout` contract
 * in `crates/aero-wasm/src/lib.rs`.
 */
export const RUNTIME_RESERVED_BYTES = 64 * 1024 * 1024; // 64 MiB

export interface GuestRamLayout {
  /**
   * Byte offset into wasm linear memory where guest physical address 0 maps.
   */
  guest_base: number;
  /**
   * Usable guest bytes (may be clamped to fit wasm32's 4GiB limit).
   */
  guest_size: number;
  /**
   * Bytes reserved for runtime (always equals `guest_base`).
   */
  runtime_reserved: number;
  /**
   * Total wasm pages (64KiB) allocated for the memory.
   */
  wasm_pages: number;
}

// Early VGA/VBE framebuffer sizing. The buffer is reused across mode changes;
// the active dimensions live in the framebuffer header.
const VGA_FRAMEBUFFER_MAX_WIDTH = 1024;
const VGA_FRAMEBUFFER_MAX_HEIGHT = 768;

function mibToBytes(mib: number): number {
  return mib * 1024 * 1024;
}

function bytesToPages(bytes: number): number {
  return Math.ceil(bytes / WASM_PAGE_BYTES);
}

export interface SharedMemorySegments {
  control: SharedArrayBuffer;
  guestMemory: WebAssembly.Memory;
  vgaFramebuffer: SharedArrayBuffer;
  ioIpc: SharedArrayBuffer;
}

export interface RingRegions {
  command: { byteOffset: number; byteLength: number };
  event: { byteOffset: number; byteLength: number };
}

export interface SharedMemoryViews {
  segments: SharedMemorySegments;
  status: Int32Array;
  guestLayout: GuestRamLayout;
  guestU8: Uint8Array;
  guestI32: Int32Array;
  vgaFramebuffer: SharedArrayBuffer;
}

function align(value: number, alignment: number): number {
  if (alignment <= 0) return value;
  return Math.ceil(value / alignment) * alignment;
}

function clampToU32(value: number): number {
  // JS numbers are IEEE754 doubles; we only need a best-effort clamp for inputs
  // coming from user configuration (MiB presets).
  if (!Number.isFinite(value) || value <= 0) return 0;
  // 2^32 is still a safe integer.
  return Math.min(value, 0xffffffff);
}

function toU32(value: number): number {
  if (!Number.isFinite(value)) {
    throw new RangeError(`Expected a finite u32, got ${String(value)}`);
  }
  const int = Math.trunc(value);
  if (int < 0 || int > 0xffffffff) {
    throw new RangeError(`Expected a u32 in [0, 2^32), got ${String(value)}`);
  }
  return int >>> 0;
}

export function computeGuestRamLayout(desiredGuestBytes: number): GuestRamLayout {
  const desired = clampToU32(desiredGuestBytes);
  const guestBase = align(RUNTIME_RESERVED_BYTES, WASM_PAGE_BYTES);
  const runtimeReserved = guestBase;

  const basePages = Math.floor(guestBase / WASM_PAGE_BYTES);
  const desiredPages = bytesToPages(desired);
  const totalPages = Math.min(MAX_WASM32_PAGES, basePages + desiredPages);
  const guestPages = Math.max(0, totalPages - basePages);

  return {
    guest_base: guestBase,
    guest_size: guestPages * WASM_PAGE_BYTES,
    runtime_reserved: runtimeReserved,
    wasm_pages: totalPages,
  };
}

export function guestToLinear(layout: GuestRamLayout, paddr: number): number {
  const addr = toU32(paddr);
  if (addr >= layout.guest_size) {
    throw new RangeError(`Guest physical address out of bounds: 0x${addr.toString(16)} (guest_size=${layout.guest_size})`);
  }
  return layout.guest_base + addr;
}

export function guestRangeInBounds(layout: GuestRamLayout, paddr: number, byteLength: number): boolean {
  const addr = toU32(paddr);
  const len = toU32(byteLength);
  if (len === 0) return addr <= layout.guest_size;
  return addr < layout.guest_size && addr + len <= layout.guest_size;
}

export function readGuestRamLayoutFromStatus(status: Int32Array): GuestRamLayout {
  const guestBase = Atomics.load(status, StatusIndex.GuestBase) >>> 0;
  const guestSize = Atomics.load(status, StatusIndex.GuestSize) >>> 0;
  const runtimeReserved = Atomics.load(status, StatusIndex.RuntimeReserved) >>> 0;

  if (guestBase === 0 && guestSize === 0) {
    throw new Error("Guest RAM layout was not initialized in status SAB (guest_base/guest_size are 0).");
  }

  // wasm_pages isn't stored; infer from guest size + base.
  const totalBytes = guestBase + guestSize;
  const totalPages = bytesToPages(totalBytes);
  return { guest_base: guestBase, guest_size: guestSize, runtime_reserved: runtimeReserved || guestBase, wasm_pages: totalPages };
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
  const desiredGuestBytes = mibToBytes(guestRamMiB);
  const layout = computeGuestRamLayout(desiredGuestBytes);
  const pages = layout.wasm_pages;

  let guestMemory: WebAssembly.Memory;
  try {
    guestMemory = new WebAssembly.Memory({ initial: pages, maximum: MAX_WASM32_PAGES, shared: true });
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

  const control = new SharedArrayBuffer(CONTROL_BYTES);
  const status = new Int32Array(control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  Atomics.store(status, StatusIndex.GuestBase, layout.guest_base | 0);
  Atomics.store(status, StatusIndex.GuestSize, layout.guest_size | 0);
  Atomics.store(status, StatusIndex.RuntimeReserved, layout.runtime_reserved | 0);

  return {
    control,
    guestMemory,
    // A single shared RGBA8888 framebuffer region used for early VGA/VBE display.
    // This is sized for a modest SVGA mode; actual modes are communicated via the
    // framebuffer protocol header.
    vgaFramebuffer: new SharedArrayBuffer(
      requiredFramebufferBytes(VGA_FRAMEBUFFER_MAX_WIDTH, VGA_FRAMEBUFFER_MAX_HEIGHT, VGA_FRAMEBUFFER_MAX_WIDTH * 4),
    ),
    ioIpc: createIpcBuffer([
      { kind: IO_IPC_CMD_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
      { kind: IO_IPC_EVT_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
    ]).buffer,
  };
}

export function createSharedMemoryViews(segments: SharedMemorySegments): SharedMemoryViews {
  const status = new Int32Array(segments.control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  const guestLayout = readGuestRamLayoutFromStatus(status);

  const memBytes = segments.guestMemory.buffer.byteLength;
  if (guestLayout.guest_base + guestLayout.guest_size > memBytes) {
    throw new Error(
      `Guest RAM layout (${guestLayout.guest_base}+${guestLayout.guest_size}) exceeds wasm memory size (${memBytes}).`,
    );
  }

  const guestU8 = new Uint8Array(segments.guestMemory.buffer, guestLayout.guest_base, guestLayout.guest_size);
  const guestI32 = new Int32Array(
    segments.guestMemory.buffer,
    guestLayout.guest_base,
    Math.floor(guestLayout.guest_size / Int32Array.BYTES_PER_ELEMENT),
  );
  return { segments, status, guestLayout, guestU8, guestI32, vgaFramebuffer: segments.vgaFramebuffer };
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
    const mem = new WebAssembly.Memory({ initial: 1, maximum: MAX_WASM32_PAGES, shared: true });
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
