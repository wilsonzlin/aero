import { ringCtrl } from "../ipc/layout";
import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_STATE_U32_LEN,
  ScanoutStateIndex,
} from "../ipc/scanout_state";
import { CURSOR_FORMAT_B8G8R8A8, CURSOR_STATE_U32_LEN, CursorStateIndex } from "../ipc/cursor_state";
import {
  CONTROL_BYTES,
  STATUS_OFFSET_BYTES,
  STATUS_INTS,
  StatusIndex,
  WORKER_ROLES,
  ringRegionsForWorker,
  type SharedMemorySegments,
} from "./shared_layout";

const WASM_PAGE_BYTES = 64 * 1024;

function bytesToPages(bytes: number): number {
  return Math.ceil(bytes / WASM_PAGE_BYTES);
}

function initRing(control: SharedArrayBuffer, byteOffset: number, byteLength: number): void {
  const capacityBytes = byteLength - ringCtrl.BYTES;
  new Int32Array(control, byteOffset, ringCtrl.WORDS).set([0, 0, 0, capacityBytes]);
}

function initControlRings(control: SharedArrayBuffer): void {
  for (const role of WORKER_ROLES) {
    const regions = ringRegionsForWorker(role);
    initRing(control, regions.command.byteOffset, regions.command.byteLength);
    initRing(control, regions.event.byteOffset, regions.event.byteLength);
  }
}

/**
 * Allocate the minimal shared-memory segments needed to boot a runtime worker in harnesses / diagnostic pages.
 *
 * Unlike the full runtime allocator (`allocateSharedMemorySegments`), this intentionally does **not**
 * reserve a large wasm32 runtime region. It exists for pages that only need:
 * - a SharedArrayBuffer-backed guest RAM region (via shared WebAssembly.Memory), and
 * - a shared `ScanoutState` descriptor for WDDM scanout presentation.
 */
export function allocateHarnessSharedMemorySegments(opts: {
  guestRamBytes: number;
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes?: number;
  ioIpcBytes?: number;
  vramBytes?: number;
}): SharedMemorySegments {
  const guestRamBytes = Math.max(0, Math.trunc(opts.guestRamBytes));
  const pages = bytesToPages(guestRamBytes);
  if (pages <= 0) {
    throw new Error(`guestRamBytes must be > 0 (got ${guestRamBytes})`);
  }

  const guestMemory = new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
  const guestBuffer = guestMemory.buffer as unknown as ArrayBufferLike;
  if (!(guestBuffer instanceof SharedArrayBuffer)) {
    throw new Error("Shared WebAssembly.Memory is unavailable (memory.buffer is not a SharedArrayBuffer).");
  }

  const control = new SharedArrayBuffer(CONTROL_BYTES);
  const status = new Int32Array(control, STATUS_OFFSET_BYTES, STATUS_INTS);
  Atomics.store(status, StatusIndex.GuestBase, 0);
  Atomics.store(status, StatusIndex.GuestSize, guestRamBytes | 0);
  Atomics.store(status, StatusIndex.RuntimeReserved, 0);
  initControlRings(control);

  const scanoutState = new SharedArrayBuffer(SCANOUT_STATE_U32_LEN * 4);
  const scanoutWords = new Int32Array(scanoutState, 0, SCANOUT_STATE_U32_LEN);
  Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_LEGACY_TEXT);
  Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_LO, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.BASE_PADDR_HI, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.WIDTH, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.HEIGHT, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.PITCH_BYTES, 0);
  Atomics.store(scanoutWords, ScanoutStateIndex.FORMAT, SCANOUT_FORMAT_B8G8R8X8);

  const cursorState = new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4);
  const cursorWords = new Int32Array(cursorState, 0, CURSOR_STATE_U32_LEN);
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

  const vramBytes = Math.max(0, Math.trunc(opts.vramBytes ?? 0));
  const vram = vramBytes > 0 ? new SharedArrayBuffer(vramBytes) : undefined;
  const ioIpcBytes = Math.max(0, Math.trunc(opts.ioIpcBytes ?? 0));
  const ioIpc = new SharedArrayBuffer(ioIpcBytes);

  return {
    control,
    guestMemory,
    vram,
    ioIpc,
    sharedFramebuffer: opts.sharedFramebuffer,
    sharedFramebufferOffsetBytes: opts.sharedFramebufferOffsetBytes ?? 0,
    scanoutState,
    scanoutStateOffsetBytes: 0,
    cursorState,
    cursorStateOffsetBytes: 0,
  };
}
