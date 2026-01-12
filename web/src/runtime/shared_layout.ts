import { RECORD_ALIGN, queueKind, ringCtrl } from "../ipc/layout";
import { requiredFramebufferBytes } from "../display/framebuffer_protocol";
import { createIpcBuffer } from "../ipc/ipc";
import { PCI_MMIO_BASE } from "../arch/guest_phys.ts";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
} from "../ipc/shared-layout";

export const WORKER_ROLES = ["cpu", "gpu", "io", "jit", "net"] as const;
export type WorkerRole = (typeof WORKER_ROLES)[number];

/**
 * A small, fixed control/status region used for flags and counters.
 *
 * This stays intentionally tiny; large regions (guest RAM, framebuffers, etc.)
 * will live in separate buffers over time.
 */
export const STATUS_INTS = 64;
export const STATUS_BYTES = STATUS_INTS * 4;

export const StatusIndex = {
  HeartbeatCounter: 0,
  StopRequested: 1,

  // I/O worker input telemetry (optional; used by tests and perf instrumentation).
  IoInputBatchCounter: 2,
  IoInputEventCounter: 3,

  // Audio telemetry (producer-side). Owned by the active audio producer:
  // - CPU worker (demo tone / mic loopback)
  // - I/O worker (guest HDA device during real VM runs)
  //
  // Counters are expressed in frames and stored as wrapping u32 values.
  AudioBufferLevelFrames: 4,
  AudioUnderrunCount: 5,
  AudioOverrunCount: 6,

  CpuReady: 8,
  GpuReady: 9,
  IoReady: 10,
  JitReady: 11,
  NetReady: 15,

  // I/O worker HID passthrough telemetry.
  IoHidAttachCounter: 12,
  IoHidDetachCounter: 13,
  IoHidInputReportCounter: 14,
  IoHidInputReportDropCounter: 19,

  // Device-bus state observed by the CPU worker.
  //
  // IRQ bitmaps represent *line levels* (asserted/deasserted) after wire-OR; they are useful for
  // debugging and simple polling, but do not encode edge-triggered interrupts directly (edge
  // sources are represented as pulses and must be latched by the PIC/APIC model).
  //
  // See `docs/irq-semantics.md`.
  CpuIrqBitmapLo: 32,
  CpuIrqBitmapHi: 33,
  CpuA20Enabled: 34,

  /**
   * Guest RAM layout contract (in bytes, stored as u32).
   *
   * `guest_base` is the byte offset within `WebAssembly.Memory` where guest
   * physical address 0 begins.
   *
   * This is written once by the coordinator when allocating the shared memory
   * segments and then treated as immutable for the life of that VM instance.
   */
  GuestBase: 16,
  GuestSize: 17,
  RuntimeReserved: 18,
} as const;

export type StatusIndex = (typeof StatusIndex)[keyof typeof StatusIndex];

export const COMMAND_RING_CAPACITY_BYTES = 32 * 1024;
export const EVENT_RING_CAPACITY_BYTES = 32 * 1024;

// CPU<->I/O AIPC queues used for high-frequency device operations (disk I/O, etc).
//
// These queues use the AIPC layout/protocol defined in `web/src/ipc` /
// `crates/aero-ipc` and are separate from the runtime START/STOP rings.
export const IO_IPC_CMD_QUEUE_KIND = queueKind.CMD;
export const IO_IPC_EVT_QUEUE_KIND = queueKind.EVT;
export const IO_IPC_RING_CAPACITY_BYTES = 32 * 1024;

// Raw Ethernet frame transport (guest <-> host) for the Option C L2 tunnel.
//
// These are separate from the command/event queues so bulk frame traffic does not
// starve low-latency device operations.
export const IO_IPC_NET_TX_QUEUE_KIND = queueKind.NET_TX;
export const IO_IPC_NET_RX_QUEUE_KIND = queueKind.NET_RX;
export const IO_IPC_NET_RING_CAPACITY_BYTES = 512 * 1024;

// WebHID input report forwarding (main thread -> I/O worker).
export const IO_IPC_HID_IN_QUEUE_KIND = queueKind.HID_IN;
export const IO_IPC_HID_IN_RING_CAPACITY_BYTES = 1024 * 1024;

export function createIoIpcSab(): SharedArrayBuffer {
  return createIpcBuffer([
    { kind: IO_IPC_CMD_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
    { kind: IO_IPC_EVT_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
    { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: IO_IPC_NET_RING_CAPACITY_BYTES },
    { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: IO_IPC_NET_RING_CAPACITY_BYTES },
    { kind: IO_IPC_HID_IN_QUEUE_KIND, capacityBytes: IO_IPC_HID_IN_RING_CAPACITY_BYTES },
  ]).buffer;
}

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
 * - `ioIpc`: a SharedArrayBuffer carrying high-frequency AIPC queues
 *   (device command/event traffic, raw Ethernet frames, etc).
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
export const GUEST_RAM_PRESETS_MIB = [256, 512, 1024, 2048, 3072, 3584] as const;
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
export const RUNTIME_RESERVED_BYTES = 128 * 1024 * 1024; // 128 MiB

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

// -----------------------------------------------------------------------------
// Demo guest-memory layout (temporary)
// -----------------------------------------------------------------------------

// Offset (in bytes) of a shared counter incremented by the CPU worker demo.
//
// Note: keep this non-zero; in Rust/WASM a raw pointer of 0 is a null pointer
// and must not be dereferenced.
//
// This is expressed as a guest-physical offset; to convert to a wasm linear
// address, add `guest_base` from [`GuestRamLayout`].
export const CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES = 0x200;
export const CPU_WORKER_DEMO_GUEST_COUNTER_INDEX = CPU_WORKER_DEMO_GUEST_COUNTER_OFFSET_BYTES / 4;

// Offset (in bytes) of the shared framebuffer used by the CPU worker demo.
//
// This is expressed as a guest-physical offset; to convert to a wasm linear
// address, add `guest_base` from [`GuestRamLayout`].
export const CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES = 0x20_0000; // 2 MiB (64-byte aligned)
export const CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH = 640;
export const CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT = 480;
export const CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE = 32;

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
  sharedFramebuffer: SharedArrayBuffer;
  /**
   * Byte offset within `sharedFramebuffer` where the header begins.
   *
   * Note: `sharedFramebuffer` may alias `guestMemory.buffer` (embedded in WASM
   * linear memory), so this is not necessarily 0.
   */
  sharedFramebufferOffsetBytes: number;
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
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes: number;
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
  const maxGuestPagesByWasm = Math.max(0, MAX_WASM32_PAGES - basePages);
  const maxGuestBytesByWasm = maxGuestPagesByWasm * WASM_PAGE_BYTES;
  const maxGuestBytes = Math.min(maxGuestBytesByWasm, PCI_MMIO_BASE);

  const desiredPages = bytesToPages(Math.min(desired, maxGuestBytes));
  const totalPages = basePages + desiredPages;
  const guestPages = desiredPages;

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
  if (guestSize > PCI_MMIO_BASE) {
    throw new Error(
      `Guest RAM overlaps the PCI MMIO aperture: guest_size=0x${guestSize.toString(16)} PCI_MMIO_BASE=0x${PCI_MMIO_BASE.toString(16)}`,
    );
  }

  // wasm_pages isn't stored; infer from guest size + base.
  const totalBytes = guestBase + guestSize;
  const totalPages = bytesToPages(totalBytes);
  return { guest_base: guestBase, guest_size: guestSize, runtime_reserved: runtimeReserved || guestBase, wasm_pages: totalPages };
}

const RING_REGION_BYTES = ringCtrl.BYTES + COMMAND_RING_CAPACITY_BYTES;
const EVENT_RING_REGION_BYTES = ringCtrl.BYTES + EVENT_RING_CAPACITY_BYTES;

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
    net: { command: { byteOffset: 0, byteLength: 0 }, event: { byteOffset: 0, byteLength: 0 } },
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
    // Keep the guest memory fixed-size for the life of the VM. Growing a shared
    // `WebAssembly.Memory` can replace the underlying SharedArrayBuffer in some
    // runtimes, which would invalidate existing typed array views and break the
    // shared-memory contract across workers.
    guestMemory = new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(
      `Failed to allocate shared WebAssembly.Memory for guest RAM (${guestRamMiB} MiB). Try a smaller size. Error: ${msg}`,
    );
  }

  // `WebAssembly.Memory.buffer` is typed as `ArrayBuffer` in lib.dom.d.ts even for
  // `shared: true` memories. Cast to `ArrayBufferLike` so the runtime guard can
  // narrow correctly without TypeScript concluding the branch is unreachable.
  const guestBuffer = guestMemory.buffer as unknown as ArrayBufferLike;
  if (!(guestBuffer instanceof SharedArrayBuffer)) {
    throw new Error(
      "Shared WebAssembly.Memory is unavailable (memory.buffer is not a SharedArrayBuffer). " +
        "Ensure COOP/COEP headers are set and the browser supports WASM threads.",
    );
  }
  const guestSab = guestBuffer;

  const control = new SharedArrayBuffer(CONTROL_BYTES);
  const status = new Int32Array(control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  Atomics.store(status, StatusIndex.GuestBase, layout.guest_base | 0);
  Atomics.store(status, StatusIndex.GuestSize, layout.guest_size | 0);
  Atomics.store(status, StatusIndex.RuntimeReserved, layout.runtime_reserved | 0);
  initControlRings(control);

  // Shared CPU→GPU framebuffer demo region.
  //
  // Prefer embedding directly in the shared guest `WebAssembly.Memory` so the
  // CPU worker's WASM code can write/publish frames without JS-side copies. When
  // the configured guest RAM is too small (e.g. unit tests using ~1MiB guest
  // memory), fall back to a standalone SharedArrayBuffer.
  const sharedFramebufferLayout = computeSharedFramebufferLayout(
    CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
    CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
    CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH * 4,
    FramebufferFormat.RGBA8,
    CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
  );
  const embeddedOffsetBytes = layout.guest_base + CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES;
  const embeddedRequiredBytes = embeddedOffsetBytes + sharedFramebufferLayout.totalBytes;

  const sharedFramebufferEmbedded = embeddedRequiredBytes <= guestSab.byteLength;

  const sharedFramebuffer = sharedFramebufferEmbedded ? guestSab : new SharedArrayBuffer(sharedFramebufferLayout.totalBytes);
  const sharedFramebufferOffsetBytes = sharedFramebufferEmbedded ? embeddedOffsetBytes : 0;
  const sharedHeader = new Int32Array(sharedFramebuffer, sharedFramebufferOffsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.WIDTH, sharedFramebufferLayout.width);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.HEIGHT, sharedFramebufferLayout.height);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.STRIDE_BYTES, sharedFramebufferLayout.strideBytes);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FORMAT, sharedFramebufferLayout.format);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILE_SIZE, sharedFramebufferLayout.tileSize);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILES_X, sharedFramebufferLayout.tilesX);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILES_Y, sharedFramebufferLayout.tilesY);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, sharedFramebufferLayout.dirtyWordsPerBuffer);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FLAGS, 0);

  return {
    control,
    guestMemory,
    // A single shared RGBA8888 framebuffer region used for early VGA/VBE display.
    // This is sized for a modest SVGA mode; actual modes are communicated via the
    // framebuffer protocol header.
    vgaFramebuffer: new SharedArrayBuffer(
      requiredFramebufferBytes(VGA_FRAMEBUFFER_MAX_WIDTH, VGA_FRAMEBUFFER_MAX_HEIGHT, VGA_FRAMEBUFFER_MAX_WIDTH * 4),
    ),
    ioIpc: createIoIpcSab(),
    sharedFramebuffer,
    sharedFramebufferOffsetBytes,
  };
}

function initControlRings(control: SharedArrayBuffer): void {
  if (COMMAND_RING_CAPACITY_BYTES % RECORD_ALIGN !== 0) {
    throw new Error(`COMMAND_RING_CAPACITY_BYTES must be aligned to ${RECORD_ALIGN}`);
  }
  if (EVENT_RING_CAPACITY_BYTES % RECORD_ALIGN !== 0) {
    throw new Error(`EVENT_RING_CAPACITY_BYTES must be aligned to ${RECORD_ALIGN}`);
  }

  for (const role of WORKER_ROLES) {
    const regions = ringRegionsForWorker(role);
    initRing(control, regions.command.byteOffset, regions.command.byteLength);
    initRing(control, regions.event.byteOffset, regions.event.byteLength);
  }
}

function initRing(control: SharedArrayBuffer, byteOffset: number, byteLength: number): void {
  const capacityBytes = byteLength - ringCtrl.BYTES;
  if (capacityBytes < 0) {
    throw new Error("ring region too small");
  }
  if (capacityBytes % RECORD_ALIGN !== 0) {
    throw new Error(`ring capacity must be aligned to ${RECORD_ALIGN}`);
  }
  new Int32Array(control, byteOffset, ringCtrl.WORDS).set([0, 0, 0, capacityBytes]);
}

export function createSharedMemoryViews(segments: SharedMemorySegments): SharedMemoryViews {
  const status = new Int32Array(segments.control, CONTROL_LAYOUT.statusOffset, STATUS_INTS);
  const guestLayout = readGuestRamLayoutFromStatus(status);

  const memBytes = (segments.guestMemory.buffer as unknown as SharedArrayBuffer).byteLength;
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
  return {
    segments,
    status,
    guestLayout,
    guestU8,
    guestI32,
    vgaFramebuffer: segments.vgaFramebuffer,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
  };
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
    // Only probe for *support* here; do not require the ability to declare a 4GiB
    // maximum. Some runtimes may allow shared memories but enforce lower maxima
    // depending on device/browser limits. The actual guest RAM allocation path
    // will surface size-related failures with a more specific error message.
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
  let idx: StatusIndex;
  switch (role) {
    case "cpu":
      idx = StatusIndex.CpuReady;
      break;
    case "gpu":
      idx = StatusIndex.GpuReady;
      break;
    case "io":
      idx = StatusIndex.IoReady;
      break;
    case "jit":
      idx = StatusIndex.JitReady;
      break;
    case "net":
      idx = StatusIndex.NetReady;
      break;
    default: {
      const neverRole: never = role;
      throw new Error(`Unknown worker role: ${String(neverRole)}`);
    }
  }
  Atomics.store(status, idx, ready ? 1 : 0);
}
