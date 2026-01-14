import { RECORD_ALIGN, queueKind, ringCtrl } from "../ipc/layout";
import { createIpcBuffer, type IpcQueueSpec } from "../ipc/ipc";
import { PCI_MMIO_BASE, VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { guestPaddrToRamOffset as guestPaddrToRamOffsetRaw, guestRangeInBounds as guestRangeInBoundsRaw } from "../arch/guest_ram_translate.ts";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
} from "../ipc/shared-layout";
import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_STATE_BYTE_LEN,
  SCANOUT_STATE_U32_LEN,
  ScanoutStateIndex,
  wrapScanoutState,
} from "../ipc/scanout_state";
import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_STATE_BYTE_LEN,
  CURSOR_STATE_U32_LEN,
  CursorStateIndex,
  wrapCursorState,
} from "../ipc/cursor_state";

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

  // Input telemetry (optional; used by tests and perf instrumentation).
  //
  // Ownership:
  // - legacy runtime: written by the IO worker (input is injected there)
  // - `vmRuntime="machine"`: written by the machine CPU worker (input is injected there)
  IoInputBatchCounter: 2,
  IoInputEventCounter: 3,

  // Input backend selection status (debug HUD / tests).
  //
  // Ownership:
  // - `vmRuntime=legacy`: written by the I/O worker input routing layer
  // - `vmRuntime=machine`: written by the machine CPU worker (typically conservative defaults)
  //
  // Integer encoding contract:
  //   0 = ps2
  //   1 = usb
  //   2 = virtio
  //
  // See `web/src/input/input_backend_status.ts`.
  IoInputKeyboardBackend: 20,
  IoInputMouseBackend: 21,
  IoInputVirtioKeyboardDriverOk: 22,
  IoInputVirtioMouseDriverOk: 23,
  // Synthetic USB HID guest readiness (i.e. device configured by the guest stack).
  IoInputUsbKeyboardOk: 24,
  IoInputUsbMouseOk: 25,
  // Total number of held "keyboard-like" inputs (keyboard HID usages + Consumer Control usages).
  // This is used to gate backend switching to avoid stuck keys when routing changes mid-press.
  IoInputKeyboardHeldCount: 26,
  IoInputMouseButtonsHeldMask: 27,

  // Guest-reported keyboard LED bitmasks for each backend (best-effort diagnostics).
  //
  // Bit layout (HID-style; shared with `Machine.*_keyboard_leds()` helpers):
  // - bit0: Num Lock
  // - bit1: Caps Lock
  // - bit2: Scroll Lock
  // - bit3: Compose
  // - bit4: Kana
  IoInputKeyboardLedsUsb: 36,
  IoInputKeyboardLedsVirtio: 37,
  IoInputKeyboardLedsPs2: 38,

  // Total input batches received by the input injector (including queued/dropped while snapshot-paused).
  IoInputBatchReceivedCounter: 28,
  // Total input batches dropped by the input injector (e.g. when snapshot-paused queue is full, or when a malformed batch is received).
  IoInputBatchDropCounter: 29,
  // Total backend switches (ps2↔usb↔virtio) observed by the active input injector's routing layer.
  //
  // Ownership:
  // - `vmRuntime=legacy`: written by the I/O worker
  // - `vmRuntime=machine`: written by the machine CPU worker
  IoKeyboardBackendSwitchCounter: 30,
  IoMouseBackendSwitchCounter: 31,

  // Input `performance.now()`-derived u32 microsecond latencies.
  // All values wrap as u32; consumers should use unsigned arithmetic (`>>> 0`).
  //
  // - batch_send_latency_us: (io_now_us - batchSendTimestampUs) for the most recent batch
  // - event_latency_*: based on (io_now_us - eventTimestampUs) for events in the most recent batch
  IoInputBatchSendLatencyUs: 40,
  IoInputBatchSendLatencyEwmaUs: 41,
  IoInputBatchSendLatencyMaxUs: 42,
  IoInputEventLatencyAvgUs: 43,
  IoInputEventLatencyEwmaUs: 44,
  IoInputEventLatencyMaxUs: 45,

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
  // Main thread WebHID output/feature report telemetry (bounded send queue drops).
  IoHidOutputReportDropCounter: 35,

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

export function createIoIpcSab(opts: { includeNet?: boolean; includeHidIn?: boolean } = {}): SharedArrayBuffer {
  const includeNet = opts.includeNet ?? true;
  const includeHidIn = opts.includeHidIn ?? true;
  const specs: IpcQueueSpec[] = [
    { kind: IO_IPC_CMD_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
    { kind: IO_IPC_EVT_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
  ];
  if (includeNet) {
    specs.push(
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: IO_IPC_NET_RING_CAPACITY_BYTES },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: IO_IPC_NET_RING_CAPACITY_BYTES },
    );
  }
  if (includeHidIn) {
    specs.push({ kind: IO_IPC_HID_IN_QUEUE_KIND, capacityBytes: IO_IPC_HID_IN_RING_CAPACITY_BYTES });
  }
  return createIpcBuffer(specs).buffer;
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
 * - `sharedFramebuffer`: a shared framebuffer region for legacy/demo scanout.
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

/**
 * Default VRAM aperture size (BAR1 backing) for the browser runtime.
 *
 * This is a standalone `SharedArrayBuffer` (not embedded in WASM linear memory) so multiple
 * workers can access VRAM bytes directly.
 *
 * Tests should override this to avoid large allocations.
 */
export const DEFAULT_VRAM_MIB = 64;

const WASM_PAGE_BYTES = 64 * 1024;
const MAX_WASM32_PAGES = 65536;

/**
 * Guest-physical base of the PCI MMIO BAR allocation window used by the web runtime.
 *
 * Note: on the canonical PC/Q35 platform, the reserved below-4 GiB PCI/MMIO hole is larger
 * (`0xC000_0000..0x1_0000_0000`) and includes PCIe ECAM at `0xB000_0000..0xC000_0000`. The web
 * runtime currently allocates PCI BARs out of the high sub-window starting at `PCI_MMIO_BASE`.
 *
 * Guest RAM is clamped to lie strictly below this address so that any access to
 * `paddr >= GUEST_PCI_MMIO_BASE` is routed to MMIO handlers instead of silently
 * hitting RAM.
 *
 * NOTE: Keep this in sync with:
 * - `web/src/arch/guest_phys.ts` (`PCI_MMIO_BASE`).
 * - `crates/aero-wasm/src/guest_layout.rs` (`GUEST_PCI_MMIO_BASE`).
 */
export const GUEST_PCI_MMIO_BASE = PCI_MMIO_BASE;

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

export { HIGH_RAM_START, LOW_RAM_END } from "../arch/guest_ram_translate.ts";

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
//
// IMPORTANT: The CPU worker continuously publishes demo frames into this region
// when the shared framebuffer is embedded in guest RAM. Any test harness or
// device-model scratch buffers that use fixed guest-physical offsets must keep
// their ranges disjoint from this region or they will be corrupted in the
// background (causing flaky failures).
export const CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES = 0x20_0000; // 2 MiB (64-byte aligned)
export const CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH = 640;
export const CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT = 480;
export const CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE = 32;

// Shared scanout descriptor lives inside the wasm linear memory (guestMemory.buffer) so both:
// - WASM device models (legacy VGA/VBE, AeroGPU WDDM path, etc), and
// - JS workers (GPU presenter, frame scheduler)
// can read/write it using Atomics without extra SharedArrayBuffer allocations/copies.
//
// The region is placed at the *end* of the runtime-reserved region, immediately before the
// 64-byte memory-wiring probe window used by `web/src/runtime/wasm_memory_probe.ts`.
//
// IMPORTANT: Keep these constants in sync with the wasm-side runtime allocator guard in
// `crates/aero-wasm/src/runtime_alloc.rs`.
const WASM_MEMORY_PROBE_WINDOW_BYTES = 64;
const WASM_RUNTIME_HEAP_TAIL_GUARD_BYTES =
  WASM_MEMORY_PROBE_WINDOW_BYTES + SCANOUT_STATE_BYTE_LEN + CURSOR_STATE_BYTE_LEN;

function mibToBytes(mib: number): number {
  return mib * 1024 * 1024;
}

function bytesToPages(bytes: number): number {
  return Math.ceil(bytes / WASM_PAGE_BYTES);
}

export interface SharedMemorySegments {
  control: SharedArrayBuffer;
  guestMemory: WebAssembly.Memory;
  /**
   * Shared VRAM aperture (BAR1 backing).
   *
   * When present, this buffer backs guest physical addresses
   * `[VRAM_BASE_PADDR, VRAM_BASE_PADDR + vram.byteLength)`.
   */
  vram?: SharedArrayBuffer;
  ioIpc: SharedArrayBuffer;
  sharedFramebuffer: SharedArrayBuffer;
  /**
   * Shared scanout descriptor used to select which framebuffer the presenter should display.
   *
   * Layout/protocol: `web/src/ipc/scanout_state.ts` / `crates/aero-shared/src/scanout_state.rs`.
   */
  scanoutState?: SharedArrayBuffer;
  /**
   * Byte offset within `scanoutState` where the scanout header begins.
   *
   * This is typically 0 (dedicated SharedArrayBuffer), but the contract supports embedding
   * in a larger shared region in the future.
   */
  scanoutStateOffsetBytes?: number;
  /**
   * Shared hardware cursor descriptor used to present the WDDM hardware cursor without
   * legacy `cursor_set_*` postMessages.
   *
   * Layout/protocol: `web/src/ipc/cursor_state.ts` / `crates/aero-shared/src/cursor_state.rs`.
   */
  cursorState?: SharedArrayBuffer;
  /**
   * Byte offset within `cursorState` where the cursor header begins (typically 0).
   */
  cursorStateOffsetBytes?: number;
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
  /**
   * Flat byte view of the shared VRAM aperture.
   *
   * When the runtime is configured without VRAM (e.g. some unit tests), this is a zero-length
   * `Uint8Array`.
   */
  vramU8: Uint8Array;
  /**
   * Size of the VRAM aperture in bytes (equals `vramU8.byteLength`).
   */
  vramSizeBytes: number;
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes: number;
  scanoutState?: SharedArrayBuffer;
  scanoutStateOffsetBytes?: number;
  scanoutStateI32?: Int32Array;
  cursorState?: SharedArrayBuffer;
  cursorStateOffsetBytes?: number;
  cursorStateI32?: Int32Array;
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

function toU64(value: number): number {
  // JS numbers are IEEE754 doubles; we can exactly represent up to 2^53-1.
  if (!Number.isFinite(value)) {
    throw new RangeError(`Expected a finite integer address, got ${String(value)}`);
  }
  const int = Math.trunc(value);
  if (int < 0 || int > Number.MAX_SAFE_INTEGER) {
    throw new RangeError(`Expected an integer in [0, 2^53), got ${String(value)}`);
  }
  return int;
}

export function computeGuestRamLayout(desiredGuestBytes: number): GuestRamLayout {
  const desired = clampToU32(desiredGuestBytes);
  const guestBase = align(RUNTIME_RESERVED_BYTES, WASM_PAGE_BYTES);
  const runtimeReserved = guestBase;

  const basePages = Math.floor(guestBase / WASM_PAGE_BYTES);
  const maxGuestPagesByWasm = Math.max(0, MAX_WASM32_PAGES - basePages);
  const maxGuestBytesByWasm = maxGuestPagesByWasm * WASM_PAGE_BYTES;
  const maxGuestBytes = Math.min(maxGuestBytesByWasm, GUEST_PCI_MMIO_BASE);

  // Clamp using page counts rather than bytes so rounding-up never produces a guest_size
  // that exceeds maxGuestBytes (important if we ever change GUEST_PCI_MMIO_BASE to a
  // non-page-aligned boundary).
  const desiredPages = bytesToPages(desired);
  const maxGuestPages = Math.floor(maxGuestBytes / WASM_PAGE_BYTES);
  const guestPages = Math.min(desiredPages, maxGuestPages);
  const totalPages = basePages + guestPages;

  return {
    guest_base: guestBase,
    guest_size: guestPages * WASM_PAGE_BYTES,
    runtime_reserved: runtimeReserved,
    wasm_pages: totalPages,
  };
}

/**
 * Translate a guest physical address into a backing-RAM offset (0..guest_size) when (and only
 * when) the address is backed by RAM.
 *
 * Returns `null` for ECAM/PCI holes or out-of-range addresses.
 *
 * Implementation lives in `web/src/arch/guest_ram_translate.ts`; this wrapper adapts it to the
 * `GuestRamLayout` shape used throughout the runtime.
 *
 * This mirrors the PC/Q35 address translation in `crates/aero-wasm/src/guest_phys.rs`.
 */
export function guestPaddrToRamOffset(layout: GuestRamLayout, paddr: number): number | null {
  return guestPaddrToRamOffsetRaw(layout.guest_size, paddr);
}

export function guestToLinear(layout: GuestRamLayout, paddr: number): number {
  const addr = toU64(paddr);
  const ramOffset = guestPaddrToRamOffset(layout, addr);
  if (ramOffset === null) {
    throw new RangeError(
      `Guest physical address is not backed by RAM: 0x${addr.toString(16)} (guest_size=0x${layout.guest_size.toString(16)})`,
    );
  }
  return layout.guest_base + ramOffset;
}

export function guestRangeInBounds(layout: GuestRamLayout, paddr: number, byteLength: number): boolean {
  return guestRangeInBoundsRaw(layout.guest_size, paddr, byteLength);
}

export function readGuestRamLayoutFromStatus(status: Int32Array): GuestRamLayout {
  const guestBase = Atomics.load(status, StatusIndex.GuestBase) >>> 0;
  const guestSize = Atomics.load(status, StatusIndex.GuestSize) >>> 0;
  const runtimeReserved = Atomics.load(status, StatusIndex.RuntimeReserved) >>> 0;

  if (guestBase === 0 && guestSize === 0) {
    throw new Error("Guest RAM layout was not initialized in status SAB (guest_base/guest_size are 0).");
  }
  if (guestSize > GUEST_PCI_MMIO_BASE) {
    throw new Error(
      `Guest RAM overlaps the PCI MMIO BAR window: guest_size=0x${guestSize.toString(16)} PCI_MMIO_BASE=0x${GUEST_PCI_MMIO_BASE.toString(16)}`,
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
// Byte offset within the control SAB where the shared `status` Int32Array begins.
// Exported so worker entrypoints can map status without needing to know the private control layout.
export const STATUS_OFFSET_BYTES = CONTROL_LAYOUT.statusOffset;

export function ringRegionsForWorker(role: WorkerRole): RingRegions {
  return CONTROL_LAYOUT.rings[role];
}

export function allocateSharedMemorySegments(options?: {
  guestRamMiB?: GuestRamMiB;
  vramMiB?: number;
  /**
   * Override the default IO IPC buffer allocation.
   *
   * Most callers should leave this unset (it defaults to `createIoIpcSab()`).
   * Tests/harnesses that only need CMD/EVT rings can request a smaller buffer via `ioIpcOptions`
   * to avoid allocating large NET/HID rings unnecessarily.
   */
  ioIpc?: SharedArrayBuffer;
  /**
   * Options forwarded to `createIoIpcSab()` when `ioIpc` is not supplied.
   *
   * Defaults match production (include NET + HID rings).
   */
  ioIpcOptions?: { includeNet?: boolean; includeHidIn?: boolean };
  /**
   * Override the legacy/demo shared framebuffer layout.
   *
   * This is primarily useful for tests/harnesses that do not need the demo framebuffer but still
   * need to supply a `sharedFramebuffer` segment to worker init messages.
   *
   * Defaults match the canonical CPU worker demo framebuffer.
   */
  sharedFramebufferLayout?: {
    width?: number;
    height?: number;
    strideBytes?: number;
    format?: FramebufferFormat;
    tileSize?: number;
  };
}): SharedMemorySegments {
  const guestRamMiB = options?.guestRamMiB ?? DEFAULT_GUEST_RAM_MIB;
  const vramMiB = options?.vramMiB ?? DEFAULT_VRAM_MIB;
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

  // Shared VRAM aperture (BAR1 backing).
  //
  // Keep this separate from `guestMemory` so wasm32's linear memory limit does not constrain VRAM
  // sizing and so workers can map BAR1 without relying on guest RAM address translation.
  //
  // `vramMiB=0` disables the segment (useful for unit tests).
  let vram: SharedArrayBuffer | undefined = undefined;
  const desiredVramBytes = clampToU32(Math.trunc(mibToBytes(vramMiB)));
  if (desiredVramBytes > 0) {
    try {
      vram = new SharedArrayBuffer(desiredVramBytes);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      throw new Error(
        `Failed to allocate shared VRAM aperture (${vramMiB} MiB at paddr=0x${VRAM_BASE_PADDR.toString(16)}). Try a smaller size. Error: ${msg}`,
      );
    }
  }

  // Shared CPU→GPU framebuffer demo region.
  //
  // Prefer embedding directly in the shared guest `WebAssembly.Memory` so the
  // CPU worker's WASM code can write/publish frames without JS-side copies. When
  // the configured guest RAM is too small (e.g. unit tests using ~1MiB guest
  // memory), fall back to a standalone SharedArrayBuffer.
  const fbLayoutOpts = options?.sharedFramebufferLayout;
  const fbWidth = fbLayoutOpts?.width ?? CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH;
  const fbHeight = fbLayoutOpts?.height ?? CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT;
  const fbStrideBytes = fbLayoutOpts?.strideBytes ?? fbWidth * 4;
  const fbFormat = fbLayoutOpts?.format ?? FramebufferFormat.RGBA8;
  const fbTileSize = fbLayoutOpts?.tileSize ?? CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE;
  const fbLayout = computeSharedFramebufferLayout(fbWidth, fbHeight, fbStrideBytes, fbFormat, fbTileSize);
  const embeddedOffsetBytes = layout.guest_base + CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES;
  const embeddedRequiredBytes = embeddedOffsetBytes + fbLayout.totalBytes;

  const sharedFramebufferEmbedded = embeddedRequiredBytes <= guestSab.byteLength;

  const sharedFramebuffer = sharedFramebufferEmbedded ? guestSab : new SharedArrayBuffer(fbLayout.totalBytes);
  const sharedFramebufferOffsetBytes = sharedFramebufferEmbedded ? embeddedOffsetBytes : 0;
  const sharedHeader = new Int32Array(sharedFramebuffer, sharedFramebufferOffsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.WIDTH, fbLayout.width);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.HEIGHT, fbLayout.height);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.STRIDE_BYTES, fbLayout.strideBytes);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FORMAT, fbLayout.format);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILE_SIZE, fbLayout.tileSize);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILES_X, fbLayout.tilesX);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.TILES_Y, fbLayout.tilesY);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, fbLayout.dirtyWordsPerBuffer);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FLAGS, 0);

  // Single authoritative scanout state (for selecting legacy VGA/VBE vs WDDM).
  // Embed this inside the shared WebAssembly.Memory so the WASM VM can update it directly.
  const scanoutState = guestSab;
  const scanoutStateOffsetBytes = layout.runtime_reserved - WASM_RUNTIME_HEAP_TAIL_GUARD_BYTES;
  const scanoutWords = new Int32Array(scanoutState, scanoutStateOffsetBytes, SCANOUT_STATE_U32_LEN);
  Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_LEGACY_TEXT);
  Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_LO, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_HI, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.WIDTH, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.HEIGHT, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.PITCH_BYTES, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.FORMAT, SCANOUT_FORMAT_B8G8R8X8);

  // Hardware cursor state descriptor (WDDM cursor registers + surface pointer).
  // Embed this inside the shared WebAssembly.Memory so the WASM VM can update it directly.
  const cursorState = guestSab;
  const cursorStateOffsetBytes = scanoutStateOffsetBytes + SCANOUT_STATE_BYTE_LEN;
  const cursorWords = new Int32Array(cursorState, cursorStateOffsetBytes, CURSOR_STATE_U32_LEN);
  Atomics.store(cursorWords, CursorStateIndex.GENERATION, 0);
  Atomics.store(cursorWords, CursorStateIndex.ENABLE, 0);
  Atomics.store(cursorWords, CursorStateIndex.X, 0);
  Atomics.store(cursorWords, CursorStateIndex.Y, 0);
  Atomics.store(cursorWords, CursorStateIndex.HOT_X, 0);
  Atomics.store(cursorWords, CursorStateIndex.HOT_Y, 0);
  Atomics.store(cursorWords, CursorStateIndex.WIDTH, 0);
  Atomics.store(cursorWords, CursorStateIndex.HEIGHT, 0);
  Atomics.store(cursorWords, CursorStateIndex.PITCH_BYTES, 0);
  Atomics.store(cursorWords, CursorStateIndex.FORMAT, CURSOR_FORMAT_B8G8R8A8);
  Atomics.store(cursorWords, CursorStateIndex.BASE_PADDR_LO, 0);
  Atomics.store(cursorWords, CursorStateIndex.BASE_PADDR_HI, 0);

  const ioIpc = options?.ioIpc ?? createIoIpcSab(options?.ioIpcOptions ?? {});

  return {
    control,
    guestMemory,
    vram,
    ioIpc,
    sharedFramebuffer,
    sharedFramebufferOffsetBytes,
    scanoutState,
    scanoutStateOffsetBytes,
    cursorState,
    cursorStateOffsetBytes,
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

  // A flat byte view of the guest RAM backing store. This is convenient for tests/demo harnesses.
  //
  // Note: on PC/Q35 with PCI holes + high-memory remap, guest physical memory is non-contiguous;
  // this array is not a 1:1 view of guest *physical addresses* once holes are modeled.
  const guestU8 = new Uint8Array(segments.guestMemory.buffer, guestLayout.guest_base, guestLayout.guest_size);
  const guestI32 = new Int32Array(
    segments.guestMemory.buffer,
    guestLayout.guest_base,
    Math.floor(guestLayout.guest_size / Int32Array.BYTES_PER_ELEMENT),
  );

  const vramSab = segments.vram;
  const vramU8 = vramSab instanceof SharedArrayBuffer ? new Uint8Array(vramSab) : new Uint8Array(0);
  const vramSizeBytes = vramU8.byteLength;

  const guestSab = segments.guestMemory.buffer as unknown as ArrayBufferLike;

  const scanoutStateOffsetBytes = segments.scanoutStateOffsetBytes ?? 0;
  let scanoutState = segments.scanoutState;
  // Some runtimes embed ScanoutState inside the guest WebAssembly.Memory and only communicate the
  // byte offset. This avoids passing multiple aliases of the same SharedArrayBuffer through
  // structured clone (which has been observed to corrupt init messages on Firefox).
  if (!(scanoutState instanceof SharedArrayBuffer) && scanoutStateOffsetBytes > 0 && guestSab instanceof SharedArrayBuffer) {
    scanoutState = guestSab;
  }
  let scanoutStateI32: Int32Array | undefined = undefined;
  if (scanoutState instanceof SharedArrayBuffer) {
    try {
      scanoutStateI32 = wrapScanoutState(scanoutState, scanoutStateOffsetBytes);
    } catch {
      // Defensive fallback: if a caller passed the wrong SAB (e.g. due to browser structured-clone
      // bugs), retry using the guest memory backing store when the offset suggests embedding.
      if (scanoutStateOffsetBytes > 0 && guestSab instanceof SharedArrayBuffer && scanoutState !== guestSab) {
        scanoutState = guestSab;
        scanoutStateI32 = wrapScanoutState(scanoutState, scanoutStateOffsetBytes);
      }
    }
  }

  const cursorStateOffsetBytes = segments.cursorStateOffsetBytes ?? 0;
  let cursorState = segments.cursorState;
  if (!(cursorState instanceof SharedArrayBuffer) && cursorStateOffsetBytes > 0 && guestSab instanceof SharedArrayBuffer) {
    cursorState = guestSab;
  }
  let cursorStateI32: Int32Array | undefined = undefined;
  if (cursorState instanceof SharedArrayBuffer) {
    try {
      cursorStateI32 = wrapCursorState(cursorState, cursorStateOffsetBytes);
    } catch {
      if (cursorStateOffsetBytes > 0 && guestSab instanceof SharedArrayBuffer && cursorState !== guestSab) {
        cursorState = guestSab;
        cursorStateI32 = wrapCursorState(cursorState, cursorStateOffsetBytes);
      }
    }
  }
  return {
    segments,
    status,
    guestLayout,
    guestU8,
    guestI32,
    vramU8,
    vramSizeBytes,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    scanoutState,
    scanoutStateOffsetBytes,
    scanoutStateI32,
    cursorState,
    cursorStateOffsetBytes,
    cursorStateI32,
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
