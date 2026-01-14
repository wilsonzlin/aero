/// <reference lib="webworker" />

import { initWasmForContext, type WasmApi } from "../runtime/wasm_context";
import { assertWasmMemoryWiring } from "../runtime/wasm_memory_probe";
import { negateI32Saturating } from "../input/int32";
import {
  FRAMEBUFFER_COPY_MESSAGE_TYPE,
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_FORMAT,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  HEADER_BYTE_LENGTH,
  addHeaderI32,
  initFramebufferHeader,
  storeHeaderI32,
  type FramebufferCopyMessageV1,
  wrapSharedFramebuffer,
} from "../display/framebuffer_protocol";

type MachineVgaWorkerStartMessage = {
  type: "machineVga.start";
  /**
   * Serial bytes to print from the boot sector (ASCII/UTF-8), written to COM1.
   */
  message?: string;
  /**
   * Optional VBE mode to program from the boot sector before halting.
   *
   * When set, the boot sector programs the Bochs VBE registers (0x01CE/0x01CF) for a 32bpp mode
   * and writes a single red pixel through the banked 0xA0000 window.
   *
   * Note: this uses the Bochs VBE_DISPI port interface provided by the standalone VGA/VBE device
   * model (`MachineConfig::enable_vga=true`). It is not the same as the BIOS INT 10h VBE path used
   * by the AeroGPU-owned boot display wiring (`enable_aerogpu=true`).
   */
  vbeMode?: { width: number; height: number };
  /**
   * Guest RAM size passed to `new api.Machine(ramSizeBytes)`.
   *
   * Keep this small: the wasm32 runtime allocator reserves a fixed 128MiB region
   * for Rust heap allocations.
   */
  ramSizeBytes?: number;
  /**
   * Request the canonical machine be constructed with AeroGPU enabled (and VGA disabled by default).
   *
   * Note: in `aero_machine` today, `enable_aerogpu` wires BAR1-backed VRAM plus minimal legacy VGA
   * decode (legacy VGA window aliasing + permissive VGA ports) and a minimal BAR0 register block
   * (scanout/vblank storage + ring/fence transport). Ring processing is currently a **no-op**
   * (fence completion only) and does not execute real command streams.
   *
   * Also note: the AeroGPU mode does **not** expose the Bochs VBE_DISPI register ports
   * (`0x01CE/0x01CF`) implemented by the standalone VGA/VBE device model (`aero-gpu-vga`), so the
   * `vbeMode` boot-sector demo in this worker is not supported unless VGA is enabled.
   */
  enableAerogpu?: boolean;
  /**
   * Optional explicit VGA enablement override when constructing the machine via `new_with_config`.
   *
   * Note: current native machine config treats AeroGPU and VGA as mutually exclusive; passing
   * `enableAerogpu=true` and `enableVga=true` will fail machine construction.
   */
  enableVga?: boolean;
  /**
   * Optional vCPU count passed to `api.Machine.new_with_cpu_count(ramSizeBytes, cpuCount)`.
   *
   * When omitted (or when the active WASM build does not support `new_with_cpu_count`), the machine
   * defaults to 1 vCPU.
   *
   * Note: When `enableAerogpu` or `enableVga` is provided, the worker constructs the machine via
   * `Machine.new_with_config`. Newer WASM builds accept an optional `cpuCount` parameter there as
   * well.
   */
  cpuCount?: number;
};

type MachineVgaWorkerStopMessage = {
  type: "machineVga.stop";
};

type MachineVgaWorkerInjectBrowserKeyMessage = {
  type: "machineVga.inject_browser_key";
  code: string;
  pressed: boolean;
};

type MachineVgaWorkerInjectMouseMotionMessage = {
  type: "machineVga.inject_mouse_motion";
  dx: number;
  dy: number;
  wheel?: number;
};

type MachineVgaWorkerInjectMouseButtonMessage = {
  type: "machineVga.inject_mouse_button";
  button: number;
  pressed: boolean;
};

type MachineVgaWorkerIncomingMessage =
  | MachineVgaWorkerStartMessage
  | MachineVgaWorkerStopMessage
  | MachineVgaWorkerInjectBrowserKeyMessage
  | MachineVgaWorkerInjectMouseMotionMessage
  | MachineVgaWorkerInjectMouseButtonMessage;

type MachineVgaWorkerReadyMessage = {
  type: "machineVga.ready";
  transport: "shared" | "copy";
  /**
   * Present-only shared framebuffer region (RGBA8888 + framebuffer_protocol header) containing
   * the machine's display scanout.
   *
   * Present when `transport === "shared"`.
   */
  framebuffer?: SharedArrayBuffer;
};

type MachineVgaWorkerSerialMessage = {
  type: "machineVga.serial";
  data: Uint8Array;
};

type MachineVgaWorkerStatusMessage = {
  type: "machineVga.status";
  detail: string;
};

type MachineVgaWorkerErrorMessage = {
  type: "machineVga.error";
  message: string;
};

type MachineVgaWorkerMessage =
  | MachineVgaWorkerReadyMessage
  | MachineVgaWorkerSerialMessage
  | MachineVgaWorkerStatusMessage
  | MachineVgaWorkerErrorMessage
  | FramebufferCopyMessageV1;

const ctx = self as unknown as DedicatedWorkerGlobalScope;

const encoder = new TextEncoder();

// Note: this file is still named `machine_vga.worker.ts` and uses `machineVga.*` message types for
// back-compat with existing demos/tests, but it now prefers the unified `display_*` scanout exports
// when present (falling back to legacy `vga_*`).

let api: WasmApi | null = null;
let wasmMemory: WebAssembly.Memory | null = null;
let machine: InstanceType<WasmApi["Machine"]> | null = null;
let preferredWasmMemory: WebAssembly.Memory | null = null;

let transport: "shared" | "copy" = "copy";
let sharedSab: SharedArrayBuffer | null = null;
let sharedFb: ReturnType<typeof wrapSharedFramebuffer> | null = null;
let sharedWidth = 0;
let sharedHeight = 0;
let sharedStrideBytes = 0;
let sharedFrameCounter = 0;

let tickTimer: number | null = null;
let copyFrameCounter = 0;
let lastExitDetail: string | null = null;

// Avoid pathological allocations/copies if a buggy guest or WASM build reports absurd scanout modes.
const MAX_FRAME_BYTES = 32 * 1024 * 1024;
const RUNTIME_RESERVED_BYTES = 128 * 1024 * 1024;
const WASM_PAGE_BYTES = 64 * 1024;

function post(msg: MachineVgaWorkerMessage, transfer?: Transferable[]): void {
  if (transfer && transfer.length) {
    ctx.postMessage(msg, transfer);
  } else {
    ctx.postMessage(msg);
  }
}

function stop(): void {
  if (tickTimer !== null) {
    ctx.clearInterval(tickTimer);
    tickTimer = null;
  }

  if (machine) {
    try {
      (machine as unknown as { free?: () => void }).free?.();
    } catch {
      // ignore
    }
  }
  machine = null;
  api = null;
  wasmMemory = null;

  sharedSab = null;
  sharedFb = null;
  sharedWidth = 0;
  sharedHeight = 0;
  sharedStrideBytes = 0;
  sharedFrameCounter = 0;
  copyFrameCounter = 0;
  lastExitDetail = null;
}

function buildSerialBootSector(message: string): Uint8Array {
  const msgBytes = encoder.encode(message);
  const sector = new Uint8Array(512);
  let off = 0;

  // Emit a tiny VGA text-mode banner ("AERO!") into 0xB8000 so the demo has visible output.
  //
  // cld
  sector[off++] = 0xfc;
  // mov ax, 0xb800
  sector.set([0xb8, 0x00, 0xb8], off);
  off += 3;
  // mov es, ax
  sector.set([0x8e, 0xc0], off);
  off += 2;
  // xor di, di
  sector.set([0x31, 0xff], off);
  off += 2;
  // mov ah, 0x1f  (white-on-blue)
  sector.set([0xb4, 0x1f], off);
  off += 2;
  for (const ch of encoder.encode("AERO!")) {
    // mov al, imm8
    sector.set([0xb0, ch], off);
    off += 2;
    // stosw
    sector[off++] = 0xab;
  }

  // mov dx, 0x3f8
  sector.set([0xba, 0xf8, 0x03], off);
  off += 3;

  // Reserve enough room for the fixed tail instructions (sti + hlt/jmp) plus the 0x55AA boot
  // signature at bytes 510..511. We will truncate overly-long messages rather than throwing.
  const FOOTER_BYTES = 4;
  for (const b of msgBytes) {
    // Per-byte encoding: mov al, imm8 (2 bytes) + out dx, al (1 byte).
    if (off + 3 > 510 - FOOTER_BYTES) break;
    // mov al, imm8
    sector.set([0xb0, b], off);
    off += 2;
    // out dx, al
    sector[off++] = 0xee;
  }

  // sti (ensure IRQs can wake the CPU)
  sector[off++] = 0xfb;
  // hlt; jmp hlt (wait-for-interrupt loop)
  const hltOff = off;
  sector[off++] = 0xf4;
  const jmpOff = off;
  sector[off++] = 0xeb;
  sector[off++] = (hltOff - (jmpOff + 2)) & 0xff;

  // Boot signature.
  sector[510] = 0x55;
  sector[511] = 0xaa;
  return sector;
}

function buildVbeBootSector(opts: { message: string; width: number; height: number }): Uint8Array {
  const msgBytes = encoder.encode(opts.message);
  const width = Math.max(1, Math.min(0xffff, Math.trunc(opts.width)));
  const height = Math.max(1, Math.min(0xffff, Math.trunc(opts.height)));
  const sector = new Uint8Array(512);
  let off = 0;

  // Program a Bochs VBE mode (WxHx32) and write a single red pixel at (0,0).
  // cld
  sector[off++] = 0xfc;

  // mov dx, 0x01CE  (Bochs VBE index port)
  sector.set([0xba, 0xce, 0x01], off);
  off += 3;

  const writeVbeReg = (index: number, value: number) => {
    // mov ax, imm16 (index)
    sector.set([0xb8, index & 0xff, (index >>> 8) & 0xff], off);
    off += 3;
    // out dx, ax
    sector[off++] = 0xef;
    // inc dx (0x01CF)
    sector[off++] = 0x42;
    // mov ax, imm16 (value)
    sector.set([0xb8, value & 0xff, (value >>> 8) & 0xff], off);
    off += 3;
    // out dx, ax
    sector[off++] = 0xef;
    // dec dx (back to 0x01CE)
    sector[off++] = 0x4a;
  };

  // XRES = width
  writeVbeReg(0x0001, width);
  // YRES = height
  writeVbeReg(0x0002, height);
  // BPP = 32
  writeVbeReg(0x0003, 32);
  // ENABLE = 0x0041 (enable + LFB)
  writeVbeReg(0x0004, 0x0041);
  // BANK = 0
  writeVbeReg(0x0005, 0);

  // mov ax, 0xA000 ; mov es, ax ; xor di, di
  sector.set([0xb8, 0x00, 0xa0, 0x8e, 0xc0, 0x31, 0xff], off);
  off += 7;

  // Write a red pixel at (0,0) in BGRX format expected by the SVGA renderer.
  // mov al, 0x00 ; stosb ; stosb ; mov al, 0xff ; stosb ; mov al, 0x00 ; stosb
  sector.set([0xb0, 0x00, 0xaa, 0xaa, 0xb0, 0xff, 0xaa, 0xb0, 0x00, 0xaa], off);
  off += 10;

  // Serial output (COM1).
  // mov dx, 0x3f8
  sector.set([0xba, 0xf8, 0x03], off);
  off += 3;
  const FOOTER_BYTES = 4;
  for (const b of msgBytes) {
    // Per-byte encoding: mov al, imm8 (2 bytes) + out dx, al (1 byte).
    if (off + 3 > 510 - FOOTER_BYTES) break;
    sector.set([0xb0, b, 0xee], off); // mov al, imm8 ; out dx, al
    off += 3;
  }

  // cli; hlt; jmp $
  sector[off++] = 0xfa;
  sector[off++] = 0xf4;
  sector.set([0xeb, 0xfe], off);
  off += 2;

  // Boot signature.
  sector[510] = 0x55;
  sector[511] = 0xaa;
  return sector;
}

function ensureSharedFramebuffer(): ReturnType<typeof wrapSharedFramebuffer> | null {
  const Sab = globalThis.SharedArrayBuffer;
  if (typeof Sab === "undefined") return null;

  // Allocate an initial SharedArrayBuffer sized for a "modest" SVGA mode, but allow the
  // framebuffer to grow dynamically (up to `MAX_FRAME_BYTES`) when the guest switches to a
  // larger VBE mode.
  //
  // This keeps memory usage reasonable for the typical BIOS/VGA text-mode demo while still
  // supporting larger scanout modes.
  const initialMaxWidth = 1024;
  const initialMaxHeight = 768;
  const bytes = HEADER_BYTE_LENGTH + initialMaxWidth * initialMaxHeight * 4;
  let sab: SharedArrayBuffer;
  try {
    sab = new SharedArrayBuffer(bytes);
  } catch {
    return null;
  }

  let fb: ReturnType<typeof wrapSharedFramebuffer>;
  try {
    fb = wrapSharedFramebuffer(sab, 0);
  } catch {
    return null;
  }
  initFramebufferHeader(fb.header, { width: 1, height: 1, strideBytes: 4, format: FRAMEBUFFER_FORMAT_RGBA8888 });
  sharedSab = sab;
  sharedFb = fb;
  sharedWidth = 1;
  sharedHeight = 1;
  sharedStrideBytes = 4;
  sharedFrameCounter = 0;
  return fb;
}

function publishSharedFrame(width: number, height: number, strideBytes: number, pixels: Uint8Array): boolean {
  let fb = sharedFb;
  if (!fb) return false;

  const requiredBytes = strideBytes * height;
  if (requiredBytes > MAX_FRAME_BYTES) return false;

  if (requiredBytes > fb.pixelsU8.byteLength) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") return false;

    // Attempt to grow the shared framebuffer. If allocation fails (or wrap fails), degrade to copy
    // transport so the demo keeps functioning.
    const bytes = HEADER_BYTE_LENGTH + requiredBytes;
    let sab: SharedArrayBuffer;
    try {
      sab = new SharedArrayBuffer(bytes);
    } catch {
      transport = "copy";
      // Preserve a monotonic frame counter across transport changes (shared -> copy).
      copyFrameCounter = sharedFrameCounter;
      sharedSab = null;
      sharedFb = null;
      post({ type: "machineVga.ready", transport: "copy" } satisfies MachineVgaWorkerReadyMessage);
      return false;
    }

    let next: ReturnType<typeof wrapSharedFramebuffer>;
    try {
      next = wrapSharedFramebuffer(sab, 0);
    } catch {
      transport = "copy";
      // Preserve a monotonic frame counter across transport changes (shared -> copy).
      copyFrameCounter = sharedFrameCounter;
      sharedSab = null;
      sharedFb = null;
      post({ type: "machineVga.ready", transport: "copy" } satisfies MachineVgaWorkerReadyMessage);
      return false;
    }

    initFramebufferHeader(next.header, {
      width,
      height,
      strideBytes,
      format: FRAMEBUFFER_FORMAT_RGBA8888,
    });
    // Preserve the published frame counter across buffer swaps so consumers/tests can treat it as
    // a monotonic "frames published" counter.
    storeHeaderI32(next.header, HEADER_INDEX_FRAME_COUNTER, sharedFrameCounter);
    sharedSab = sab;
    sharedFb = next;
    fb = next;
    sharedWidth = width;
    sharedHeight = height;
    sharedStrideBytes = strideBytes;

    post({ type: "machineVga.ready", transport: "shared", framebuffer: sab } satisfies MachineVgaWorkerReadyMessage);
  }

  if (sharedWidth !== width || sharedHeight !== height || sharedStrideBytes !== strideBytes) {
    storeHeaderI32(fb.header, HEADER_INDEX_WIDTH, width);
    storeHeaderI32(fb.header, HEADER_INDEX_HEIGHT, height);
    storeHeaderI32(fb.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
    storeHeaderI32(fb.header, HEADER_INDEX_FORMAT, FRAMEBUFFER_FORMAT_RGBA8888);
    addHeaderI32(fb.header, HEADER_INDEX_CONFIG_COUNTER, 1);
    sharedWidth = width;
    sharedHeight = height;
    sharedStrideBytes = strideBytes;
  }

  fb.pixelsU8.set(pixels.subarray(0, requiredBytes), 0);
  sharedFrameCounter = (sharedFrameCounter + 1) >>> 0;
  storeHeaderI32(fb.header, HEADER_INDEX_FRAME_COUNTER, sharedFrameCounter);
  return true;
}

function postCopyFrame(width: number, height: number, strideBytes: number, pixels: Uint8Array): void {
  const requiredBytes = strideBytes * height;
  if (requiredBytes > MAX_FRAME_BYTES) return;
  const copy = new Uint8Array(requiredBytes);
  copy.set(pixels.subarray(0, requiredBytes));
  copyFrameCounter = (copyFrameCounter + 1) >>> 0;

  const msg: FramebufferCopyMessageV1 = {
    type: FRAMEBUFFER_COPY_MESSAGE_TYPE,
    width,
    height,
    strideBytes,
    format: FRAMEBUFFER_FORMAT_RGBA8888,
    frameCounter: copyFrameCounter,
    pixels: copy.buffer,
  };
  post(msg, [copy.buffer]);
}

function tryPresentScanoutFrame(kind: "display" | "vga"): boolean {
  const m = machine;
  if (!m) return false;

  const presentFn =
    kind === "display"
      ? (m as unknown as { display_present?: () => void }).display_present
      : (m as unknown as { vga_present?: () => void }).vga_present;
  const widthFn =
    kind === "display"
      ? (m as unknown as { display_width?: () => number }).display_width
      : (m as unknown as { vga_width?: () => number }).vga_width;
  const heightFn =
    kind === "display"
      ? (m as unknown as { display_height?: () => number }).display_height
      : (m as unknown as { vga_height?: () => number }).vga_height;
  if (typeof presentFn !== "function" || typeof widthFn !== "function" || typeof heightFn !== "function") return false;

  // Update the WASM-side scanout front buffer.
  presentFn.call(m);

  const width = widthFn.call(m) >>> 0;
  const height = heightFn.call(m) >>> 0;
  if (width === 0 || height === 0) return false;

  const strideFn =
    kind === "display"
      ? (m as unknown as { display_stride_bytes?: () => number }).display_stride_bytes
      : (m as unknown as { vga_stride_bytes?: () => number }).vga_stride_bytes;
  let strideBytes = (typeof strideFn === "function" ? strideFn.call(m) : width * 4) >>> 0;
  if (strideBytes < width * 4) return false;

  const requiredDstBytes = width * height * 4;
  let requiredBytes = strideBytes * height;
  if (requiredBytes > MAX_FRAME_BYTES) return false;

  let pixels: Uint8Array | null = null;
  const mem = wasmMemory;
  const ptrFn =
    kind === "display"
      ? (m as unknown as { display_framebuffer_ptr?: () => number }).display_framebuffer_ptr
      : (m as unknown as { vga_framebuffer_ptr?: () => number }).vga_framebuffer_ptr;
  const lenFn =
    kind === "display"
      ? (m as unknown as { display_framebuffer_len_bytes?: () => number }).display_framebuffer_len_bytes
      : (m as unknown as { vga_framebuffer_len_bytes?: () => number }).vga_framebuffer_len_bytes;
  if (mem && typeof ptrFn === "function" && typeof lenFn === "function") {
    const ptr = ptrFn.call(m) >>> 0;
    const len = lenFn.call(m) >>> 0;
    if (ptr !== 0) {
      // Some builds may report a stride but still expose a tightly-packed framebuffer length.
      // If the length matches `width*height*4`, treat it as tightly packed to avoid rejecting
      // the scanout entirely.
      if (len < requiredBytes && len === requiredDstBytes) {
        strideBytes = width * 4;
        requiredBytes = requiredDstBytes;
      }
      if (len >= requiredBytes) {
        const buf = mem.buffer;
        if (ptr + requiredBytes <= buf.byteLength) {
          pixels = new Uint8Array(buf, ptr, requiredBytes);
        }
      }
    }
  }

  if (!pixels) {
    const copyFn =
      kind === "display"
        ? (m as unknown as { display_framebuffer_copy_rgba8888?: () => Uint8Array }).display_framebuffer_copy_rgba8888
        : (m as unknown as { vga_framebuffer_copy_rgba8888?: () => Uint8Array }).vga_framebuffer_copy_rgba8888;
    const legacyFn =
      kind === "vga" ? (m as unknown as { vga_framebuffer_rgba8888_copy?: () => Uint8Array | null }).vga_framebuffer_rgba8888_copy : undefined;
    if (typeof copyFn === "function") {
      pixels = copyFn.call(m);
    } else if (typeof legacyFn === "function") {
      pixels = legacyFn.call(m);
    }
  }

  if (pixels && pixels.byteLength < requiredBytes && pixels.byteLength === requiredDstBytes) {
    strideBytes = width * 4;
    requiredBytes = requiredDstBytes;
  }
  if (!pixels || pixels.byteLength < requiredBytes) return false;

  if (transport === "shared") {
    const ok = publishSharedFrame(width, height, strideBytes, pixels);
    if (!ok && transport !== "shared") {
      // Shared transport failed and downgraded to copy mode.
      postCopyFrame(width, height, strideBytes, pixels);
    }
    return true;
  }

  postCopyFrame(width, height, strideBytes, pixels);
  return true;
}

function presentScanoutFrame(): void {
  // Prefer the unified display scanout APIs when present, but fall back to the legacy VGA exports
  // for older WASM builds.
  if (tryPresentScanoutFrame("display")) return;
  void tryPresentScanoutFrame("vga");
}

function tick(): void {
  const m = machine;
  if (!m) return;

  const anyMachine = m as unknown as Record<string, unknown>;
  const runSlice = anyMachine.run_slice ?? anyMachine.runSlice;
  if (typeof runSlice !== "function") {
    throw new Error("Machine missing run_slice/runSlice export.");
  }
  const exit = (runSlice as (maxInsts: number) => unknown).call(m, 50_000);
  const detail = (exit as unknown as { detail?: string }).detail;
  if (typeof detail === "string" && detail !== lastExitDetail) {
    lastExitDetail = detail;
    post({ type: "machineVga.status", detail });
  }

  // Avoid copying large serial buffers into JS when empty.
  const lenFn = anyMachine.serial_output_len ?? anyMachine.serialOutputLen;
  const shouldReadSerial = (() => {
    if (typeof lenFn !== "function") return true;
    try {
      const n = (lenFn as () => number).call(m);
      return typeof n === "number" && Number.isFinite(n) && n > 0;
    } catch {
      return true;
    }
  })();

  if (shouldReadSerial) {
    const serialOutput = anyMachine.serial_output ?? anyMachine.serialOutput;
    if (typeof serialOutput !== "function") {
      throw new Error("Machine missing serial_output/serialOutput export.");
    }
    const serialBytes = (serialOutput as () => unknown).call(m);
    if (serialBytes instanceof Uint8Array && serialBytes.byteLength > 0) {
      // Prefer transferring the buffer for standalone ArrayBuffers, but avoid throwing if the
      // underlying memory is non-transferable (e.g. a WebAssembly.Memory view).
      const buf = serialBytes.buffer;
      if (
        buf instanceof ArrayBuffer &&
        serialBytes.byteOffset === 0 &&
        serialBytes.byteLength === buf.byteLength
      ) {
        try {
          post({ type: "machineVga.serial", data: serialBytes } satisfies MachineVgaWorkerSerialMessage, [buf]);
        } catch {
          post({ type: "machineVga.serial", data: serialBytes } satisfies MachineVgaWorkerSerialMessage);
        }
      } else {
        post({ type: "machineVga.serial", data: serialBytes } satisfies MachineVgaWorkerSerialMessage);
      }
    }
  }

  presentScanoutFrame();

  try {
    (exit as unknown as { free?: () => void }).free?.();
  } catch {
    // ignore
  }
}

async function start(msg: MachineVgaWorkerStartMessage): Promise<void> {
  stop();

  // Prefer single-threaded WASM for this standalone worker demo. It avoids requiring
  // crossOriginIsolated + shared WebAssembly.Memory.
  //
  // Allocate an explicit memory sized to the runtime-reserved region (128MiB) so the Rust heap
  // has enough room even if wasm-bindgen's default init uses a tiny memory.
  //
  // If the wasm build ignores imported memory (old toolchain output), fall back to wasm-bindgen's
  // default init so we still get a working `wasmMemory` handle for ptr/len scanout reads.
  let init: Awaited<ReturnType<typeof initWasmForContext>>;
  try {
    if (!preferredWasmMemory) {
      const pages = RUNTIME_RESERVED_BYTES / WASM_PAGE_BYTES;
      preferredWasmMemory = new WebAssembly.Memory({ initial: pages, maximum: pages });
    }
    init = await initWasmForContext({ variant: "single", memory: preferredWasmMemory });
    assertWasmMemoryWiring({ api: init.api, memory: preferredWasmMemory, context: "machine_vga.worker" });
  } catch (err) {
    console.warn("[machine_vga.worker] Failed to init single-threaded WASM with a preallocated memory; falling back:", err);
    init = await initWasmForContext();
  }

  api = init.api;
  wasmMemory = init.wasmMemory ?? null;

  if (!api.Machine) {
    throw new Error("Machine export unavailable in this WASM build.");
  }

  const ramSizeBytes = typeof msg.ramSizeBytes === "number" ? msg.ramSizeBytes : 2 * 1024 * 1024;
  const enableAerogpuOverride = typeof msg.enableAerogpu === "boolean" ? msg.enableAerogpu : undefined;
  const enableVgaOverride = typeof msg.enableVga === "boolean" ? msg.enableVga : undefined;
  // `Machine.new_with_config` is optional across wasm builds. Stash the property in a local so
  // TypeScript can safely narrow before invoking it (property reads are not stable).
  const newWithConfig = api.Machine.new_with_config;
  const newWithCpuCount = api.Machine.new_with_cpu_count;
  const wantsGraphicsOverride = enableAerogpuOverride !== undefined || enableVgaOverride !== undefined;
  const cpuCount = (() => {
    const raw = msg.cpuCount;
    if (typeof raw !== "number") return 1;
    const n = Math.trunc(raw);
    if (!Number.isFinite(n) || n < 1 || n > 255) return 1;
    return n;
  })();
  machine =
    wantsGraphicsOverride && typeof newWithConfig === "function"
      ? (() => {
          const enableAerogpu =
            enableAerogpuOverride ?? (enableVgaOverride !== undefined ? !enableVgaOverride : false);
          return newWithConfig(
            ramSizeBytes >>> 0,
            enableAerogpu,
            enableVgaOverride,
            cpuCount !== 1 ? cpuCount : undefined,
          );
        })()
      : cpuCount !== 1 && typeof newWithCpuCount === "function"
        ? newWithCpuCount(ramSizeBytes >>> 0, cpuCount)
        : new api.Machine(ramSizeBytes >>> 0);
  const machineInstance = machine;
  if (!machineInstance) {
    throw new Error("Machine init failed (machine is null)");
  }
  const bootMessage = msg.message ?? "Hello from machine_vga.worker\\n";
  // Bochs VBE programming requires the legacy VGA/VBE device model. When VGA is absent, ignore any
  // requested VBE mode so we keep text-mode scanout via the `0xB8000` fallback.
  const vgaDevicePresent = (() => {
    try {
      const vgaWidth = machineInstance.vga_width;
      return typeof vgaWidth === "function" && vgaWidth.call(machineInstance) > 0;
    } catch {
      return false;
    }
  })();
  const vbeMode = vgaDevicePresent ? msg.vbeMode : undefined;
  let diskImage: Uint8Array;
  if (vbeMode && typeof vbeMode === "object" && typeof vbeMode.width === "number" && typeof vbeMode.height === "number") {
    const width = Math.trunc(vbeMode.width);
    const height = Math.trunc(vbeMode.height);
    const requiredBytes = width * height * 4;
    if (
      Number.isFinite(width) &&
      Number.isFinite(height) &&
      width > 0 &&
      height > 0 &&
      Number.isFinite(requiredBytes) &&
      requiredBytes > 0 &&
      requiredBytes <= MAX_FRAME_BYTES
    ) {
      diskImage = buildVbeBootSector({ message: bootMessage, width, height });
    } else {
      diskImage = buildSerialBootSector(bootMessage);
    }
  } else {
    diskImage = buildSerialBootSector(bootMessage);
  }
  machineInstance.set_disk_image(diskImage);
  machineInstance.reset();

  // Prefer shared-buffer transport when supported; otherwise fall back to copy frames.
  transport = ensureSharedFramebuffer() ? "shared" : "copy";

  post({
    type: "machineVga.ready",
    transport,
    ...(transport === "shared" && sharedSab ? { framebuffer: sharedSab } : {}),
  } satisfies MachineVgaWorkerReadyMessage);

  tickTimer = ctx.setInterval(() => {
    try {
      tick();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      post({ type: "machineVga.error", message } satisfies MachineVgaWorkerErrorMessage);
      stop();
    }
  }, 50);
  (tickTimer as unknown as { unref?: () => void }).unref?.();
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  // Treat messages as untrusted input from the main thread; parse defensively.
  //
  // Note: use a union here (not an intersection) so `msg.type` remains a usable discriminant.
  const msg = ev.data as Partial<MachineVgaWorkerIncomingMessage> | null | undefined;
  if (!msg || typeof msg.type !== "string") return;

  if (msg.type === "machineVga.stop") {
    stop();
    return;
  }

  if (msg.type === "machineVga.start") {
    void start(msg as MachineVgaWorkerStartMessage).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      post({ type: "machineVga.error", message } satisfies MachineVgaWorkerErrorMessage);
      stop();
    });
    return;
  }

  const m = machine;
  if (!m) return;

  if ((msg as Partial<MachineVgaWorkerInjectBrowserKeyMessage>).type === "machineVga.inject_browser_key") {
    const input = msg as MachineVgaWorkerInjectBrowserKeyMessage;
    if (typeof input.code !== "string") return;
    const pressed = input.pressed === true;
    try {
      m.inject_browser_key(input.code, pressed);
    } catch {
      // ignore
    }
    return;
  }

  if ((msg as Partial<MachineVgaWorkerInjectMouseMotionMessage>).type === "machineVga.inject_mouse_motion") {
    const input = msg as MachineVgaWorkerInjectMouseMotionMessage;
    const dx = typeof input.dx === "number" ? input.dx : 0;
    const dy = typeof input.dy === "number" ? input.dy : 0;
    const wheel = typeof input.wheel === "number" ? input.wheel : 0;
    const motion = (m as unknown as { inject_mouse_motion?: unknown }).inject_mouse_motion;
    const ps2Motion = (m as unknown as { inject_ps2_mouse_motion?: unknown }).inject_ps2_mouse_motion;
    try {
      if (typeof motion === "function") {
        (motion as (dx: number, dy: number, wheel: number) => void).call(m, dx | 0, dy | 0, wheel | 0);
      } else if (typeof ps2Motion === "function") {
        // `inject_ps2_mouse_motion` expects +Y up; browser deltas are +Y down.
        const dyPs2 = negateI32Saturating(dy | 0);
        (ps2Motion as (dx: number, dy: number, wheel: number) => void).call(m, dx | 0, dyPs2, wheel | 0);
      }
    } catch {
      // ignore
    }
    return;
  }

  if ((msg as Partial<MachineVgaWorkerInjectMouseButtonMessage>).type === "machineVga.inject_mouse_button") {
    const input = msg as MachineVgaWorkerInjectMouseButtonMessage;
    const button = typeof input.button === "number" ? input.button : -1;
    const pressed = input.pressed === true;
    const fn = (m as unknown as { inject_mouse_button?: unknown }).inject_mouse_button;
    try {
      if (typeof fn === "function") {
        (fn as (button: number, pressed: boolean) => void).call(m, button | 0, pressed);
      }
    } catch {
      // ignore
    }
    return;
  }
};
