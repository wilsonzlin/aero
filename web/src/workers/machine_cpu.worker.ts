/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { UART_COM1 } from "../io/devices/uart16550";
import { InputEventType } from "../input/event_queue";
import { negateI32Saturating } from "../input/int32";
import { chooseKeyboardInputBackend, chooseMouseInputBackend, type InputBackend } from "../input/input_backend_selection";
import { encodeInputBackendStatus } from "../input/input_backend_status";
import { hidConsumerUsageToLinuxKeyCode, hidUsageToLinuxKeyCode } from "../io/devices/virtio_input";
import {
  DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES,
  normalizeSetBootDisksMessage,
  type SetBootDisksMessage,
} from "../runtime/boot_disks_protocol";
import { planMachineBootDiskAttachment } from "../runtime/machine_disk_attach";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  type AerogpuCompleteFenceMessage,
  type AerogpuSubmitMessage,
  ErrorCode,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { CURSOR_STATE_BYTE_LEN } from "../ipc/cursor_state";
import { SCANOUT_STATE_BYTE_LEN } from "../ipc/scanout_state";
import { openFileHandle } from "../platform/opfs";
import {
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  StatusIndex,
  type GuestRamLayout,
  type SharedMemorySegments,
  type WorkerRole,
} from "../runtime/shared_layout";
import { u32Delta } from "../utils/u32";
import {
  restoreMachineSnapshotAndReattachDisks,
  restoreMachineSnapshotFromOpfsAndReattachDisks,
} from "../runtime/machine_snapshot_disks";
import {
  serializeVmSnapshotError,
  type VmSnapshotErr,
  type VmSnapshotMachineRestoreFromOpfsMessage,
  type VmSnapshotMachineRestoredMessage,
  type VmSnapshotMachineSaveToOpfsMessage,
  type VmSnapshotMachineSavedMessage,
  type VmSnapshotOk,
  type VmSnapshotPauseMessage,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumeMessage,
  type VmSnapshotResumedMessage,
  type VmSnapshotSerializedError,
} from "../runtime/snapshot_protocol";
import type { WasmApi } from "../runtime/wasm_loader";
import { diskMetaToOpfsCowPaths } from "../storage/opfs_paths";
import { INPUT_BATCH_HEADER_WORDS, INPUT_BATCH_WORDS_PER_EVENT, validateInputBatchBuffer } from "./io_input_batch";
import { MAX_INPUT_BATCH_RECYCLE_BYTES, shouldRecycleInputBatchBuffer } from "./input_batch_recycle_guard";

function toArrayBufferUint8(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, but OPFS write streams
  // are typed to accept only ArrayBuffer-backed views.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes);
}

/**
 * Canonical `api.Machine` CPU worker entrypoint.
 *
 * Runs the canonical wasm `api.Machine` runtime for `vmRuntime === "machine"`, driven by the
 * coordinator ring buffers (mirrors `cpu.worker.ts` command semantics).
 *
 * Node `worker_threads` integration tests execute TypeScript sources directly (no Vite transforms)
 * and typically do not have wasm-pack outputs available. WASM initialization is therefore
 * best-effort and skipped entirely under Node.
 */

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: WorkerRole = "cpu";
let status: Int32Array | null = null;
let commandRing: RingBuffer | null = null;
let eventRing: RingBuffer | null = null;
let ioIpcSab: SharedArrayBuffer | null = null;
let guestLayout: GuestRamLayout | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;
let networkWanted = false;
let networkAttached = false;

let running = false;
let started = false;
let vmSnapshotPaused = false;
let machineBusy = false;
const machineIdleWaiters: Array<() => void> = [];
let runLoopWakeResolve: (() => void) | null = null;
let runLoopWakePromise: Promise<void> | null = null;

function setMachineBusy(busy: boolean): void {
  machineBusy = busy;
  if (busy) return;
  if (machineIdleWaiters.length === 0) return;
  const waiters = machineIdleWaiters.splice(0, machineIdleWaiters.length);
  for (const resolve of waiters) {
    try {
      resolve();
    } catch {
      // ignore
    }
  }
}

function wakeRunLoop(): void {
  const resolve = runLoopWakeResolve;
  if (resolve) {
    try {
      resolve();
    } catch {
      // ignore
    }
  }
  runLoopWakeResolve = null;
  runLoopWakePromise = null;
}

function ensureRunLoopWakePromise(): Promise<void> {
  const existing = runLoopWakePromise;
  if (existing) return existing;
  runLoopWakePromise = new Promise((resolve) => {
    runLoopWakeResolve = resolve;
  });
  return runLoopWakePromise;
}

// Boot disk selection (shared protocol with the legacy IO worker).
//
// In `vmRuntime="machine"` mode the coordinator sends boot disks to this CPU worker so it can
// attach them to the synchronous Rust storage controllers (OPFS-only). Validate selections early
// so unsupported configs (IDB backend, remote streaming, unsupported formats) fail fast.
let pendingBootDisks: SetBootDisksMessage | null = null;

// Canonical machine BIOS boot device policy for the next reset.
//
// Normative flows are defined in `docs/05-storage-topology-win7.md`:
// - install: prefer CD-ROM (install ISO mounted)
// - normal: boot HDD
type MachineCpuBootDevice = "hdd" | "cdrom";
type MachineCpuBootDeviceSelectedMessage = { type: "machineCpu.bootDeviceSelected"; bootDevice: MachineCpuBootDevice };
type MachineCpuBootDeviceActiveMessage = { type: "machineCpu.bootDeviceActive"; bootDevice: MachineCpuBootDevice };
type MachineCpuBootConfigMessage = {
  type: "machineCpu.bootConfig";
  bootDrive: number;
  cdBootDrive: number;
  bootFromCdIfPresent: boolean;
};
let pendingBootDevice: MachineCpuBootDevice = "hdd";
// Last boot disk selection successfully applied to the Machine. Used to decide whether an HDD is
// present when handling guest resets (avoid looping on install media).
let currentBootDisks: SetBootDisksMessage | null = null;

type InputBatchMessage = {
  type: "in:input-batch";
  buffer: ArrayBuffer;
  recycle?: boolean;
};

type InputBatchRecycleMessage = {
  type: "in:input-batch-recycle";
  buffer: ArrayBuffer;
};

type MachineSnapshotSerializedError = VmSnapshotSerializedError;
type MachineSnapshotResultOk = VmSnapshotOk;
type MachineSnapshotResultErr = VmSnapshotErr;

type MachineSnapshotRestoreFromOpfsMessage = {
  kind: "machine.snapshot.restoreFromOpfs";
  requestId: number;
  path: string;
};

type MachineSnapshotRestoreMessage = {
  kind: "machine.snapshot.restore";
  requestId: number;
  bytes: ArrayBuffer;
};

type MachineSnapshotRestoredMessage =
  | ({ kind: "machine.snapshot.restored"; requestId: number } & MachineSnapshotResultOk)
  | ({ kind: "machine.snapshot.restored"; requestId: number } & MachineSnapshotResultErr);
type PendingMachineOp =
  | { kind: "machine.restoreFromOpfs"; requestId: number; path: string }
  | { kind: "machine.restore"; requestId: number; bytes: Uint8Array }
  | { kind: "vm.machine.saveToOpfs"; requestId: number; path: string }
  | { kind: "vm.machine.restoreFromOpfs"; requestId: number; path: string };

const pendingMachineOps: PendingMachineOp[] = [];

let wasmApi: WasmApi | null = null;
let machine: InstanceType<WasmApi["Machine"]> | null = null;
const pendingAerogpuFenceCompletions: bigint[] = [];
let aerogpuBridgeEnabled = false;

// Test-only (Node `worker_threads`): allow integration tests to exercise the AeroGPU submission
// drain/forwarding path without loading WASM by queuing synthetic submissions on the dummy Machine.
type TestDummyAerogpuSubmission = {
  cmdStream: Uint8Array;
  allocTable?: Uint8Array | null;
  signalFence: bigint;
  contextId: number;
  flags?: number;
  engineId?: number;
};
let testDummyAerogpuSubmissions: TestDummyAerogpuSubmission[] | null = null;
let testDummyNextRunExitKind: number | null = null;

function verifyWasmSharedStateLayout(
  m: InstanceType<WasmApi["Machine"]>,
  init: WorkerInitMessage,
  guestMemory: WebAssembly.Memory,
): void {
  // These shared-state headers are embedded at fixed offsets inside wasm linear memory so both:
  // - Rust device models (running inside the Machine wasm module), and
  // - JS workers (GPU presenter, etc)
  // can access them without dedicated SharedArrayBuffer allocations.
  //
  // The offsets are calculated independently in:
  // - Rust (`crates/aero-wasm/src/lib.rs` + `runtime_alloc.rs`), and
  // - JS (`web/src/runtime/shared_layout.ts`).
  //
  // When JS/wasm assets are out of sync (stale wasm-pack output, mixed dev/prod bundles, etc),
  // the Machine will publish scanout/cursor updates into a different region than the JS workers
  // are reading. Detect that mismatch early and surface a warning with actionable context.
  try {
    const guestSab = guestMemory.buffer as unknown as SharedArrayBuffer;
    const expectedScanoutPtr =
      typeof init.scanoutStateOffsetBytes === "number" ? (init.scanoutStateOffsetBytes >>> 0) : null;
    const expectedCursorPtr =
      typeof init.cursorStateOffsetBytes === "number" ? (init.cursorStateOffsetBytes >>> 0) : null;
    // Newer coordinators omit `scanoutState`/`cursorState` from the init message to avoid passing
    // multiple aliases of the same SharedArrayBuffer through structured clone (Firefox has been
    // observed to corrupt such messages). When an offset is provided, treat the guest memory
    // backing store as the implicit shared-state buffer.
    const scanoutSab =
      init.scanoutState ??
      (expectedScanoutPtr !== null && expectedScanoutPtr > 0 ? (guestSab as unknown as SharedArrayBuffer) : undefined);
    const cursorSab =
      init.cursorState ??
      (expectedCursorPtr !== null && expectedCursorPtr > 0 ? (guestSab as unknown as SharedArrayBuffer) : undefined);
    const scanoutEmbedded = scanoutSab instanceof SharedArrayBuffer && scanoutSab === guestSab;
    const cursorEmbedded = cursorSab instanceof SharedArrayBuffer && cursorSab === guestSab;

    if (typeof m.scanout_state_ptr === "function") {
      const got = m.scanout_state_ptr() >>> 0;
      if (!scanoutSab) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared scanout state is missing from WorkerInitMessage. ` +
            `Machine publishes scanout state at linear offset ${got}, but this worker was initialized without scanoutState. ` +
            "WDDM scanout selection may be broken; ensure the coordinator passes scanoutState (typically guestMemory.buffer).",
        );
      } else if (!scanoutEmbedded) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared scanout state is not embedded in guestMemory.buffer. ` +
            `Machine publishes scanout state at linear offset ${got}, but WorkerInitMessage.scanoutState does not alias guest memory. ` +
            "Scanout updates will be invisible to other workers; ensure the runtime embeds scanoutState in guestMemory.buffer (shared_layout.ts).",
        );
      } else if (expectedScanoutPtr !== null && got !== expectedScanoutPtr) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared scanout state offset mismatch: js=${expectedScanoutPtr} wasm=${got}. ` +
            "This usually means the worker is running a stale wasm-pack build. Rebuild/reload the WASM assets.",
        );
      }
    }

    if (typeof m.scanout_state_len_bytes === "function") {
      const len = m.scanout_state_len_bytes() >>> 0;
      if (len !== 0 && len !== (SCANOUT_STATE_BYTE_LEN >>> 0)) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared scanout state length mismatch: js=${SCANOUT_STATE_BYTE_LEN} wasm=${len}. ` +
            "Update/rebuild the WASM assets to match the web runtime.",
        );
      }
    }

    if (typeof m.cursor_state_ptr === "function") {
      const got = m.cursor_state_ptr() >>> 0;
      if (!cursorSab) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared cursor state is missing from WorkerInitMessage. ` +
            `Machine publishes cursor state at linear offset ${got}, but this worker was initialized without cursorState. ` +
            "Hardware cursor updates may be broken; ensure the coordinator passes cursorState (typically guestMemory.buffer).",
        );
      } else if (!cursorEmbedded) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared cursor state is not embedded in guestMemory.buffer. ` +
            `Machine publishes cursor state at linear offset ${got}, but WorkerInitMessage.cursorState does not alias guest memory. ` +
            "Cursor updates will be invisible to other workers; ensure the runtime embeds cursorState in guestMemory.buffer (shared_layout.ts).",
        );
      } else if (expectedCursorPtr !== null && got !== expectedCursorPtr) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared cursor state offset mismatch: js=${expectedCursorPtr} wasm=${got}. ` +
            "This usually means the worker is running a stale wasm-pack build. Rebuild/reload the WASM assets.",
        );
      }
    }

    if (typeof m.cursor_state_len_bytes === "function") {
      const len = m.cursor_state_len_bytes() >>> 0;
      if (len !== 0 && len !== (CURSOR_STATE_BYTE_LEN >>> 0)) {
        // eslint-disable-next-line no-console
        console.warn(
          `[machine_cpu.worker] Shared cursor state length mismatch: js=${CURSOR_STATE_BYTE_LEN} wasm=${len}. ` +
            "Update/rebuild the WASM assets to match the web runtime.",
        );
      }
    }
  } catch {
    // ignore
  }
}

const HEARTBEAT_INTERVAL_MS = 250;
const RUN_SLICE_MAX_INSTS = 50_000;
// `Machine::run_slice` advances guest time by 1ms when the CPU is halted (`HLT`) so timer
// interrupts can make progress. If we immediately re-enter `run_slice` in a tight loop, we'd
// effectively advance guest time far faster than real time while idle. Throttle halted loops to
// ~1kHz to keep guest wallclock/timeout behavior sane.
const HALTED_RUN_SLICE_DELAY_MS = 1;

const BIOS_DRIVE_HDD0 = 0x80;
const BIOS_DRIVE_CD0 = 0xe0;
const BIOS_DRIVE_CD_LAST = 0xef;

const MAX_INPUT_BATCHES_PER_TICK = 8;
const MAX_QUEUED_INPUT_BATCH_BYTES = MAX_INPUT_BATCH_RECYCLE_BYTES;
let queuedInputBatchBytes = 0;
const queuedInputBatches: Array<{ buffer: ArrayBuffer; recycle: boolean }> = [];

// Avoid per-event allocations when falling back to `inject_keyboard_bytes` (older WASM builds).
// Preallocate small scancode buffers for len=1..4.
const packedScancodeScratch = [new Uint8Array(0), new Uint8Array(1), new Uint8Array(2), new Uint8Array(3), new Uint8Array(4)];

// Best-effort held-state telemetry. We keep the same indices/semantics as `io.worker.ts` so the
// input diagnostics panel can detect stuck keys/buttons in machine runtime.
const pressedKeyboardHidUsages = new Uint8Array(256);
let pressedKeyboardHidUsageCount = 0;
const pressedConsumerUsages = new Uint8Array(0x0400);
let pressedConsumerUsageCount = 0;
let mouseButtonsMask = 0;
let virtioMouseButtonsInjectedMask = 0;

let keyboardInputBackend: InputBackend = "ps2";
let mouseInputBackend: InputBackend = "ps2";
let keyboardUsbOk = false;
let mouseUsbOk = false;

const warnedForcedKeyboardBackendUnavailable = new Set<string>();
const warnedForcedMouseBackendUnavailable = new Set<string>();

// Linux input button codes (EV_KEY) used by virtio-input. Kept in sync with `web/src/io/devices/virtio_input.ts`.
const VIRTIO_BTN_LEFT = 0x110;
const VIRTIO_BTN_RIGHT = 0x111;
const VIRTIO_BTN_MIDDLE = 0x112;
const VIRTIO_BTN_SIDE = 0x113;
const VIRTIO_BTN_EXTRA = 0x114;
const VIRTIO_BTN_FORWARD = 0x115;
const VIRTIO_BTN_BACK = 0x116;
const VIRTIO_BTN_TASK = 0x117;

function updatePressedKeyboardHidUsage(usage: number, pressed: boolean): void {
  const idx = usage & 0xff;
  const prev = pressedKeyboardHidUsages[idx] !== 0;
  if (pressed) {
    if (!prev) {
      pressedKeyboardHidUsages[idx] = 1;
      pressedKeyboardHidUsageCount = Math.min(256, pressedKeyboardHidUsageCount + 1);
    }
    return;
  }
  if (prev) {
    pressedKeyboardHidUsages[idx] = 0;
    pressedKeyboardHidUsageCount = Math.max(0, pressedKeyboardHidUsageCount - 1);
  }
}

function updatePressedConsumerUsage(usageId: number, pressed: boolean): void {
  const u = usageId & 0xffff;
  if (u >= pressedConsumerUsages.length) return;
  const prev = pressedConsumerUsages[u] !== 0;
  if (pressed) {
    if (!prev) {
      pressedConsumerUsages[u] = 1;
      pressedConsumerUsageCount = Math.min(pressedConsumerUsages.length, pressedConsumerUsageCount + 1);
    }
    return;
  }
  if (prev) {
    pressedConsumerUsages[u] = 0;
    pressedConsumerUsageCount = Math.max(0, pressedConsumerUsageCount - 1);
  }
}

function initInputDiagnosticsTelemetry(): void {
  // The legacy IO worker publishes "current input backend + held state" telemetry into the shared
  // status SAB for the input diagnostics panel. In `vmRuntime="machine"` mode, the IO worker runs
  // in host-only stub mode, so the machine CPU worker must initialize these fields.
  //
  // Note: these are best-effort debug fields only; the emulator must keep running even if Atomics
  // stores fail for any reason.
  pressedKeyboardHidUsages.fill(0);
  pressedKeyboardHidUsageCount = 0;
  pressedConsumerUsages.fill(0);
  pressedConsumerUsageCount = 0;
  mouseButtonsMask = 0;
  virtioMouseButtonsInjectedMask = 0;
  keyboardInputBackend = "ps2";
  mouseInputBackend = "ps2";
  keyboardUsbOk = false;
  mouseUsbOk = false;
  warnedForcedKeyboardBackendUnavailable.clear();
  warnedForcedMouseBackendUnavailable.clear();

  ioInputLatencyMaxWindowStartMs = 0;
  ioInputBatchSendLatencyEwmaUs = 0;
  ioInputBatchSendLatencyMaxUs = 0;
  ioInputEventLatencyEwmaUs = 0;
  ioInputEventLatencyMaxUs = 0;

  const st = status;
  if (!st) return;
  try {
    // Reset counters as well: the shared status SAB may outlive a worker instance when the VM is
    // reset/restarted in-place.
    Atomics.store(st, StatusIndex.IoInputBatchCounter, 0);
    Atomics.store(st, StatusIndex.IoInputEventCounter, 0);
    Atomics.store(st, StatusIndex.IoInputBatchReceivedCounter, 0);
    Atomics.store(st, StatusIndex.IoInputBatchDropCounter, 0);
    Atomics.store(st, StatusIndex.IoKeyboardBackendSwitchCounter, 0);
    Atomics.store(st, StatusIndex.IoMouseBackendSwitchCounter, 0);

    // 0 = ps2 (see `web/src/input/input_backend_status.ts`).
    Atomics.store(st, StatusIndex.IoInputKeyboardBackend, 0);
    Atomics.store(st, StatusIndex.IoInputMouseBackend, 0);
    Atomics.store(st, StatusIndex.IoInputVirtioKeyboardDriverOk, 0);
    Atomics.store(st, StatusIndex.IoInputVirtioMouseDriverOk, 0);
    Atomics.store(st, StatusIndex.IoInputUsbKeyboardOk, 0);
    Atomics.store(st, StatusIndex.IoInputUsbMouseOk, 0);
    Atomics.store(st, StatusIndex.IoInputKeyboardHeldCount, 0);
    Atomics.store(st, StatusIndex.IoInputMouseButtonsHeldMask, 0);
    Atomics.store(st, StatusIndex.IoInputKeyboardLedsUsb, 0);
    Atomics.store(st, StatusIndex.IoInputKeyboardLedsVirtio, 0);
    Atomics.store(st, StatusIndex.IoInputKeyboardLedsPs2, 0);

    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyUs, 0);
    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyEwmaUs, 0);
    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyMaxUs, 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyAvgUs, 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyEwmaUs, 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyMaxUs, 0);
  } catch {
    // ignore (best-effort)
  }
}

function invalidateInputStateAfterSnapshotRestore(): void {
  // VM snapshot restore rewinds guest time + guest device state, but host-side input held-state
  // tracking (used to gate backend switching) is not part of the snapshot format. Reset it here so
  // the next input batch can re-sync safely without leaving keys/buttons stuck across backends.
  pressedKeyboardHidUsages.fill(0);
  pressedKeyboardHidUsageCount = 0;
  pressedConsumerUsages.fill(0);
  pressedConsumerUsageCount = 0;
  mouseButtonsMask = 0;
  // Invalidate virtio mouse transition cache (see `injectVirtioMouseButtons`).
  virtioMouseButtonsInjectedMask = -1;
  keyboardInputBackend = "ps2";
  mouseInputBackend = "ps2";
  keyboardUsbOk = false;
  mouseUsbOk = false;
}

function safeCallBool(thisArg: unknown, fn: unknown): boolean {
  if (typeof fn !== "function") return false;
  try {
    return !!(fn as () => unknown).call(thisArg);
  } catch {
    return false;
  }
}

function safeCallU32(thisArg: unknown, fn: unknown): number {
  if (typeof fn !== "function") return 0;
  try {
    const raw = (fn as () => unknown).call(thisArg) as unknown;
    if (typeof raw === "number") {
      return Number.isFinite(raw) ? raw >>> 0 : 0;
    }
    if (typeof raw === "bigint") {
      if (raw < 0n) return 0;
      return Number(raw & 0xffff_ffffn);
    }
    return 0;
  } catch {
    return 0;
  }
}

function injectVirtioMouseButtons(m: unknown, nextMask: number): void {
  const injectFn = (m as unknown as { inject_virtio_mouse_button?: unknown; inject_virtio_button?: unknown }).inject_virtio_mouse_button;
  const fallback = (m as unknown as { inject_virtio_button?: unknown }).inject_virtio_button;
  const inject = typeof injectFn === "function" ? injectFn : typeof fallback === "function" ? fallback : null;
  if (!inject) {
    virtioMouseButtonsInjectedMask = nextMask & 0xff;
    return;
  }

  const prevRaw = virtioMouseButtonsInjectedMask;
  const prev = prevRaw & 0xff;
  const next = nextMask & 0xff;
  // Snapshot restore can rewind guest input state without rewinding this host-side button cache.
  // Invalidate it to a sentinel (see `invalidateInputStateAfterSnapshotRestore`) and treat it as
  // "unknown previous state" so the first post-restore click is not dropped.
  const delta = prevRaw === -1 ? 0xff : prev ^ next;
  if (delta === 0) {
    virtioMouseButtonsInjectedMask = next;
    return;
  }

  const call = (code: number, pressed: boolean) => {
    try {
      (inject as (btn: number, pressed: boolean) => void).call(m, code >>> 0, pressed);
    } catch {
      // ignore
    }
  };

  if (delta & 0x01) call(VIRTIO_BTN_LEFT, (next & 0x01) !== 0);
  if (delta & 0x02) call(VIRTIO_BTN_RIGHT, (next & 0x02) !== 0);
  if (delta & 0x04) call(VIRTIO_BTN_MIDDLE, (next & 0x04) !== 0);
  if (delta & 0x08) call(VIRTIO_BTN_SIDE, (next & 0x08) !== 0);
  if (delta & 0x10) call(VIRTIO_BTN_EXTRA, (next & 0x10) !== 0);
  if (delta & 0x20) call(VIRTIO_BTN_FORWARD, (next & 0x20) !== 0);
  if (delta & 0x40) call(VIRTIO_BTN_BACK, (next & 0x40) !== 0);
  if (delta & 0x80) call(VIRTIO_BTN_TASK, (next & 0x80) !== 0);

  virtioMouseButtonsInjectedMask = next;
}

function maybeUpdateKeyboardInputBackend(opts: { virtioKeyboardOk: boolean }): void {
  const m = machine;
  if (!m) return;

  const anyMachine = m as unknown as {
    inject_virtio_key?: unknown;
    inject_usb_hid_keyboard_usage?: unknown;
    usb_hid_keyboard_configured?: unknown;
  };

  keyboardUsbOk = safeCallBool(m, anyMachine.usb_hid_keyboard_configured);
  const virtioOk = opts.virtioKeyboardOk && typeof anyMachine.inject_virtio_key === "function";
  const usbOk = keyboardUsbOk && typeof anyMachine.inject_usb_hid_keyboard_usage === "function";

  const force = currentConfig?.forceKeyboardBackend;
  if (force && force !== "auto") {
    const forcedOk = force === "ps2" || (force === "virtio" && virtioOk) || (force === "usb" && usbOk);
    if (!forcedOk && !warnedForcedKeyboardBackendUnavailable.has(force)) {
      warnedForcedKeyboardBackendUnavailable.add(force);
      const reason =
        force === "virtio"
          ? typeof anyMachine.inject_virtio_key === "function"
            ? "virtio-input keyboard is not ready (DRIVER_OK not set)"
            : "virtio-input keyboard device is unavailable"
          : force === "usb"
            ? typeof anyMachine.inject_usb_hid_keyboard_usage !== "function"
              ? "synthetic USB HID keyboard device is unavailable"
              : "synthetic USB keyboard is not configured by the guest yet"
            : "requested backend is unavailable";
      const message = `[machine_cpu.worker] forceKeyboardBackend=${force} requested, but ${reason}; falling back to auto selection.`;
      // eslint-disable-next-line no-console
      console.warn(message);
      pushEvent({ kind: "log", level: "warn", message });
    }
  }

  const prevBackend = keyboardInputBackend;
  const nextBackend = chooseKeyboardInputBackend({
    current: keyboardInputBackend,
    keysHeld: pressedKeyboardHidUsageCount !== 0 || pressedConsumerUsageCount !== 0,
    virtioOk,
    usbOk,
    force,
  });
  if (nextBackend !== prevBackend) {
    // Low-overhead telemetry (u32 wrap semantics).
    if (status) Atomics.add(status, StatusIndex.IoKeyboardBackendSwitchCounter, 1);
  }
  keyboardInputBackend = nextBackend;
}

function maybeUpdateMouseInputBackend(opts: { virtioMouseOk: boolean }): void {
  const m = machine;
  if (!m) return;

  const anyMachine = m as unknown as {
    inject_ps2_mouse_motion?: unknown;
    inject_mouse_motion?: unknown;
    inject_mouse_buttons_mask?: unknown;
    inject_ps2_mouse_buttons?: unknown;
    inject_virtio_rel?: unknown;
    inject_virtio_mouse_rel?: unknown;
    inject_usb_hid_mouse_move?: unknown;
    inject_usb_hid_mouse_buttons?: unknown;
    usb_hid_mouse_configured?: unknown;
  };

  const ps2Available =
    typeof anyMachine.inject_ps2_mouse_motion === "function" ||
    typeof anyMachine.inject_mouse_motion === "function" ||
    typeof anyMachine.inject_mouse_buttons_mask === "function" ||
    typeof anyMachine.inject_ps2_mouse_buttons === "function";

  const usbConfigured = safeCallBool(m, anyMachine.usb_hid_mouse_configured);
  // Expose "configured" (not merely selected) status for diagnostics/HUDs.
  mouseUsbOk = usbConfigured;

  const virtioOk =
    opts.virtioMouseOk &&
    (typeof anyMachine.inject_virtio_mouse_rel === "function" || typeof anyMachine.inject_virtio_rel === "function");
  const usbOk = typeof anyMachine.inject_usb_hid_mouse_move === "function" && (!ps2Available || usbConfigured);

  const prevBackend = mouseInputBackend;
  const force = currentConfig?.forceMouseBackend;
  const nextBackend = chooseMouseInputBackend({
    current: mouseInputBackend,
    buttonsHeld: (mouseButtonsMask & 0xff) !== 0,
    virtioOk,
    // Use PS/2 injection until the synthetic USB mouse is configured; once configured, route via
    // the USB HID path to avoid duplicate devices in the guest.
    usbOk,
    force,
  });

  if (force && force !== "auto") {
    const forcedOk = force === "ps2" || (force === "virtio" && virtioOk) || (force === "usb" && usbOk);
    if (!forcedOk && !warnedForcedMouseBackendUnavailable.has(force)) {
      warnedForcedMouseBackendUnavailable.add(force);
      const reason =
        force === "virtio"
          ? typeof anyMachine.inject_virtio_mouse_rel === "function" || typeof anyMachine.inject_virtio_rel === "function"
            ? "virtio-input mouse is not ready (DRIVER_OK not set)"
            : "virtio-input mouse device is unavailable"
          : force === "usb"
            ? typeof anyMachine.inject_usb_hid_mouse_move !== "function"
              ? "synthetic USB HID mouse device is unavailable"
              : ps2Available && !usbConfigured
                ? "synthetic USB mouse is not configured by the guest yet"
                : "USB mouse backend is unavailable"
            : "requested backend is unavailable";
      const message = `[machine_cpu.worker] forceMouseBackend=${force} requested, but ${reason}; falling back to auto selection.`;
      // eslint-disable-next-line no-console
      console.warn(message);
      pushEvent({ kind: "log", level: "warn", message });
    }
  }

  if (nextBackend !== prevBackend) {
    // Low-overhead telemetry (u32 wrap semantics).
    if (status) Atomics.add(status, StatusIndex.IoMouseBackendSwitchCounter, 1);
  }

  // Optional extra robustness: when we *do* switch backends, send a "buttons=0" update to the
  // previous backend. This should be redundant because backend switching is gated on
  // `mouseButtonsMask===0`, but it provides a safety net in case a prior button-up was dropped.
  if (nextBackend !== prevBackend && (mouseButtonsMask & 0xff) === 0) {
    if (prevBackend === "virtio") {
      injectVirtioMouseButtons(m, 0);
    } else if (prevBackend === "usb") {
      const injectButtons = anyMachine.inject_usb_hid_mouse_buttons;
      if (typeof injectButtons === "function") {
        try {
          (injectButtons as (mask: number) => void).call(m, 0);
        } catch {
          // ignore
        }
      }
    } else {
      const injectButtons = anyMachine.inject_mouse_buttons_mask;
      if (typeof injectButtons === "function") {
        try {
          (injectButtons as (mask: number) => void).call(m, 0);
        } catch {
          // ignore
        }
      } else if (typeof anyMachine.inject_ps2_mouse_buttons === "function") {
        try {
          (anyMachine.inject_ps2_mouse_buttons as (buttons: number) => void).call(m, 0);
        } catch {
          // ignore
        }
      }
    }
  }

  mouseInputBackend = nextBackend;
}

function publishInputBackendStatus(opts: { virtioKeyboardOk: boolean; virtioMouseOk: boolean }): void {
  const st = status;
  if (!st) return;
  // These values are best-effort debug telemetry only; the emulator must keep running even if
  // Atomics stores fail for any reason.
  try {
    Atomics.store(st, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus(keyboardInputBackend));
    Atomics.store(st, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus(mouseInputBackend));
    Atomics.store(st, StatusIndex.IoInputVirtioKeyboardDriverOk, opts.virtioKeyboardOk ? 1 : 0);
    Atomics.store(st, StatusIndex.IoInputVirtioMouseDriverOk, opts.virtioMouseOk ? 1 : 0);
    Atomics.store(st, StatusIndex.IoInputUsbKeyboardOk, keyboardUsbOk ? 1 : 0);
    Atomics.store(st, StatusIndex.IoInputUsbMouseOk, mouseUsbOk ? 1 : 0);
    Atomics.store(st, StatusIndex.IoInputKeyboardHeldCount, (pressedKeyboardHidUsageCount + pressedConsumerUsageCount) | 0);
    Atomics.store(st, StatusIndex.IoInputMouseButtonsHeldMask, mouseButtonsMask & 0x1f);

    const m = machine;
    if (m) {
      const anyMachine = m as unknown as {
        usb_hid_keyboard_leds?: unknown;
        virtio_input_keyboard_leds?: unknown;
        ps2_keyboard_leds?: unknown;
      };
      Atomics.store(st, StatusIndex.IoInputKeyboardLedsUsb, safeCallU32(m, anyMachine.usb_hid_keyboard_leds) & 0x1f);
      Atomics.store(
        st,
        StatusIndex.IoInputKeyboardLedsVirtio,
        safeCallU32(m, anyMachine.virtio_input_keyboard_leds) & 0x1f,
      );
      Atomics.store(st, StatusIndex.IoInputKeyboardLedsPs2, safeCallU32(m, anyMachine.ps2_keyboard_leds) & 0x1f);
    } else {
      Atomics.store(st, StatusIndex.IoInputKeyboardLedsUsb, 0);
      Atomics.store(st, StatusIndex.IoInputKeyboardLedsVirtio, 0);
      Atomics.store(st, StatusIndex.IoInputKeyboardLedsPs2, 0);
    }
  } catch {
    // ignore (best-effort)
  }
}

function publishInputBackendStatusFromMachine(): void {
  // Best-effort status publishing for the input diagnostics panel.
  //
  // In legacy runtime, the IO worker is responsible for publishing backend selection + driver
  // readiness. In `vmRuntime="machine"`, the CPU worker owns the canonical `api.Machine`, so it is
  // the only place we can probe e.g. virtio-input driver readiness.
  const st = status;
  const m = machine;
  if (!st || !m) return;

  // Be defensive: older WASM builds may not expose these probes.
  const anyMachine = m as unknown as {
    virtio_input_keyboard_driver_ok?: unknown;
    virtio_input_mouse_driver_ok?: unknown;
  };
  const virtioKeyboardOk = safeCallBool(m, anyMachine.virtio_input_keyboard_driver_ok);
  const virtioMouseOk = safeCallBool(m, anyMachine.virtio_input_mouse_driver_ok);

  // Allow backend switching even when no input is flowing so diagnostics/HUDs
  // reflect driver readiness promptly (mirrors legacy io.worker.ts behavior).
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });
  publishInputBackendStatus({ virtioKeyboardOk, virtioMouseOk });
}

// End-to-end input latency telemetry (main thread capture -> CPU worker injection).
//
// Keep this in sync with the IO worker's input telemetry so debug HUDs (input diagnostics panel)
// report meaningful latency statistics in `vmRuntime="machine"` mode.
const INPUT_LATENCY_EWMA_ALPHA = 0.125; // 1/8 smoothing factor
const INPUT_LATENCY_MAX_WINDOW_MS = 1000;
let ioInputLatencyMaxWindowStartMs = 0;
let ioInputBatchSendLatencyEwmaUs = 0;
let ioInputBatchSendLatencyMaxUs = 0;
let ioInputEventLatencyEwmaUs = 0;
let ioInputEventLatencyMaxUs = 0;

const AEROSPARSE_HEADER_SIZE_BYTES = 64;
const AEROSPARSE_MAGIC = [0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52] as const; // "AEROSPAR"

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

type RunExitKindMap = Readonly<{
  Completed: number;
  Halted: number;
  ResetRequested: number;
  Assist: number;
  Exception: number;
  CpuExit: number;
}>;

// wasm-bindgen assigns discriminants in declaration order.
// Keep these defaults in sync with `crates/aero-wasm/src/lib.rs`.
let runExitKindMap: RunExitKindMap = {
  Completed: 0,
  Halted: 1,
  ResetRequested: 2,
  Assist: 3,
  Exception: 4,
  CpuExit: 5,
};

function post(msg: ProtocolMessage | ConfigAckMessage): void {
  ctx.postMessage(msg);
}

function postBootDeviceSelected(bootDevice: MachineCpuBootDevice): void {
  // Best-effort side-channel used by tests and debugging tools.
  const msg: MachineCpuBootDeviceSelectedMessage = { type: "machineCpu.bootDeviceSelected", bootDevice };
  try {
    ctx.postMessage(msg);
  } catch {
    // ignore
  }
}

function postBootDeviceActive(bootDevice: MachineCpuBootDevice): void {
  // Best-effort side-channel used by tests and debugging tools.
  const msg: MachineCpuBootDeviceActiveMessage = { type: "machineCpu.bootDeviceActive", bootDevice };
  try {
    ctx.postMessage(msg);
  } catch {
    // ignore
  }
}

function postBootConfig(bootDrive: number, cdBootDrive: number, bootFromCdIfPresent: boolean): void {
  // Best-effort side-channel used by tests and debugging tools.
  const msg: MachineCpuBootConfigMessage = { type: "machineCpu.bootConfig", bootDrive, cdBootDrive, bootFromCdIfPresent };
  try {
    ctx.postMessage(msg);
  } catch {
    // ignore
  }
}

function tryReadMachineActiveBootDevice(m: unknown): MachineCpuBootDevice | null {
  try {
    const fn = (m as unknown as { active_boot_device?: unknown }).active_boot_device ?? (m as unknown as { activeBootDevice?: unknown }).activeBootDevice;
    if (typeof fn !== "function") return null;
    const raw = (fn as () => unknown).call(m);
    if (typeof raw !== "number" || !Number.isFinite(raw)) return null;
    const value = raw | 0;

    // Prefer the wasm-bindgen enum object when available (avoids hard-coding discriminants).
    const enumObj = wasmApi?.MachineBootDevice as unknown;
    const anyEnum = enumObj as { Hdd?: unknown; Cdrom?: unknown } | undefined;
    const hddVal = typeof anyEnum?.Hdd === "number" ? (anyEnum.Hdd as number) : 0;
    const cdVal = typeof anyEnum?.Cdrom === "number" ? (anyEnum.Cdrom as number) : 1;

    if (value === cdVal) return "cdrom";
    if (value === hddVal) return "hdd";

    // Defensive fallback for older builds (or if enum bindings change).
    if (value === 1) return "cdrom";
    if (value === 0) return "hdd";
    return null;
  } catch {
    return null;
  }
}

function tryReadMachineBootDrive(m: unknown): number | null {
  try {
    const fn = (m as unknown as { boot_drive?: unknown }).boot_drive ?? (m as unknown as { bootDrive?: unknown }).bootDrive;
    if (typeof fn === "function") {
      const raw = (fn as () => unknown).call(m);
      if (typeof raw !== "number" || !Number.isFinite(raw) || !Number.isSafeInteger(raw)) return null;
      return raw;
    }
  } catch {
    // ignore
  }

  // Back-compat: some builds expose only `boot_device(MachineBootDevice::<...>)`, which is enough
  // to map to the canonical BIOS drive numbers.
  try {
    const fn = (m as unknown as { boot_device?: unknown }).boot_device ?? (m as unknown as { bootDevice?: unknown }).bootDevice;
    if (typeof fn !== "function") return null;
    const raw = (fn as () => unknown).call(m);
    if (typeof raw !== "number" || !Number.isFinite(raw)) return null;
    const value = raw | 0;

    // Prefer the wasm-bindgen enum object when available (avoids hard-coding discriminants).
    const enumObj = wasmApi?.MachineBootDevice as unknown;
    const anyEnum = enumObj as { Hdd?: unknown; Cdrom?: unknown } | undefined;
    const hddVal = typeof anyEnum?.Hdd === "number" ? (anyEnum.Hdd as number) : 0;
    const cdVal = typeof anyEnum?.Cdrom === "number" ? (anyEnum.Cdrom as number) : 1;

    if (value === cdVal) return BIOS_DRIVE_CD0;
    if (value === hddVal) return BIOS_DRIVE_HDD0;

    // Defensive fallback for older builds (or if enum bindings change).
    if (value === 1) return BIOS_DRIVE_CD0;
    if (value === 0) return BIOS_DRIVE_HDD0;
    return null;
  } catch {
    return null;
  }
}

function tryReadMachineCdBootDrive(m: unknown): number | null {
  try {
    const fn = (m as unknown as { cd_boot_drive?: unknown }).cd_boot_drive ?? (m as unknown as { cdBootDrive?: unknown }).cdBootDrive;
    if (typeof fn !== "function") return null;
    const raw = (fn as () => unknown).call(m);
    if (typeof raw !== "number" || !Number.isFinite(raw) || !Number.isSafeInteger(raw)) return null;
    return raw;
  } catch {
    return null;
  }
}

function tryReadMachineBootFromCdIfPresent(m: unknown): boolean | null {
  try {
    const fn =
      (m as unknown as { boot_from_cd_if_present?: unknown }).boot_from_cd_if_present ??
      (m as unknown as { bootFromCdIfPresent?: unknown }).bootFromCdIfPresent;
    if (typeof fn !== "function") return null;
    const raw = (fn as () => unknown).call(m);
    if (typeof raw === "boolean") return raw;
    if (typeof raw === "number" && Number.isFinite(raw)) return raw !== 0;
    return null;
  } catch {
    return null;
  }
}

function maybePostActiveBootDevice(m: unknown): void {
  const active = tryReadMachineActiveBootDevice(m);
  if (active) {
    postBootDeviceActive(active);
  }
}

function maybePostBootConfig(m: unknown): void {
  const bootDrive = tryReadMachineBootDrive(m);
  if (bootDrive === null) return;
  const cdBootDrive = tryReadMachineCdBootDrive(m);
  if (cdBootDrive === null) return;
  const bootFromCdIfPresent = tryReadMachineBootFromCdIfPresent(m);
  if (bootFromCdIfPresent === null) return;
  postBootConfig(bootDrive, cdBootDrive, bootFromCdIfPresent);
}

function postSnapshot(msg: MachineSnapshotRestoredMessage): void {
  ctx.postMessage(msg);
}

function postVmSnapshot(
  msg: VmSnapshotPausedMessage | VmSnapshotResumedMessage | VmSnapshotMachineSavedMessage | VmSnapshotMachineRestoredMessage,
): void {
  ctx.postMessage(msg);
}

function pushEvent(evt: Event): void {
  const ring = eventRing;
  if (!ring) return;
  try {
    ring.tryPush(encodeEvent(evt));
  } catch {
    // best-effort
  }
}

function pushEventBlocking(evt: Event, timeoutMs = 250): void {
  const ring = eventRing;
  if (!ring) return;
  const payload = encodeEvent(evt);
  if (ring.tryPush(payload)) return;
  try {
    ring.pushBlocking(payload, timeoutMs);
  } catch {
    // best-effort
  }
}

function serializeError(err: unknown): MachineSnapshotSerializedError {
  return serializeVmSnapshotError(err);
}

function trySetMachineBootDrive(m: unknown, drive: number): boolean {
  // Prefer the explicit `set_boot_drive(DL)` API when available.
  try {
    const setBootDrive =
      (m as unknown as { set_boot_drive?: unknown }).set_boot_drive ?? (m as unknown as { setBootDrive?: unknown }).setBootDrive;
    if (typeof setBootDrive === "function") {
      (setBootDrive as (drive: number) => void).call(m, drive);
      return true;
    }
  } catch {
    // ignore
  }

  // Back-compat: some builds expose `set_boot_device(MachineBootDevice::<...>)` instead.
  try {
    const setBootDevice =
      (m as unknown as { set_boot_device?: unknown }).set_boot_device ?? (m as unknown as { setBootDevice?: unknown }).setBootDevice;
    if (typeof setBootDevice !== "function") return false;

    const enumObj = wasmApi?.MachineBootDevice as unknown;
    const anyEnum = enumObj as { Hdd?: unknown; Cdrom?: unknown } | undefined;
    const device = drive === BIOS_DRIVE_CD0 ? anyEnum?.Cdrom : anyEnum?.Hdd;
    if (typeof device !== "number") return false;

    (setBootDevice as (device: number) => void).call(m, device);
    return true;
  } catch {
    return false;
  }
}

function trySetMachineBootFromCdIfPresent(m: unknown, enabled: boolean): boolean {
  try {
    const fn =
      (m as unknown as { set_boot_from_cd_if_present?: unknown }).set_boot_from_cd_if_present ??
      (m as unknown as { setBootFromCdIfPresent?: unknown }).setBootFromCdIfPresent;
    if (typeof fn !== "function") return false;
    (fn as (enabled: boolean) => void).call(m, enabled);
    return true;
  } catch {
    return false;
  }
}

function trySetMachineCdBootDrive(m: unknown, drive: number): boolean {
  try {
    const fn =
      (m as unknown as { set_cd_boot_drive?: unknown }).set_cd_boot_drive ?? (m as unknown as { setCdBootDrive?: unknown }).setCdBootDrive;
    if (typeof fn !== "function") return false;
    (fn as (drive: number) => void).call(m, drive);
    return true;
  } catch {
    return false;
  }
}

function isNodeWorkerThreads(): boolean {
  // Avoid referencing `process` directly so this file remains valid in browser builds without polyfills.
  const p = (globalThis as unknown as { process?: unknown }).process as { versions?: { node?: unknown } } | undefined;
  return typeof p?.versions?.node === "string";
}

async function maybeAwait(result: unknown): Promise<unknown> {
  if (!result || (typeof result !== "object" && typeof result !== "function")) {
    return result;
  }
  const then = (result as { then?: unknown }).then;
  if (typeof then === "function") {
    return await (result as Promise<unknown>);
  }
  return result;
}

function isNetworkingEnabled(config: AeroConfig | null): boolean {
  // Option C (L2 tunnel) is enabled when proxyUrl is configured.
  return !!(config?.proxyUrl && config.proxyUrl.trim().length > 0);
}

function isPowerOfTwo(n: number): boolean {
  return n > 0 && (n & (n - 1)) === 0;
}

function alignUpBigInt(value: bigint, alignment: bigint): bigint {
  if (alignment <= 0n) return value;
  return ((value + alignment - 1n) / alignment) * alignment;
}

async function tryReadAerosparseBlockSizeBytesFromOpfs(path: string): Promise<number | null> {
  if (!path) return null;
  // In CI/unit tests there is no `navigator` / OPFS environment. Treat this as best-effort.
  const storage = (globalThis as unknown as { navigator?: unknown }).navigator as { storage?: unknown } | undefined;
  const getDirectory = (storage?.storage as { getDirectory?: unknown } | undefined)?.getDirectory;
  if (typeof getDirectory !== "function") return null;

  // Overlay refs are expected to be relative OPFS paths. Refuse to interpret `..` to avoid path traversal.
  const parts = path.split("/").filter((p) => p && p !== ".");
  if (parts.length === 0 || parts.some((p) => p === "..")) return null;

  try {
    let dir = (await (getDirectory as () => Promise<FileSystemDirectoryHandle>)()) as FileSystemDirectoryHandle;
    for (const part of parts.slice(0, -1)) {
      dir = await dir.getDirectoryHandle(part, { create: false });
    }
    const file = await dir.getFileHandle(parts[parts.length - 1]!, { create: false }).then((h) => h.getFile());
    if (file.size < AEROSPARSE_HEADER_SIZE_BYTES) return null;
    const buf = await file.slice(0, AEROSPARSE_HEADER_SIZE_BYTES).arrayBuffer();
    if (buf.byteLength < AEROSPARSE_HEADER_SIZE_BYTES) return null;

    const bytes = new Uint8Array(buf);
    for (let i = 0; i < AEROSPARSE_MAGIC.length; i += 1) {
      if (bytes[i] !== AEROSPARSE_MAGIC[i]) return null;
    }
    const dv = new DataView(buf);
    const version = dv.getUint32(8, true);
    const headerSize = dv.getUint32(12, true);
    const blockSizeBytes = dv.getUint32(16, true);
    if (version !== 1 || headerSize !== AEROSPARSE_HEADER_SIZE_BYTES) return null;

    // Mirror the Rust-side aerosparse header validation (looser, but enough to avoid nonsense).
    const diskSizeBytes = dv.getBigUint64(24, true);
    if (diskSizeBytes === 0n || diskSizeBytes % 512n !== 0n) return null;
    if (blockSizeBytes === 0 || blockSizeBytes % 512 !== 0 || !isPowerOfTwo(blockSizeBytes) || blockSizeBytes > 64 * 1024 * 1024) {
      return null;
    }

    const tableOffset = dv.getBigUint64(32, true);
    if (tableOffset !== 64n) return null;
    const tableEntries = dv.getBigUint64(40, true);
    const blockSizeBig = BigInt(blockSizeBytes);
    const expectedTableEntries = (diskSizeBytes + blockSizeBig - 1n) / blockSizeBig;
    if (tableEntries !== expectedTableEntries) return null;
    const dataOffset = dv.getBigUint64(48, true);
    const expectedDataOffset = alignUpBigInt(64n + tableEntries * 8n, blockSizeBig);
    if (dataOffset !== expectedDataOffset) return null;
    const allocatedBlocks = dv.getBigUint64(56, true);
    if (allocatedBlocks > tableEntries) return null;
    // Ensure the file is large enough to contain the advertised data region.
    // (Mirrors the Rust-side `AeroSparseDisk::open` truncation checks.)
    const fileSize = file.size;
    if (typeof fileSize === "number" && Number.isFinite(fileSize) && fileSize >= 0 && Number.isSafeInteger(fileSize)) {
      const expectedMinLen = expectedDataOffset + allocatedBlocks * blockSizeBig;
      if (BigInt(fileSize) < expectedMinLen) return null;
    }

    return blockSizeBytes;
  } catch {
    return null;
  }
}

async function tryReadOpfsFileSizeBytes(path: string): Promise<number | null> {
  if (!path) return null;
  // In CI/unit tests there is no `navigator` / OPFS environment. Treat this as best-effort.
  const storage = (globalThis as unknown as { navigator?: unknown }).navigator as { storage?: unknown } | undefined;
  const getDirectory = (storage?.storage as { getDirectory?: unknown } | undefined)?.getDirectory;
  if (typeof getDirectory !== "function") return null;

  // Overlay refs are expected to be relative OPFS paths. Refuse to interpret `..` to avoid path traversal.
  const parts = path.split("/").filter((p) => p && p !== ".");
  if (parts.length === 0 || parts.some((p) => p === "..")) return null;

  try {
    let dir = (await (getDirectory as () => Promise<FileSystemDirectoryHandle>)()) as FileSystemDirectoryHandle;
    for (const part of parts.slice(0, -1)) {
      dir = await dir.getDirectoryHandle(part, { create: false });
    }
    const file = await dir.getFileHandle(parts[parts.length - 1]!, { create: false }).then((h) => h.getFile());
    const size = file.size;
    if (typeof size !== "number" || !Number.isFinite(size) || size < 0) return null;
    return size;
  } catch {
    return null;
  }
}

function detachMachineNetwork(): void {
  const m = machine;
  if (!m) return;
  try {
    const fn =
      (m as unknown as { detach_network?: unknown }).detach_network ??
      (m as unknown as { detachNetwork?: unknown }).detachNetwork ??
      (m as unknown as { detach_net_rings?: unknown }).detach_net_rings ??
      (m as unknown as { detachNetRings?: unknown }).detachNetRings;
    if (typeof fn === "function") {
      (fn as () => void).call(m);
    }
  } catch {
    // ignore
  }
  networkAttached = false;
}

function attachMachineNetwork(): void {
  const m = machine;
  if (!m) return;
  if (!networkWanted) return;
  const sab = ioIpcSab;
  if (!sab) return;

  try {
    const attachFromSab =
      (m as unknown as { attach_l2_tunnel_from_io_ipc_sab?: unknown }).attach_l2_tunnel_from_io_ipc_sab ??
      (m as unknown as { attachL2TunnelFromIoIpcSab?: unknown }).attachL2TunnelFromIoIpcSab;
    if (typeof attachFromSab === "function") {
      (attachFromSab as (sab: SharedArrayBuffer) => void).call(m, sab);
      networkAttached = true;
      return;
    }

    const attachRings =
      (m as unknown as { attach_l2_tunnel_rings?: unknown }).attach_l2_tunnel_rings ??
      (m as unknown as { attachL2TunnelRings?: unknown }).attachL2TunnelRings ??
      (m as unknown as { attach_net_rings?: unknown }).attach_net_rings ??
      (m as unknown as { attachNetRings?: unknown }).attachNetRings;
    const openRing = wasmApi?.open_ring_by_kind;
    if (typeof attachRings === "function" && typeof openRing === "function") {
      const tx = openRing(sab, IO_IPC_NET_TX_QUEUE_KIND, 0);
      const rx = openRing(sab, IO_IPC_NET_RX_QUEUE_KIND, 0);
      (attachRings as (tx: unknown, rx: unknown) => void).call(m, tx, rx);
      networkAttached = true;
    }
  } catch {
    // ignore
  }
}

async function createWin7MachineWithSharedGuestMemory(api: WasmApi, layout: GuestRamLayout): Promise<InstanceType<WasmApi["Machine"]>> {
  const guestBase = layout.guest_base >>> 0;
  const guestSize = layout.guest_size >>> 0;

  const Machine = api.Machine;
  if (!Machine) {
    throw new Error("Machine wasm export is unavailable; cannot start machine_cpu.worker.");
  }

  // Optional config override: force-disable AeroGPU (enable VGA) for debugging/compatibility.
  // This requires a newer WASM build that exposes `Machine.new_shared_with_config`.
  const requestedAerogpu = currentConfig?.machineEnableAerogpu;
  if (typeof requestedAerogpu === "boolean") {
    const newSharedWithConfig = (Machine as unknown as { new_shared_with_config?: unknown }).new_shared_with_config;
    if (typeof newSharedWithConfig === "function") {
      try {
        const result = (newSharedWithConfig as (guestBase: number, guestSize: number, enableAerogpu: boolean) => unknown).call(
          Machine,
          guestBase,
          guestSize,
          requestedAerogpu,
        );
        return (await maybeAwait(result)) as InstanceType<WasmApi["Machine"]>;
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[machine_cpu.worker] Failed to construct Machine via Machine.new_shared_with_config:", err);
      }
    }
  }

  type Candidate = { name: string; fn: unknown; thisArg: unknown };
  // Prefer the canonical shared-guest-memory constructor (`MachineConfig::browser_defaults`):
  // - AeroGPU enabled (VGA disabled)
  // - E1000 + UHCI enabled (browser runtime expectations)
  //
  // When the config explicitly disables AeroGPU (`machineEnableAerogpu=false`) but the build does
  // not expose `Machine.new_shared_with_config`, fall back to the conservative Win7 storage preset
  // (VGA enabled, networking/USB disabled) rather than ignoring the user's intent.
  const preferWin7StorageShared = requestedAerogpu === false;
  const candidates: Candidate[] = [
    ...(preferWin7StorageShared
      ? [
          { name: "Machine.new_win7_storage_shared", fn: Machine.new_win7_storage_shared, thisArg: Machine },
          { name: "Machine.new_shared", fn: Machine.new_shared, thisArg: Machine },
        ]
      : [
          { name: "Machine.new_shared", fn: Machine.new_shared, thisArg: Machine },
          { name: "Machine.new_win7_storage_shared", fn: Machine.new_win7_storage_shared, thisArg: Machine },
        ]),

    // Back-compat shims for intermediate builds.
    { name: "Machine.new_win7_storage_shared_guest_memory", fn: Machine.new_win7_storage_shared_guest_memory, thisArg: Machine },
    { name: "Machine.new_shared_guest_memory_win7_storage", fn: Machine.new_shared_guest_memory_win7_storage, thisArg: Machine },
    { name: "Machine.new_shared_guest_memory", fn: Machine.new_shared_guest_memory, thisArg: Machine },
    { name: "Machine.from_shared_guest_memory_win7_storage", fn: Machine.from_shared_guest_memory_win7_storage, thisArg: Machine },
    { name: "Machine.from_shared_guest_memory", fn: Machine.from_shared_guest_memory, thisArg: Machine },

    // Free-function factories (older wasm-bindgen exports).
    { name: "create_win7_machine_shared_guest_memory", fn: api.create_win7_machine_shared_guest_memory, thisArg: api },
    { name: "create_machine_win7_shared_guest_memory", fn: api.create_machine_win7_shared_guest_memory, thisArg: api },
    { name: "create_machine_shared_guest_memory_win7", fn: api.create_machine_shared_guest_memory_win7, thisArg: api },
  ];

  for (const c of candidates) {
    if (typeof c.fn !== "function") continue;
    try {
      const arity = (c.fn as (...args: unknown[]) => unknown).length;
      let result: unknown;
      if (arity === 0) {
        result = (c.fn as () => unknown).call(c.thisArg);
      } else if (arity === 1) {
        result = (c.fn as (guestBase: number) => unknown).call(c.thisArg, guestBase);
      } else {
        result = (c.fn as (guestBase: number, guestSize: number) => unknown).call(c.thisArg, guestBase, guestSize);
      }
      return (await maybeAwait(result)) as InstanceType<WasmApi["Machine"]>;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`[machine_cpu.worker] Failed to construct Machine via ${c.name}:`, err);
    }
  }

  // No shared-memory constructor exists in this WASM build. Fall back to heap-allocating guest RAM
  // (`new Machine(ramBytes)`), but warn loudly: large RAM sizes will likely OOM without `new_shared`.
  const hasSharedCtor = candidates.some((c) => typeof c.fn === "function");
  if (!hasSharedCtor) {
    try {
      // eslint-disable-next-line no-console
      console.warn(
        `[machine_cpu.worker] api.Machine.new_shared(guest_base, guest_size) is unavailable; falling back to new api.Machine(ramBytes=${guestSize}). ` +
          "Large RAM sizes will likely OOM without new_shared. Rebuild the wasm-pack output (threaded build) to enable shared guest RAM.",
      );
    } catch {
      // ignore
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const ctor = Machine as unknown as new (ramSizeBytes: number) => InstanceType<WasmApi["Machine"]>;
    return new ctor(guestSize >>> 0);
  }

  throw new Error(
    "Shared-guest-memory Machine constructor is unavailable in this WASM build. " +
      "Expected Machine.new_shared(guestBase, guestSize) (or an equivalent factory).",
  );
}

function postInputBatchRecycle(buffer: ArrayBuffer): void {
  // Input batch recycling is a performance optimization. Avoid recycling extremely large buffers so
  // a malicious or buggy sender cannot force the main thread's recycle pool to retain unbounded
  // memory. The cap matches `MAX_QUEUED_INPUT_BATCH_BYTES` (used to bound snapshot-paused input
  // buffering) so tests that intentionally allocate up to that limit remain supported.
  if (!shouldRecycleInputBatchBuffer(buffer, MAX_QUEUED_INPUT_BATCH_BYTES)) return;
  const msg: InputBatchRecycleMessage = { type: "in:input-batch-recycle", buffer };
  try {
    ctx.postMessage(msg, [buffer]);
  } catch {
    try {
      ctx.postMessage(msg);
    } catch {
      // ignore
    }
  }
}

function queueInputBatch(buffer: ArrayBuffer, recycle: boolean): void {
  if (queuedInputBatchBytes + buffer.byteLength <= MAX_QUEUED_INPUT_BATCH_BYTES) {
    queuedInputBatches.push({ buffer, recycle });
    queuedInputBatchBytes += buffer.byteLength;
    return;
  }

  // Drop excess input to keep memory bounded; best-effort recycle the transferred buffer.
  const st = status;
  if (st) Atomics.add(st, StatusIndex.IoInputBatchDropCounter, 1);
  if (recycle) {
    postInputBatchRecycle(buffer);
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  const st = status;
  const m = machine;
  if (!m) return;

  const t0 = performance.now();
  const nowUs = Math.round(t0 * 1000) >>> 0;
  const decoded = validateInputBatchBuffer(buffer);
  if (!decoded.ok) {
    if (st) Atomics.add(st, StatusIndex.IoInputBatchDropCounter, 1);
    return;
  }

  const { words, count, claimedCount } = decoded;
  const batchSendTimestampUs = words[1] >>> 0;
  const batchSendLatencyUs = u32Delta(nowUs, batchSendTimestampUs);

  if (st) {
    // Maintain the same shared status telemetry indices as the legacy I/O worker so existing
    // UIs/tests that track `ioBatches`/`ioEvents` remain meaningful when input is injected by the
    // machine CPU worker.
    if (ioInputLatencyMaxWindowStartMs === 0 || t0 - ioInputLatencyMaxWindowStartMs > INPUT_LATENCY_MAX_WINDOW_MS) {
      ioInputLatencyMaxWindowStartMs = t0;
      ioInputBatchSendLatencyMaxUs = 0;
      ioInputEventLatencyMaxUs = 0;
    }
    ioInputBatchSendLatencyEwmaUs =
      ioInputBatchSendLatencyEwmaUs === 0
        ? batchSendLatencyUs
        : Math.round(ioInputBatchSendLatencyEwmaUs + (batchSendLatencyUs - ioInputBatchSendLatencyEwmaUs) * INPUT_LATENCY_EWMA_ALPHA) >>> 0;
    if (batchSendLatencyUs > ioInputBatchSendLatencyMaxUs) {
      ioInputBatchSendLatencyMaxUs = batchSendLatencyUs;
    }

    Atomics.add(st, StatusIndex.IoInputBatchCounter, 1);
    Atomics.add(st, StatusIndex.IoInputEventCounter, count);
    if (count !== claimedCount) {
      // Count clamping as a "drop" for telemetry parity with io.worker.ts.
      Atomics.add(st, StatusIndex.IoInputBatchDropCounter, 1);
    }
  }

  if (count === 0) {
    return;
  }

  // Snapshot the current virtio driver readiness state so backend selection +
  // input routing for this batch is consistent.
  const anyMachine = m as unknown as {
    virtio_input_keyboard_driver_ok?: unknown;
    virtio_input_mouse_driver_ok?: unknown;
  };
  const virtioKeyboardOk = safeCallBool(m, anyMachine.virtio_input_keyboard_driver_ok);
  const virtioMouseOk = safeCallBool(m, anyMachine.virtio_input_mouse_driver_ok);

  // Ensure backend selection is evaluated before processing this batch so we
  // can correctly decide whether to consume PS/2 scancodes or USB HID usages.
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });

  const base = INPUT_BATCH_HEADER_WORDS;
  let eventLatencySumUs = 0;
  let eventLatencyMaxUsBatch = 0;
  // When snapshot restore rewinds guest time, the host-side pressed-key tracking used to gate
  // backend switching is not part of the snapshot. If we only observe a key-up after restore, we
  // may need to force-deliver the PS/2 break bytes (from `KeyScancode` events) even when the
  // keyboard backend is currently virtio/USB.
  //
  // State machine:
  // - 0: inactive
  // - 1: armed (waiting for the next KeyScancode event)
  // - 2: active (inject all contiguous KeyScancode events, then auto-clear)
  let forcePs2ScancodeBurst: 0 | 1 | 2 = 0;
  for (let i = 0; i < count; i += 1) {
    const off = base + i * INPUT_BATCH_WORDS_PER_EVENT;
    const type = words[off] >>> 0;
    // Clear/expire the forced PS/2 scancode injection state once we leave the scancode burst.
    if (forcePs2ScancodeBurst === 2 && type !== InputEventType.KeyScancode) {
      forcePs2ScancodeBurst = 0;
    } else if (forcePs2ScancodeBurst === 1 && type !== InputEventType.KeyScancode) {
      // Scancodes should follow immediately after the key event. If we see something else first,
      // drop the arm to avoid misapplying it to unrelated scancodes later in the batch.
      forcePs2ScancodeBurst = 0;
    }
    const eventTimestampUs = words[off + 1] >>> 0;
    const eventLatencyUs = u32Delta(nowUs, eventTimestampUs);
    eventLatencySumUs += eventLatencyUs;
    if (eventLatencyUs > eventLatencyMaxUsBatch) {
      eventLatencyMaxUsBatch = eventLatencyUs;
    }
    if (type === InputEventType.KeyHidUsage) {
      const packed = words[off + 2] >>> 0;
      const usage = packed & 0xff;
      const pressed = ((packed >>> 8) & 1) !== 0;
      const prevPressed = pressedKeyboardHidUsages[usage] !== 0;
      updatePressedKeyboardHidUsage(usage, pressed);

      if (!pressed && !prevPressed) {
        // Unknown key-up: best-effort clear all keyboard backends. This can happen after a VM
        // snapshot restore, where host-side pressed-key tracking is not serialized and we may only
        // observe the release event.
        const injectUsb = (m as unknown as { inject_usb_hid_keyboard_usage?: unknown }).inject_usb_hid_keyboard_usage;
        if (typeof injectUsb === "function") {
          try {
            (injectUsb as (usage: number, pressed: boolean) => void).call(m, usage >>> 0, false);
          } catch {
            // ignore
          }
        }
        if (virtioKeyboardOk) {
          const keyCode = hidUsageToLinuxKeyCode(usage);
          if (keyCode !== null) {
            const injectKey = (m as unknown as { inject_virtio_key?: unknown }).inject_virtio_key;
            if (typeof injectKey === "function") {
              try {
                (injectKey as (linuxKey: number, pressed: boolean) => void).call(m, keyCode >>> 0, false);
              } catch {
                // ignore
              }
            }
          }
        }
        // Also best-effort deliver the matching PS/2 break scancodes (from subsequent `KeyScancode`
        // events) so guests that observed the key-down via i8042 don't remain stuck.
        if (keyboardInputBackend !== "ps2") {
          forcePs2ScancodeBurst = 1;
        }
        continue;
      }
      if (keyboardInputBackend === "virtio") {
        if (virtioKeyboardOk) {
          const keyCode = hidUsageToLinuxKeyCode(usage);
          if (keyCode !== null) {
            const injectKey = (m as unknown as { inject_virtio_key?: unknown }).inject_virtio_key;
            if (typeof injectKey === "function") {
              try {
                (injectKey as (linuxKey: number, pressed: boolean) => void).call(m, keyCode >>> 0, pressed);
              } catch {
                // ignore
              }
            }
          }
        }
      } else if (keyboardInputBackend === "usb") {
        const inject = (m as unknown as { inject_usb_hid_keyboard_usage?: unknown }).inject_usb_hid_keyboard_usage;
        if (typeof inject === "function") {
          try {
            (inject as (usage: number, pressed: boolean) => void).call(m, usage >>> 0, pressed);
          } catch {
            // ignore
          }
        }
      }
    } else if (type === InputEventType.KeyScancode) {
      // Only inject PS/2 scancodes when the PS/2 backend is active. Other backends
      // (virtio-input / synthetic USB HID) rely on `KeyHidUsage` events and would
      // otherwise cause duplicated input in the guest.
      if (keyboardInputBackend !== "ps2" && forcePs2ScancodeBurst === 0) continue;
      const packed = words[off + 2] >>> 0;
      const len = Math.min(words[off + 3] >>> 0, 4);
      if (len === 0) continue;
      try {
        if (typeof m.inject_key_scancode_bytes === "function") {
          m.inject_key_scancode_bytes(packed, len);
        } else if (typeof m.inject_keyboard_bytes === "function") {
          const bytes = packedScancodeScratch[len]!;
          for (let j = 0; j < len; j++) bytes[j] = (packed >>> (j * 8)) & 0xff;
          m.inject_keyboard_bytes(bytes);
        }
      } catch {
        // ignore
      }
      if (forcePs2ScancodeBurst === 1) {
        forcePs2ScancodeBurst = 2;
      }
    } else if (type === InputEventType.MouseMove) {
      const dx = words[off + 2] | 0;
      const dyPs2 = words[off + 3] | 0;
      // Input batches use PS/2 convention: positive Y is up. virtio-input and USB HID use +Y down.
      const dyDown = negateI32Saturating(dyPs2);
      if (mouseInputBackend === "virtio") {
        if (virtioMouseOk) {
          const injectRel =
            (m as unknown as { inject_virtio_mouse_rel?: unknown }).inject_virtio_mouse_rel ??
            (m as unknown as { inject_virtio_rel?: unknown }).inject_virtio_rel;
          if (typeof injectRel === "function") {
            try {
              (injectRel as (dx: number, dy: number) => void).call(m, dx | 0, dyDown | 0);
            } catch {
              // ignore
            }
          }
        }
      } else if (mouseInputBackend === "ps2") {
        try {
          if (typeof m.inject_ps2_mouse_motion === "function") {
            m.inject_ps2_mouse_motion(dx, dyPs2, 0);
          } else if (typeof m.inject_mouse_motion === "function") {
            m.inject_mouse_motion(dx, dyDown, 0);
          }
        } catch {
          // ignore
        }
      } else {
        // Synthetic USB HID convention matches browser coordinates: positive = down.
        const inject = (m as unknown as { inject_usb_hid_mouse_move?: unknown }).inject_usb_hid_mouse_move;
        if (typeof inject === "function") {
          try {
            (inject as (dx: number, dy: number) => void).call(m, dx | 0, dyDown | 0);
          } catch {
            // ignore
          }
        }
      }
    } else if (type === InputEventType.MouseWheel) {
      const dz = words[off + 2] | 0;
      const dx = words[off + 3] | 0;
      if (mouseInputBackend === "virtio") {
        if (virtioMouseOk) {
          const wheel2 = (m as unknown as { inject_virtio_wheel2?: unknown }).inject_virtio_wheel2;
          if (typeof wheel2 === "function") {
            if (dz !== 0 || dx !== 0) {
              try {
                (wheel2 as (wheel: number, hwheel: number) => void).call(m, dz | 0, dx | 0);
              } catch {
                // ignore
              }
            }
          } else {
            const wheel = (m as unknown as { inject_virtio_wheel?: unknown }).inject_virtio_wheel;
            const hwheel = (m as unknown as { inject_virtio_hwheel?: unknown }).inject_virtio_hwheel;
            if (dz !== 0 && typeof wheel === "function") {
              try {
                (wheel as (delta: number) => void).call(m, dz | 0);
              } catch {
                // ignore
              }
            }
            if (dx !== 0 && typeof hwheel === "function") {
              try {
                (hwheel as (delta: number) => void).call(m, dx | 0);
              } catch {
                // ignore
              }
            }
          }
        }
      } else if (mouseInputBackend === "ps2") {
        if (dz === 0) continue;
        try {
          if (typeof m.inject_ps2_mouse_motion === "function") {
            m.inject_ps2_mouse_motion(0, 0, dz);
          } else if (typeof m.inject_mouse_motion === "function") {
            m.inject_mouse_motion(0, 0, dz);
          }
        } catch {
          // ignore
        }
      } else {
        // Prefer a combined wheel2 API when available so diagonal scroll events can be represented
        // as a single HID report (matching `InputEventType.MouseWheel`, which carries both axes).
        const wheel2 = (m as unknown as { inject_usb_hid_mouse_wheel2?: unknown }).inject_usb_hid_mouse_wheel2;
        if (typeof wheel2 === "function") {
          if (dz !== 0 || dx !== 0) {
            try {
              (wheel2 as (wheel: number, hwheel: number) => void).call(m, dz | 0, dx | 0);
            } catch {
              // ignore
            }
          }
        } else {
          const wheel = (m as unknown as { inject_usb_hid_mouse_wheel?: unknown }).inject_usb_hid_mouse_wheel;
          const hwheel = (m as unknown as { inject_usb_hid_mouse_hwheel?: unknown }).inject_usb_hid_mouse_hwheel;
          if (dz !== 0 && typeof wheel === "function") {
            try {
              (wheel as (delta: number) => void).call(m, dz | 0);
            } catch {
              // ignore
            }
          }
          if (dx !== 0 && typeof hwheel === "function") {
            try {
              (hwheel as (delta: number) => void).call(m, dx | 0);
            } catch {
              // ignore
            }
          }
        }
      }
    } else if (type === InputEventType.MouseButtons) {
      const buttons = words[off + 2] & 0xff;
      const prevButtons = mouseButtonsMask;
      mouseButtonsMask = buttons;

      if (prevButtons === 0 && buttons === 0) {
        // Unknown "all released" snapshot: best-effort clear all mouse backends. Like keyboards,
        // host-side held-state tracking is not part of the VM snapshot format, so after restore we
        // may only observe the release event.
        if (virtioMouseOk) {
          injectVirtioMouseButtons(m, 0);
        }
        try {
          if (typeof m.inject_mouse_buttons_mask === "function") {
            m.inject_mouse_buttons_mask(0);
          } else if (typeof m.inject_ps2_mouse_buttons === "function") {
            m.inject_ps2_mouse_buttons(0);
          }
        } catch {
          // ignore
        }
        const injectUsb = (m as unknown as { inject_usb_hid_mouse_buttons?: unknown }).inject_usb_hid_mouse_buttons;
        if (typeof injectUsb === "function") {
          try {
            (injectUsb as (mask: number) => void).call(m, 0);
          } catch {
            // ignore
          }
        }
        continue;
      }
      if (mouseInputBackend === "virtio") {
        if (virtioMouseOk) {
          injectVirtioMouseButtons(m, buttons);
        }
      } else {
        const mask = buttons & 0x1f;
        if (mouseInputBackend === "ps2") {
          try {
            if (typeof m.inject_mouse_buttons_mask === "function") {
              m.inject_mouse_buttons_mask(mask);
            } else if (typeof m.inject_ps2_mouse_buttons === "function") {
              m.inject_ps2_mouse_buttons(mask);
            }
          } catch {
            // ignore
          }
        } else {
          const inject = (m as unknown as { inject_usb_hid_mouse_buttons?: unknown }).inject_usb_hid_mouse_buttons;
          if (typeof inject === "function") {
            try {
              (inject as (mask: number) => void).call(m, mask >>> 0);
            } catch {
              // ignore
            }
          }
        }
      }
    } else if (type === InputEventType.HidUsage16) {
      const a = words[off + 2] >>> 0;
      const usagePage = a & 0xffff;
      const pressed = ((a >>> 16) & 1) !== 0;
      const usageId = words[off + 3] & 0xffff;
      // Consumer Control (0x0C) can be delivered either via:
      // - virtio-input keyboard (media keys subset, exposed by the Win7 virtio-input driver as a Consumer Control collection), or
      // - a dedicated synthetic USB HID consumer-control device (supports the full usage ID range).
      if (usagePage !== 0x0c) continue;

      const idx = usageId & 0xffff;
      const prevPressed = idx < pressedConsumerUsages.length && pressedConsumerUsages[idx] !== 0;
      updatePressedConsumerUsage(usageId, pressed);

      if (!pressed && !prevPressed) {
        // Unknown consumer-key release after snapshot restore: best-effort clear both virtio and
        // synthetic USB consumer-control backends.
        if (virtioKeyboardOk) {
          const keyCode = hidConsumerUsageToLinuxKeyCode(usageId);
          if (keyCode !== null) {
            const injectKey = (m as unknown as { inject_virtio_key?: unknown }).inject_virtio_key;
            if (typeof injectKey === "function") {
              try {
                (injectKey as (linuxKey: number, pressed: boolean) => void).call(m, keyCode >>> 0, false);
              } catch {
                // ignore
              }
            }
          }
        }
        try {
          const inject = (m as unknown as { inject_usb_hid_consumer_usage?: unknown }).inject_usb_hid_consumer_usage;
          if (typeof inject === "function") {
            (inject as (usage: number, pressed: boolean) => void).call(m, usageId >>> 0, false);
          }
        } catch {
          // ignore
        }
        continue;
      }

      // Prefer virtio-input when the virtio keyboard backend is active and the usage is representable as a Linux key code.
      if (keyboardInputBackend === "virtio" && virtioKeyboardOk) {
        const keyCode = hidConsumerUsageToLinuxKeyCode(usageId);
        if (keyCode !== null) {
          const injectKey = (m as unknown as { inject_virtio_key?: unknown }).inject_virtio_key;
          if (typeof injectKey === "function") {
            try {
              (injectKey as (linuxKey: number, pressed: boolean) => void).call(m, keyCode >>> 0, pressed);
            } catch {
              // ignore
            }
          }
          continue;
        }
      }

      // Otherwise fall back to the synthetic USB consumer-control device. This handles browser navigation keys (AC Back/Forward/etc.)
      // which are not currently modeled by the virtio-input keyboard.
      try {
        const inject = (m as unknown as { inject_usb_hid_consumer_usage?: unknown }).inject_usb_hid_consumer_usage;
        if (typeof inject === "function") {
          (inject as (usage: number, pressed: boolean) => void).call(m, usageId >>> 0, pressed);
        }
      } catch {
        // ignore
      }
    } else if (type === InputEventType.GamepadReport) {
      // USB HID gamepad report: a/b are packed 8 bytes (little-endian).
      const packedLo = words[off + 2] >>> 0;
      const packedHi = words[off + 3] >>> 0;
      try {
        const inject = (m as unknown as { inject_usb_hid_gamepad_report?: unknown }).inject_usb_hid_gamepad_report;
        if (typeof inject === "function") {
          (inject as (a: number, b: number) => void).call(m, packedLo, packedHi);
        }
      } catch {
        // ignore
      }
    }
  }

  // Re-evaluate backend selection after processing this batch; key-up events can make it safe to
  // transition away from PS/2 scancode injection.
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });
  publishInputBackendStatus({ virtioKeyboardOk, virtioMouseOk });

  if (st) {
    const eventLatencyAvgUs = Math.round(eventLatencySumUs / count) >>> 0;
    ioInputEventLatencyEwmaUs =
      ioInputEventLatencyEwmaUs === 0
        ? eventLatencyAvgUs
        : Math.round(ioInputEventLatencyEwmaUs + (eventLatencyAvgUs - ioInputEventLatencyEwmaUs) * INPUT_LATENCY_EWMA_ALPHA) >>> 0;
    if (eventLatencyMaxUsBatch > ioInputEventLatencyMaxUs) {
      ioInputEventLatencyMaxUs = eventLatencyMaxUsBatch;
    }

    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyUs, batchSendLatencyUs | 0);
    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyEwmaUs, ioInputBatchSendLatencyEwmaUs | 0);
    Atomics.store(st, StatusIndex.IoInputBatchSendLatencyMaxUs, ioInputBatchSendLatencyMaxUs | 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyAvgUs, eventLatencyAvgUs | 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyEwmaUs, ioInputEventLatencyEwmaUs | 0);
    Atomics.store(st, StatusIndex.IoInputEventLatencyMaxUs, ioInputEventLatencyMaxUs | 0);
  }
}

async function applyBootDisks(msg: SetBootDisksMessage): Promise<void> {
  const m = machine;
  if (!m) return;

  let changed = false;

  // The Aero BIOS can expose both HDD0 (`DL=0x80`) and CD0 (`DL=0xE0`) via INT 13h when both are
  // present. The boot-device choice is still driven by the boot-drive number (`DL`) used when
  // transferring control to the boot sector, but newer BIOS builds also support an optional
  // "CD-first when present" policy (attempt CD boot first, then fall back to the configured HDD
  // boot drive).
  //
  // Policy (worker-side):
  // - When an ISO is attached, prefer a CD boot for the next host-triggered reset (install/recovery).
  // - When the guest requests a reset, fall back to HDD0 after the first CD boot so installs do not
  //   loop back into the ISO.
  //
  // Track boot-device preference separately from disk attachment so the ISO can remain mounted
  // (for file access) while still booting from HDD after installation.
  const hasHdd =
    !!msg.hdd || (typeof msg.mounts?.hddId === "string" && msg.mounts.hddId.trim().length > 0);
  const desiredBootDrive =
    pendingBootDevice === "cdrom" && msg.cd
      ? BIOS_DRIVE_CD0
      : pendingBootDevice === "hdd" && hasHdd
        ? BIOS_DRIVE_HDD0
        : msg.cd
          ? BIOS_DRIVE_CD0
          : BIOS_DRIVE_HDD0;

  // When supported by the wasm build, prefer the BIOS "CD-first when present" policy so we can boot
  // from the ISO once while leaving the configured boot drive as HDD0 (useful for later ISO eject /
  // post-install reboots without host-side `DL` toggling).
  const canCdFirstPolicy =
    typeof (m as unknown as { set_boot_from_cd_if_present?: unknown }).set_boot_from_cd_if_present === "function" ||
    typeof (m as unknown as { setBootFromCdIfPresent?: unknown }).setBootFromCdIfPresent === "function";
  const useCdFirstPolicy = desiredBootDrive === BIOS_DRIVE_CD0 && !!msg.cd && hasHdd && canCdFirstPolicy;
  const configuredBootDrive = useCdFirstPolicy ? BIOS_DRIVE_HDD0 : desiredBootDrive;

  if (msg.hdd) {
    const plan = planMachineBootDiskAttachment(msg.hdd, "hdd");
    if (plan.format === "aerospar") {
      // Prefer a copy-on-write overlay when available so machine runtime matches the legacy
      // disk worker behaviour: imported base images remain unchanged and guest writes persist in a
      // derived `*.overlay.aerospar` file.
      const cowOpenAndSetRef =
        (m as unknown as { set_disk_cow_opfs_open_and_set_overlay_ref?: unknown }).set_disk_cow_opfs_open_and_set_overlay_ref ??
        (m as unknown as { setDiskCowOpfsOpenAndSetOverlayRef?: unknown }).setDiskCowOpfsOpenAndSetOverlayRef;
      const cowOpen =
        (m as unknown as { set_disk_cow_opfs_open?: unknown }).set_disk_cow_opfs_open ??
        (m as unknown as { setDiskCowOpfsOpen?: unknown }).setDiskCowOpfsOpen;
      const cowCreateAndSetRef =
        (m as unknown as { set_disk_cow_opfs_create_and_set_overlay_ref?: unknown }).set_disk_cow_opfs_create_and_set_overlay_ref ??
        (m as unknown as { setDiskCowOpfsCreateAndSetOverlayRef?: unknown }).setDiskCowOpfsCreateAndSetOverlayRef;
      const cowCreate =
        (m as unknown as { set_disk_cow_opfs_create?: unknown }).set_disk_cow_opfs_create ??
        (m as unknown as { setDiskCowOpfsCreate?: unknown }).setDiskCowOpfsCreate;

      const canCowOpen = typeof cowOpenAndSetRef === "function" || typeof cowOpen === "function";
      const canCowCreate = typeof cowCreateAndSetRef === "function" || typeof cowCreate === "function";

      const cowPaths = diskMetaToOpfsCowPaths(msg.hdd);
      if ((canCowOpen || canCowCreate) && cowPaths) {
        const overlaySize = await tryReadOpfsFileSizeBytes(cowPaths.overlayPath);
        const overlayHasHeader = typeof overlaySize === "number" && overlaySize >= AEROSPARSE_HEADER_SIZE_BYTES;

        if (overlayHasHeader && canCowOpen) {
          const fn = (typeof cowOpenAndSetRef === "function" ? cowOpenAndSetRef : cowOpen) as unknown;
          try {
            await maybeAwait((fn as (base: string, overlay: string) => unknown).call(m, cowPaths.basePath, cowPaths.overlayPath));
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            throw new Error(
              `setBootDisks: failed to open COW overlay for aerospar HDD (disk_id=0) ` +
                `base=${cowPaths.basePath} overlay=${cowPaths.overlayPath}: ${message}`,
            );
          }

          // Best-effort overlay ref: `set_disk_cow_opfs_open` may not record snapshot refs.
          if (typeof cowOpenAndSetRef !== "function") {
            try {
              const setRef =
                (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
                (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
              if (typeof setRef === "function") {
                (setRef as (base: string, overlay: string) => void).call(m, cowPaths.basePath, cowPaths.overlayPath);
              }
            } catch {
              // ignore
            }
          }

          changed = true;
        } else if (!overlayHasHeader && canCowCreate) {
          const fn = (typeof cowCreateAndSetRef === "function" ? cowCreateAndSetRef : cowCreate) as unknown;
          const blockSizeBytes =
            cowPaths.overlayBlockSizeBytes ??
            (await tryReadAerosparseBlockSizeBytesFromOpfs(cowPaths.overlayPath)) ??
            DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES;
          try {
            await maybeAwait(
              (fn as (base: string, overlay: string, blockSizeBytes: number) => unknown).call(
                m,
                cowPaths.basePath,
                cowPaths.overlayPath,
                blockSizeBytes,
              ),
            );
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            throw new Error(
              `setBootDisks: failed to create COW overlay for aerospar HDD (disk_id=0) ` +
                `base=${cowPaths.basePath} overlay=${cowPaths.overlayPath}: ${message}`,
            );
          }

          if (typeof cowCreateAndSetRef !== "function") {
            try {
              const setRef =
                (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
                (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
              if (typeof setRef === "function") {
                (setRef as (base: string, overlay: string) => void).call(m, cowPaths.basePath, cowPaths.overlayPath);
              }
            } catch {
              // ignore
            }
          }
          changed = true;
        }
      }

      if (!changed) {
        // Fall back to attaching the aerosparse disk directly when COW overlay helpers are unavailable.
        const openAndSetRef =
          (m as unknown as { set_disk_aerospar_opfs_open_and_set_overlay_ref?: unknown }).set_disk_aerospar_opfs_open_and_set_overlay_ref ??
          (m as unknown as { setDiskAerosparOpfsOpenAndSetOverlayRef?: unknown }).setDiskAerosparOpfsOpenAndSetOverlayRef;
        const open =
          (m as unknown as { set_disk_aerospar_opfs_open?: unknown }).set_disk_aerospar_opfs_open ??
          (m as unknown as { setDiskAerosparOpfsOpen?: unknown }).setDiskAerosparOpfsOpen;
        if (typeof openAndSetRef === "function") {
          try {
            await maybeAwait((openAndSetRef as (path: string) => unknown).call(m, plan.opfsPath));
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            throw new Error(`setBootDisks: failed to attach aerospar HDD (disk_id=0) path=${plan.opfsPath}: ${message}`);
          }
        } else if (typeof open === "function") {
          try {
            await maybeAwait((open as (path: string) => unknown).call(m, plan.opfsPath));
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            throw new Error(`setBootDisks: failed to attach aerospar HDD (disk_id=0) path=${plan.opfsPath}: ${message}`);
          }
          // Best-effort overlay ref: ensure snapshots record a stable base_image for disk_id=0.
          try {
            const setRef =
              (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
              (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
            if (typeof setRef === "function") {
              (setRef as (base: string, overlay: string) => void).call(m, plan.opfsPath, "");
            }
          } catch {
            // ignore
          }
        } else {
          // Newer WASM builds can open aerosparse disks via the generic OPFS existing open path when
          // provided an explicit base format.
          const existingAndSetRef =
            (m as unknown as { set_disk_opfs_existing_and_set_overlay_ref?: unknown }).set_disk_opfs_existing_and_set_overlay_ref ??
            (m as unknown as { setDiskOpfsExistingAndSetOverlayRef?: unknown }).setDiskOpfsExistingAndSetOverlayRef;
          const existing =
            (m as unknown as { set_disk_opfs_existing?: unknown }).set_disk_opfs_existing ??
            (m as unknown as { setDiskOpfsExisting?: unknown }).setDiskOpfsExisting;
          const openViaFormat =
            typeof existingAndSetRef === "function" && existingAndSetRef.length >= 2
              ? existingAndSetRef
              : typeof existing === "function" && existing.length >= 2
                ? existing
                : null;
          if (openViaFormat == null) {
            throw new Error(
              `Machine.set_disk_aerospar_opfs_open* exports are unavailable in this WASM build (disk path=${plan.opfsPath}), and generic aerospar open via Machine.set_disk_opfs_existing*(path, \"aerospar\") is unsupported.`,
            );
          }

          const expectedSizeBytes =
            typeof msg.hdd.sizeBytes === "number" &&
            Number.isSafeInteger(msg.hdd.sizeBytes) &&
            msg.hdd.sizeBytes > 0
              ? BigInt(msg.hdd.sizeBytes)
              : undefined;
          try {
            await maybeAwait(
              (openViaFormat as (path: string, baseFormat: string, expectedSizeBytes?: bigint) => unknown).call(
                m,
                plan.opfsPath,
                "aerospar",
                expectedSizeBytes,
              ),
            );
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            throw new Error(`setBootDisks: failed to attach aerospar HDD (disk_id=0) path=${plan.opfsPath}: ${message}`);
          }

          if (openViaFormat !== existingAndSetRef) {
            // Best-effort overlay ref: ensure snapshots record a stable base_image for disk_id=0.
            try {
              const setRef =
                (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
                (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
              if (typeof setRef === "function") {
                (setRef as (base: string, overlay: string) => void).call(m, plan.opfsPath, "");
              }
            } catch {
              // ignore
            }
          }
        }
        changed = true;
      }
    } else {
      const cow = diskMetaToOpfsCowPaths(msg.hdd);
      if (!cow) {
        throw new Error(
          `setBootDisks: HDD is not OPFS-backed (cannot attach in machine_cpu.worker). ` +
            `disk=${String((msg.hdd as { name?: unknown }).name ?? "")} id=${String((msg.hdd as { id?: unknown }).id ?? "")}`,
        );
      }

      const setPrimary =
        (m as unknown as { set_primary_hdd_opfs_cow?: unknown }).set_primary_hdd_opfs_cow ??
        (m as unknown as { setPrimaryHddOpfsCow?: unknown }).setPrimaryHddOpfsCow;
      if (typeof setPrimary !== "function") {
        throw new Error("Machine.set_primary_hdd_opfs_cow is unavailable in this WASM build.");
      }

      const blockSizeBytes =
        cow.overlayBlockSizeBytes ??
        (await tryReadAerosparseBlockSizeBytesFromOpfs(cow.overlayPath)) ??
        // Default for newly-created overlays when no metadata/header is available.
        DEFAULT_PRIMARY_HDD_OVERLAY_BLOCK_SIZE_BYTES;
      // Always pass a non-zero block size hint. Older builds that accept only 2 args will ignore it.
      try {
        await maybeAwait(
          (setPrimary as (basePath: string, overlayPath: string, overlayBlockSizeBytes: number) => unknown).call(
            m,
            cow.basePath,
            cow.overlayPath,
            blockSizeBytes,
          ),
        );
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(
          `setBootDisks: failed to attach primary HDD (disk_id=0) ` +
            `base=${cow.basePath} overlay=${cow.overlayPath}: ${message}`,
        );
      }

      // Best-effort overlay ref: ensure machine snapshots record the base/overlay paths even if
      // `set_primary_hdd_opfs_cow` does not populate the `DISKS` overlay refs in this WASM build.
      try {
        const setRef =
          (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
          (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
        if (typeof setRef === "function") {
          (setRef as (base: string, overlay: string) => void).call(m, cow.basePath, cow.overlayPath);
        }
      } catch {
        // ignore
      }
      changed = true;
    }
  } else {
    // Best-effort: clear HDD overlay refs when the slot is cleared so future snapshots do not
    // persist stale disk paths.
    try {
      const clearRef =
        (m as unknown as { clear_ahci_port0_disk_overlay_ref?: unknown }).clear_ahci_port0_disk_overlay_ref ??
        (m as unknown as { clearAhciPort0DiskOverlayRef?: unknown }).clearAhciPort0DiskOverlayRef;
      if (typeof clearRef === "function") {
        (clearRef as () => void).call(m);
        changed = true;
      } else {
        const setRef =
          (m as unknown as { set_ahci_port0_disk_overlay_ref?: unknown }).set_ahci_port0_disk_overlay_ref ??
          (m as unknown as { setAhciPort0DiskOverlayRef?: unknown }).setAhciPort0DiskOverlayRef;
        if (typeof setRef === "function") {
          (setRef as (base: string, overlay: string) => void).call(m, "", "");
          changed = true;
        }
      }
    } catch {
      // ignore
    }
  }

  if (!msg.cd) {
    // Best-effort: allow detaching install media when the selection removes it.
    const eject =
      (m as unknown as { eject_install_media?: unknown }).eject_install_media ??
      (m as unknown as { ejectInstallMedia?: unknown }).ejectInstallMedia;
    if (typeof eject === "function") {
      try {
        await maybeAwait((eject as () => unknown).call(m));
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(`setBootDisks: failed to eject install media: ${message}`);
      }
      changed = true;
    }

    // Best-effort: clear CD overlay refs when the slot is cleared.
    try {
      const clearRef =
        (m as unknown as { clear_ide_secondary_master_atapi_overlay_ref?: unknown }).clear_ide_secondary_master_atapi_overlay_ref ??
        (m as unknown as { clearIdeSecondaryMasterAtapiOverlayRef?: unknown }).clearIdeSecondaryMasterAtapiOverlayRef;
      if (typeof clearRef === "function") {
        (clearRef as () => void).call(m);
        changed = true;
      } else {
        const setRef =
          (m as unknown as { set_ide_secondary_master_atapi_overlay_ref?: unknown }).set_ide_secondary_master_atapi_overlay_ref ??
          (m as unknown as { setIdeSecondaryMasterAtapiOverlayRef?: unknown }).setIdeSecondaryMasterAtapiOverlayRef;
        if (typeof setRef === "function") {
          (setRef as (base: string, overlay: string) => void).call(m, "", "");
          changed = true;
        }
      }
    } catch {
      // ignore
    }
  }

  if (msg.cd) {
    const plan = planMachineBootDiskAttachment(msg.cd, "cd");
    const isoPath = plan.opfsPath;

    const attachIso =
      (m as unknown as { attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref?: unknown })
        .attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref ??
      (m as unknown as { attachIdeSecondaryMasterIsoOpfsExistingAndSetOverlayRef?: unknown })
        .attachIdeSecondaryMasterIsoOpfsExistingAndSetOverlayRef ??
      (m as unknown as { attach_ide_secondary_master_iso_opfs_existing?: unknown }).attach_ide_secondary_master_iso_opfs_existing ??
      (m as unknown as { attachIdeSecondaryMasterIsoOpfsExisting?: unknown }).attachIdeSecondaryMasterIsoOpfsExisting ??
      // Back-compat: some wasm builds expose install-media naming with an `_existing` suffix.
      (m as unknown as { attach_install_media_iso_opfs_existing_and_set_overlay_ref?: unknown })
        .attach_install_media_iso_opfs_existing_and_set_overlay_ref ??
      (m as unknown as { attachInstallMediaIsoOpfsExistingAndSetOverlayRef?: unknown })
        .attachInstallMediaIsoOpfsExistingAndSetOverlayRef ??
      (m as unknown as { attach_install_media_iso_opfs_existing?: unknown }).attach_install_media_iso_opfs_existing ??
      (m as unknown as { attachInstallMediaIsoOpfsExisting?: unknown }).attachInstallMediaIsoOpfsExisting ??
      (m as unknown as { attach_install_media_iso_opfs_and_set_overlay_ref?: unknown }).attach_install_media_iso_opfs_and_set_overlay_ref ??
      (m as unknown as { attachInstallMediaIsoOpfsAndSetOverlayRef?: unknown }).attachInstallMediaIsoOpfsAndSetOverlayRef ??
      (m as unknown as { attach_install_media_iso_opfs?: unknown }).attach_install_media_iso_opfs ??
      (m as unknown as { attachInstallMediaIsoOpfs?: unknown }).attachInstallMediaIsoOpfs ??
      (m as unknown as { attach_install_media_opfs_iso?: unknown }).attach_install_media_opfs_iso ??
      (m as unknown as { attachInstallMediaOpfsIso?: unknown }).attachInstallMediaOpfsIso;

    if (typeof attachIso !== "function") {
      throw new Error("Machine install-media ISO OPFS attach export is unavailable in this WASM build.");
    }

    try {
      await maybeAwait((attachIso as (path: string) => unknown).call(m, isoPath));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`setBootDisks: failed to attach install ISO (disk_id=1) path=${isoPath}: ${message}`);
    }

    // Best-effort overlay ref: some attach APIs do not set DISKS refs; try to do it here when available.
    try {
      const setRef =
        (m as unknown as { set_ide_secondary_master_atapi_overlay_ref?: unknown }).set_ide_secondary_master_atapi_overlay_ref ??
        (m as unknown as { setIdeSecondaryMasterAtapiOverlayRef?: unknown }).setIdeSecondaryMasterAtapiOverlayRef;
      if (typeof setRef === "function") {
        (setRef as (base: string, overlay: string) => void).call(m, isoPath, "");
      }
    } catch {
      // ignore
    }

    changed = true;
  }

  if (changed) {
    // Enable/disable the firmware "CD-first when present" policy when available. When enabled, we
    // keep the configured `boot_drive` as HDD0 and let firmware temporarily switch `DL` to CD0 for
    // the El Torito boot attempt.
    trySetMachineBootFromCdIfPresent(m, useCdFirstPolicy);
    if (useCdFirstPolicy) {
      trySetMachineCdBootDrive(m, BIOS_DRIVE_CD0);
    }

    const bootDriveOk = trySetMachineBootDrive(m, configuredBootDrive);
    if (!bootDriveOk && msg.cd && desiredBootDrive === BIOS_DRIVE_CD0 && !useCdFirstPolicy) {
      throw new Error(
        "Machine boot-drive APIs are unavailable in this WASM build; cannot boot from install media (expected set_boot_drive/setBootDrive or set_boot_device/setBootDevice).",
      );
    }

    try {
      m.reset();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`setBootDisks: Machine.reset failed after disk attachment: ${message}`);
    }

    // Report what firmware actually booted from (CD vs HDD). This differs from the selected policy
    // when the firmware "CD-first when present" fallback is enabled.
    maybePostActiveBootDevice(m);
    maybePostBootConfig(m);
  }

  currentBootDisks = msg;
}

function drainSerialOutput(): void {
  const m = machine;
  if (!m) return;

  if (typeof m.serial_output_len === "function") {
    try {
      const n = m.serial_output_len();
      if (typeof n === "number" && Number.isFinite(n) && n <= 0) return;
    } catch {
      // ignore
    }
  }

  if (typeof m.serial_output !== "function") return;
  const bytes = m.serial_output();
  if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0) return;

  const port = UART_COM1.basePort;
  const chunkBytes = 4096;
  for (let off = 0; off < bytes.byteLength; off += chunkBytes) {
    const chunk = bytes.subarray(off, Math.min(bytes.byteLength, off + chunkBytes));
    pushEvent({ kind: "serialOutput", port, data: chunk });
  }
}

function toTransferableArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const buf = bytes.buffer;
  if (buf instanceof ArrayBuffer) {
    if (bytes.byteOffset === 0 && bytes.byteLength === buf.byteLength) return buf;
    try {
      return buf.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    } catch {
      // fall through to copy
    }
  }

  // Either the view is backed by a non-transferable buffer (e.g. SharedArrayBuffer) or slice
  // failed for some reason. Copy into a new ArrayBuffer-backed typed array.
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}

function processPendingAerogpuFenceCompletions(): void {
  if (pendingAerogpuFenceCompletions.length === 0) return;
  if (vmSnapshotPaused || machineBusy) return;
  if (!aerogpuBridgeEnabled) {
    // Ignore completions until the submission bridge is enabled (we enable it by draining
    // submissions). Calling `aerogpu_complete_fence` enables bridge semantics on the WASM side,
    // so avoid invoking it speculatively while the guest is still running under legacy
    // immediate-fence semantics.
    pendingAerogpuFenceCompletions.length = 0;
    return;
  }

  const m = machine;
  const completeFence = (m as unknown as { aerogpu_complete_fence?: unknown })?.aerogpu_complete_fence;
  if (!m || typeof completeFence !== "function") {
    // If the WASM build doesn't support fence completion, drop queued completions to keep memory
    // bounded (and ensure we don't accidentally enable bridge semantics via drain).
    pendingAerogpuFenceCompletions.length = 0;
    return;
  }

  const fences = pendingAerogpuFenceCompletions.splice(0, pendingAerogpuFenceCompletions.length);
  for (const fence of fences) {
    try {
      (completeFence as (fence: bigint) => void).call(m, fence);
    } catch {
      // ignore
    }
  }
}

function drainAerogpuSubmissions(): void {
  if (vmSnapshotPaused || machineBusy) return;
  const m = machine;
  if (!m || typeof m.aerogpu_drain_submissions !== "function") return;
  // `aerogpu_drain_submissions()` enables the submission bridge on the WASM side, which switches
  // AeroGPU into deferred-fence semantics. Avoid enabling it unless we can also deliver fence
  // completions from the GPU worker.
  if (typeof (m as unknown as { aerogpu_complete_fence?: unknown }).aerogpu_complete_fence !== "function") return;
  const st = status;
  // Before we have enabled the submission bridge, avoid draining (and thus enabling deferred-fence
  // semantics) while the GPU worker is not ready. Once the bridge is enabled, keep draining even
  // during GPU worker restart windows so the device model's bounded submission queue does not
  // overflow; the coordinator will buffer submissions until the GPU worker is READY again.
  const gpuReady = st ? Atomics.load(st, StatusIndex.GpuReady) === 1 : false;
  if (!aerogpuBridgeEnabled && !gpuReady) return;

  let drained: unknown;
  try {
    drained = m.aerogpu_drain_submissions();
    aerogpuBridgeEnabled = true;
  } catch {
    return;
  }

  if (!Array.isArray(drained) || drained.length === 0) return;

  for (const entry of drained) {
    const sub = entry as Partial<{
      cmdStream: unknown;
      signalFence: unknown;
      contextId: unknown;
      flags: unknown;
      engineId: unknown;
      allocTable: unknown;
    }>;
    if (!(sub.cmdStream instanceof Uint8Array)) continue;
    if (typeof sub.signalFence !== "bigint") continue;
    if (typeof sub.contextId !== "number" || !Number.isFinite(sub.contextId)) continue;

    const cmdStream = toTransferableArrayBuffer(sub.cmdStream);
    const transfer: Transferable[] = [cmdStream];

    const allocTableBytes = sub.allocTable;
    let allocTable: ArrayBuffer | undefined;
    if (allocTableBytes instanceof Uint8Array && allocTableBytes.byteLength > 0) {
      allocTable = toTransferableArrayBuffer(allocTableBytes);
      transfer.push(allocTable);
    }

    const flags = typeof sub.flags === "number" && Number.isFinite(sub.flags) ? sub.flags >>> 0 : undefined;
    const engineId = typeof sub.engineId === "number" && Number.isFinite(sub.engineId) ? sub.engineId >>> 0 : undefined;

    const msg: AerogpuSubmitMessage = {
      kind: "aerogpu.submit",
      contextId: sub.contextId >>> 0,
      ...(flags !== undefined ? { flags } : {}),
      ...(engineId !== undefined ? { engineId } : {}),
      signalFence: sub.signalFence,
      cmdStream,
      ...(allocTable ? { allocTable } : {}),
    };
    try {
      ctx.postMessage(msg, transfer);
    } catch {
      // Some runtimes reject transfer lists (or individual buffers may be non-transferable). Fall
      // back to structured clone before giving up. If we still cannot post, force-complete the
      // fence so the guest cannot deadlock (rendering is best-effort in this scenario).
      try {
        ctx.postMessage(msg);
      } catch {
        try {
          const completeFence = (m as unknown as { aerogpu_complete_fence?: unknown }).aerogpu_complete_fence;
          if (typeof completeFence === "function") {
            (completeFence as (fence: bigint) => void).call(m, sub.signalFence);
          }
        } catch {
          // ignore
        }
      }
    }
  }
}

function handleRunExit(exit: unknown): void {
  const st = status;

  const kindNum = (() => {
    const raw = (exit as { kind?: unknown } | null | undefined)?.kind;
    if (typeof raw === "number") return raw | 0;
    if (typeof raw === "function") {
      try {
        const v = (raw as () => unknown).call(exit);
        return typeof v === "number" ? (v | 0) : -1;
      } catch {
        return -1;
      }
    }
    return -1;
  })();

  const detailStr = (() => {
    const raw = (exit as { detail?: unknown } | null | undefined)?.detail;
    if (typeof raw === "string") return raw;
    if (typeof raw === "function") {
      try {
        const v = (raw as () => unknown).call(exit);
        return typeof v === "string" ? v : String(v);
      } catch {
        return "";
      }
    }
    return "";
  })();

  if (kindNum === runExitKindMap.Completed || kindNum === runExitKindMap.Halted) {
    return;
  }

  if (kindNum === runExitKindMap.ResetRequested) {
    // Guest requested a reset (reboot). For install media use-cases, boot from the ISO once then
    // switch to HDD0 on the first guest reset so setup can reboot into the newly-installed OS while
    // keeping the ISO attached for later file access.
    const hasHdd =
      !!currentBootDisks?.hdd ||
      (typeof currentBootDisks?.mounts?.hddId === "string" && currentBootDisks.mounts.hddId.trim().length > 0);
    if (pendingBootDevice === "cdrom" && hasHdd) {
      pendingBootDevice = "hdd";
      postBootDeviceSelected("hdd");
    }

    // Best-effort: update the BIOS boot policy *before* handing off to the coordinator reset path.
    // This keeps behaviour deterministic even if the coordinator resets without re-running
    // `setBootDisks` first.
    const m = machine;
    if (m) {
      try {
        const drive = pendingBootDevice === "cdrom" ? BIOS_DRIVE_CD0 : BIOS_DRIVE_HDD0;
        if (drive === BIOS_DRIVE_HDD0) {
          // Avoid looping back into install media on post-install guest resets.
          trySetMachineBootFromCdIfPresent(m, false);
        }
        trySetMachineBootDrive(m, drive);
        maybePostBootConfig(m);
      } catch {
        // ignore
      }
    }

    // Reset requests are rare but important; use a blocking push so the coordinator reliably
    // observes the event and can reset all workers while preserving guest RAM.
    pushEventBlocking({ kind: "resetRequest" }, 250);
    running = false;
    if (st) setReadyFlag(st, role, false);
    return;
  }

  if (kindNum === runExitKindMap.CpuExit && /triplefault/i.test(detailStr)) {
    pushEventBlocking({ kind: "tripleFault" }, 250);
  } else if (kindNum === runExitKindMap.Exception) {
    pushEventBlocking({ kind: "panic", message: `Exception: ${detailStr || "unknown"}` }, 250);
  } else if (kindNum === runExitKindMap.CpuExit) {
    pushEventBlocking({ kind: "panic", message: `CPU exit: ${detailStr || "unknown"}` }, 250);
  } else if (kindNum === runExitKindMap.Assist) {
    pushEventBlocking({ kind: "panic", message: `Assist: ${detailStr || "unknown"}` }, 250);
  } else {
    pushEventBlocking({ kind: "panic", message: `Machine exited with kind=${kindNum}${detailStr ? `: ${detailStr}` : ""}` }, 250);
  }

  running = false;
  if (st) setReadyFlag(st, role, false);
  ctx.close();
}

async function handleMachineOp(op: PendingMachineOp): Promise<void> {
  const api = wasmApi;
  const m = machine;

  if (!api || !m) {
    const error = serializeError(new Error("WASM Machine is not initialized."));
    if (op.kind === "vm.machine.saveToOpfs") {
      postVmSnapshot({ kind: "vm.snapshot.machine.saved", requestId: op.requestId, ok: false, error } satisfies VmSnapshotMachineSavedMessage);
    } else if (op.kind === "vm.machine.restoreFromOpfs") {
      postVmSnapshot({
        kind: "vm.snapshot.machine.restored",
        requestId: op.requestId,
        ok: false,
        error,
      } satisfies VmSnapshotMachineRestoredMessage);
    } else {
      postSnapshot({ kind: "machine.snapshot.restored", requestId: op.requestId, ok: false, error } satisfies MachineSnapshotRestoredMessage);
    }
    return;
  }

  setMachineBusy(true);
  detachMachineNetwork();

  try {
    if (op.kind === "vm.machine.saveToOpfs") {
      if (!vmSnapshotPaused) {
        throw new Error("VM is not paused; call vm.snapshot.pause before saving.");
      }
      const fn =
        (m as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs ??
        (m as unknown as { snapshotFullToOpfs?: unknown }).snapshotFullToOpfs ??
        (m as unknown as { snapshot_dirty_to_opfs?: unknown }).snapshot_dirty_to_opfs ??
        (m as unknown as { snapshotDirtyToOpfs?: unknown }).snapshotDirtyToOpfs;
      if (typeof fn === "function") {
        await maybeAwait((fn as (path: string) => unknown).call(m, op.path));
      } else {
        const snapBytesFn =
          (m as unknown as { snapshot_full?: unknown }).snapshot_full ??
          (m as unknown as { snapshotFull?: unknown }).snapshotFull ??
          (m as unknown as { snapshot_dirty?: unknown }).snapshot_dirty ??
          (m as unknown as { snapshotDirty?: unknown }).snapshotDirty;
        if (typeof snapBytesFn !== "function") {
          throw new Error("Machine snapshot exports are unavailable in this WASM build.");
        }
        const bytes = await maybeAwait((snapBytesFn as () => unknown).call(m));
        if (!(bytes instanceof Uint8Array)) {
          throw new Error("Machine snapshot returned invalid bytes.");
        }
        const handle = await openFileHandle(op.path, { create: true });
        let writable: FileSystemWritableFileStream;
        let truncateFallback = false;
        try {
          writable = await handle.createWritable({ keepExistingData: false });
        } catch {
          // Some implementations may not accept options; fall back to default.
          writable = await handle.createWritable();
          truncateFallback = true;
        }
        if (truncateFallback) {
          // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
          // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
          try {
            await writable.truncate(0);
          } catch {
            // ignore
          }
        }
        try {
          await writable.write(toArrayBufferUint8(bytes));
          await writable.close();
        } catch (err) {
          try {
            await writable.abort(err);
          } catch {
            // ignore abort failures
          }
          throw err;
        }
      }
      postVmSnapshot({ kind: "vm.snapshot.machine.saved", requestId: op.requestId, ok: true } satisfies VmSnapshotMachineSavedMessage);
      return;
    }

    if (op.kind === "vm.machine.restoreFromOpfs") {
      if (!vmSnapshotPaused) {
        throw new Error("VM is not paused; call vm.snapshot.pause before restoring.");
      }
      const restoreFromOpfs =
        (m as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs ??
        (m as unknown as { restoreSnapshotFromOpfs?: unknown }).restoreSnapshotFromOpfs;
      if (typeof restoreFromOpfs === "function") {
        await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine: m, path: op.path, logPrefix: "machine_cpu.worker" });
      } else {
        const handle = await openFileHandle(op.path, { create: false });
        const file = await handle.getFile();
        const buf = await file.arrayBuffer();
        await restoreMachineSnapshotAndReattachDisks({
          api,
          machine: m,
          bytes: new Uint8Array(buf),
          logPrefix: "machine_cpu.worker",
        });
      }
      // Snapshot restore can change BIOS state (including what the machine thinks it booted from).
      // Publish the updated active boot device for debug/automation tooling.
      maybePostActiveBootDevice(m);
      maybePostBootConfig(m);
      invalidateInputStateAfterSnapshotRestore();
      postVmSnapshot({ kind: "vm.snapshot.machine.restored", requestId: op.requestId, ok: true } satisfies VmSnapshotMachineRestoredMessage);
      return;
    }

    if (op.kind === "machine.restoreFromOpfs") {
      const restoreFromOpfs =
        (m as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs ??
        (m as unknown as { restoreSnapshotFromOpfs?: unknown }).restoreSnapshotFromOpfs;
      if (typeof restoreFromOpfs === "function") {
        await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine: m, path: op.path, logPrefix: "machine_cpu.worker" });
      } else {
        const handle = await openFileHandle(op.path, { create: false });
        const file = await handle.getFile();
        const buf = await file.arrayBuffer();
        await restoreMachineSnapshotAndReattachDisks({
          api,
          machine: m,
          bytes: new Uint8Array(buf),
          logPrefix: "machine_cpu.worker",
        });
      }
      // Snapshot restore can change BIOS state (including what the machine thinks it booted from).
      // Publish the updated active boot device for debug/automation tooling.
      maybePostActiveBootDevice(m);
      maybePostBootConfig(m);
      invalidateInputStateAfterSnapshotRestore();
      postSnapshot({ kind: "machine.snapshot.restored", requestId: op.requestId, ok: true });
      return;
    }

    await restoreMachineSnapshotAndReattachDisks({ api, machine: m, bytes: op.bytes, logPrefix: "machine_cpu.worker" });
    // Snapshot restore can change BIOS state (including what the machine thinks it booted from).
    // Publish the updated active boot device for debug/automation tooling.
    maybePostActiveBootDevice(m);
    maybePostBootConfig(m);
    invalidateInputStateAfterSnapshotRestore();
    postSnapshot({ kind: "machine.snapshot.restored", requestId: op.requestId, ok: true });
  } catch (err) {
    const error = serializeError(err);
    if (op.kind === "vm.machine.saveToOpfs") {
      postVmSnapshot({ kind: "vm.snapshot.machine.saved", requestId: op.requestId, ok: false, error } satisfies VmSnapshotMachineSavedMessage);
    } else if (op.kind === "vm.machine.restoreFromOpfs") {
      postVmSnapshot({ kind: "vm.snapshot.machine.restored", requestId: op.requestId, ok: false, error } satisfies VmSnapshotMachineRestoredMessage);
    } else {
      postSnapshot({ kind: "machine.snapshot.restored", requestId: op.requestId, ok: false, error } satisfies MachineSnapshotRestoredMessage);
    }
  } finally {
    setMachineBusy(false);
    if (networkWanted) {
      attachMachineNetwork();
    }
  }
}

async function runLoop(): Promise<void> {
  const ring = commandRing;
  const st = status;
  if (!ring || !st) return;

  let nextHeartbeatMs = nowMs();
  let ringWaitPromise: Promise<unknown> | null = null;

  try {
    while (Atomics.load(st, StatusIndex.StopRequested) !== 1) {
      while (true) {
        const bytes = ring.tryPop();
        if (!bytes) break;

        let cmd: Command;
        try {
          cmd = decodeCommand(bytes);
        } catch {
          continue;
        }

        if (cmd.kind === "nop") {
          running = true;
          pushEvent({ kind: "ack", seq: cmd.seq } satisfies Event);
        } else if (cmd.kind === "shutdown") {
          Atomics.store(st, StatusIndex.StopRequested, 1);
        }
      }

      if (Atomics.load(st, StatusIndex.StopRequested) === 1) break;

      const now = nowMs();
      if (now >= nextHeartbeatMs) {
        const counter = Atomics.add(st, StatusIndex.HeartbeatCounter, 1) + 1;
        pushEvent({ kind: "ack", seq: counter });
        publishInputBackendStatusFromMachine();
        nextHeartbeatMs = now + HEARTBEAT_INTERVAL_MS;
      }

      // Keep network attachment in sync with config.
      if (!machineBusy && machine) {
        if (networkWanted && !networkAttached) {
          attachMachineNetwork();
        } else if (!networkWanted && networkAttached) {
          detachMachineNetwork();
        }
      }

      // Snapshot + restore operations.
      const op = pendingMachineOps.shift();
      if (op) {
        await handleMachineOp(op);
        await new Promise((resolve) => {
          const timer = setTimeout(resolve, 0);
          (timer as unknown as { unref?: () => void }).unref?.();
        });
        continue;
      }

      // Boot disks.
      if (!vmSnapshotPaused && pendingBootDisks && machine && !machineBusy) {
        const msg = pendingBootDisks;
        pendingBootDisks = null;
        setMachineBusy(true);
        try {
          await applyBootDisks(msg);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          const hddId = msg.hdd ? String((msg.hdd as { id?: unknown }).id ?? "") : "";
          const hddName = msg.hdd ? String((msg.hdd as { name?: unknown }).name ?? "") : "";
          const cdId = msg.cd ? String((msg.cd as { id?: unknown }).id ?? "") : "";
          const cdName = msg.cd ? String((msg.cd as { name?: unknown }).name ?? "") : "";
          const contextParts: string[] = [];
          if (msg.hdd) contextParts.push(`hdd=${hddName || "?"}#${hddId || "?"}`);
          if (msg.cd) contextParts.push(`cd=${cdName || "?"}#${cdId || "?"}`);
          const context = contextParts.length ? ` (${contextParts.join(", ")})` : "";
          const fullMessage = `[machine_cpu] setBootDisks failed${context}: ${message}`;
          pushEvent({ kind: "log", level: "error", message: fullMessage });
          setReadyFlag(st, role, false);
          post({ type: MessageType.ERROR, role, message: fullMessage } satisfies ProtocolMessage);
          ctx.close();
          return;
        } finally {
          setMachineBusy(false);
        }

        await new Promise((resolve) => {
          const timer = setTimeout(resolve, 0);
          (timer as unknown as { unref?: () => void }).unref?.();
        });
        continue;
      }

      // Flush queued input (from pause or async ops) when safe. If the machine isn't initialized
      // (e.g. Node worker_threads tests), still recycle buffers on resume to avoid leaks.
      if (!vmSnapshotPaused && !machineBusy && queuedInputBatches.length) {
        const batches = Math.min(MAX_INPUT_BATCHES_PER_TICK, queuedInputBatches.length);
        for (let i = 0; i < batches; i += 1) {
          const entry = queuedInputBatches.shift();
          if (!entry) break;
          queuedInputBatchBytes = Math.max(0, queuedInputBatchBytes - (entry.buffer.byteLength >>> 0));
          if (machine) {
            handleInputBatch(entry.buffer);
          }
          if (entry.recycle) {
            postInputBatchRecycle(entry.buffer);
          }
        }
      }

      // Drain any AeroGPU fence completions forwarded from the GPU worker when safe. This ensures
      // the guest sees forward progress once the submission bridge is enabled.
      if (!vmSnapshotPaused && !machineBusy) {
        processPendingAerogpuFenceCompletions();
      }

      if (!running || !machine || vmSnapshotPaused || machineBusy) {
        // The run loop waits primarily on coordinator-issued commands (via the command ring),
        // but snapshot orchestration and input delivery arrive via `postMessage`. Race the ring
        // wait against a lightweight JS wakeup promise so we respond promptly to those messages.
        if (!ringWaitPromise) {
          ringWaitPromise = ring.waitForDataAsync(HEARTBEAT_INTERVAL_MS).finally(() => {
            ringWaitPromise = null;
          });
        }
        await Promise.race([ringWaitPromise, ensureRunLoopWakePromise()]);
        continue;
      }

      const exit = machine.run_slice(RUN_SLICE_MAX_INSTS);
      const exitKind = (exit as unknown as { kind?: unknown }).kind;
      let exitKindNum = -1;
      if (typeof exitKind === "number") {
        exitKindNum = exitKind | 0;
      } else if (typeof exitKind === "function") {
        try {
          const v = (exitKind as () => unknown).call(exit);
          if (typeof v === "number") exitKindNum = v | 0;
        } catch {
          // ignore
        }
      }
      handleRunExit(exit);
      drainSerialOutput();
      drainAerogpuSubmissions();
      try {
        (exit as unknown as { free?: () => void }).free?.();
      } catch {
        // ignore
      }

      if (exitKindNum === runExitKindMap.Halted) {
        // `Promise.race` treats non-promises as immediately resolved values. Ensure we always race
        // against an actual wakeup promise; otherwise the halted loop can devolve into an endless
        // chain of microtasks (`await null`) that starves `message` events (input batches).
        await Promise.race([ring.waitForDataAsync(HALTED_RUN_SLICE_DELAY_MS), ensureRunLoopWakePromise()]);
      } else {
        await new Promise((resolve) => {
          const timer = setTimeout(resolve, 0);
          (timer as unknown as { unref?: () => void }).unref?.();
        });
      }
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEvent({ kind: "panic", message } satisfies Event);
    setReadyFlag(st, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  } finally {
    setReadyFlag(st, role, false);
    if (machine) {
      try {
        (machine as unknown as { free?: () => void }).free?.();
      } catch {
        // ignore
      }
      machine = null;
    }
    pendingAerogpuFenceCompletions.length = 0;
    networkAttached = false;
  }
}

async function initWasmInBackground(init: WorkerInitMessage, guestMemory: WebAssembly.Memory): Promise<void> {
  if (isNodeWorkerThreads()) return;

  try {
    const { initWasmForContext } = await import("../runtime/wasm_context");
    const { assertWasmMemoryWiring } = await import("../runtime/wasm_memory_probe");

    const { api, variant } = await initWasmForContext({
      variant: init.wasmVariant,
      module: init.wasmModule,
      memory: guestMemory,
    });

    wasmApi = api as WasmApi;

    assertWasmMemoryWiring({ api, memory: guestMemory, context: "machine_cpu.worker" });

    const value = typeof api.add === "function" ? api.add(20, 22) : 0;
    const st = status;
    if (st && Atomics.load(st, StatusIndex.StopRequested) === 1) return;
    post({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);

    const layout = guestLayout;
    if (!layout) return;

    machine = await createWin7MachineWithSharedGuestMemory(api as WasmApi, layout);
    if (machine) {
      verifyWasmSharedStateLayout(machine, init, guestMemory);
    }

    // Attach optional network backend if enabled.
    if (networkWanted) {
      attachMachineNetwork();
    }

    try {
      machine.reset();
    } catch {
      // ignore
    }
    maybePostActiveBootDevice(machine);
    maybePostBootConfig(machine);
    publishInputBackendStatusFromMachine();

    // WASM init completes asynchronously relative to the main run loop. If the run loop is
    // currently waiting on the command ring heartbeat timeout, wake it so pending boot disk
    // attachment and network config changes apply without an extra delay.
    wakeRunLoop();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[machine_cpu.worker] WASM init failed (continuing without WASM):", err);
  }
}

async function initAndRun(init: WorkerInitMessage): Promise<void> {
  role = init.role ?? "cpu";

  try {
    const segments: SharedMemorySegments = {
      control: init.controlSab,
      guestMemory: init.guestMemory,
      vram: init.vram,
      scanoutState: init.scanoutState,
      scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
      cursorState: init.cursorState,
      cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
      ioIpc: init.ioIpcSab,
      sharedFramebuffer: init.sharedFramebuffer,
      sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes,
    } satisfies SharedMemorySegments;

    const views = createSharedMemoryViews(segments);
    status = views.status;
    guestLayout = views.guestLayout;
    ioIpcSab = segments.ioIpc;
    initInputDiagnosticsTelemetry();

    const regions = ringRegionsForWorker(role);
    commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
    eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

    setReadyFlag(status, role, true);
    post({ type: MessageType.READY, role } satisfies ProtocolMessage);

    void initWasmInBackground(init, init.guestMemory);

    if (!started) {
      started = true;
      void runLoop();
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (status) setReadyFlag(status, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

ctx.onmessage = (ev) => {
  const msg = ev.data as unknown;

  // Test-only hook (Node worker_threads): allow unit tests to enable a dummy machine instance so
  // input-batch parsing + telemetry can be exercised without loading WASM.
  if (isNodeWorkerThreads() && (msg as { kind?: unknown }).kind === "__test.machine_cpu.enableDummyMachine") {
    const payload = msg as Partial<{
      virtioKeyboardOk: unknown;
      virtioMouseOk: unknown;
      usbKeyboardOk?: unknown;
      enableBootDriveSpy?: unknown;
      enableNetworkSpy?: unknown;
      enableAerogpuBridge?: unknown;
      enableInputSpy?: unknown;
    }>;
    const virtioKeyboardOk = payload.virtioKeyboardOk === true;
    const virtioMouseOk = payload.virtioMouseOk === true;
    const usbKeyboardOk = payload.usbKeyboardOk === true;
    const enableBootDriveSpy = payload.enableBootDriveSpy === true;
    const enableNetworkSpy = payload.enableNetworkSpy === true;
    const enableAerogpuBridge = payload.enableAerogpuBridge !== false;
    const enableInputSpy = payload.enableInputSpy === true;

    // Keep some minimal internal state so we can exercise boot-device reporting without loading WASM.
    //
    // Some integration tests also exercise snapshot restore plumbing. Under Node, WASM init is
    // skipped entirely, so populate a minimal `wasmApi` shim so snapshot RPCs treat the dummy
    // machine as a valid `api.Machine` instance.
    if (!wasmApi) {
      wasmApi = {} as unknown as WasmApi;
    }
    let dummyBootDrive = BIOS_DRIVE_HDD0;
    let dummyBootFromCdIfPresent = false;
    let dummyCdBootDrive = BIOS_DRIVE_CD0;
    let dummyCdAttached = false;
    let dummyActiveBootDevice = 0; // MachineBootDevice::Hdd

    testDummyAerogpuSubmissions = [];
    testDummyNextRunExitKind = null;

    const recomputeDummyActiveBootDevice = (): void => {
      const cdBoot =
        (dummyBootFromCdIfPresent && dummyCdAttached) || (dummyBootDrive >= BIOS_DRIVE_CD0 && dummyBootDrive <= BIOS_DRIVE_CD_LAST);
      dummyActiveBootDevice = cdBoot ? 1 : 0;
    };

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const dummy: any = {
      virtio_input_keyboard_driver_ok: () => virtioKeyboardOk,
      virtio_input_mouse_driver_ok: () => virtioMouseOk,
      usb_hid_keyboard_configured: () => usbKeyboardOk,
      inject_usb_hid_keyboard_usage: (_usage: number, _pressed: boolean) => {
        // No-op: tests can still validate backend selection by toggling `usb_hid_keyboard_configured`.
      },
      inject_usb_hid_consumer_usage: (_usage: number, _pressed: boolean) => {
        // No-op: used by Consumer Control events.
      },
      inject_key_scancode_bytes: (packed: number, len: number) => {
        if (!enableInputSpy) return;
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.inject_key_scancode_bytes", packed: packed >>> 0, len: len >>> 0 });
        } catch {
          void 0;
        }
      },
      run_slice: () => {
        const st = status;
        const runSliceCounterIndex = 63;
        if (st && runSliceCounterIndex < st.length) {
          Atomics.add(st, runSliceCounterIndex, 1);
        }
        const nextExitKind = testDummyNextRunExitKind;
        if (typeof nextExitKind === "number") {
          testDummyNextRunExitKind = null;
          return { kind: nextExitKind };
        }
        return { kind: runExitKindMap.Halted };
      },
      active_boot_device: () => dummyActiveBootDevice,
      boot_drive: () => dummyBootDrive,
      cd_boot_drive: () => dummyCdBootDrive,
      boot_from_cd_if_present: () => dummyBootFromCdIfPresent,
      setBootFromCdIfPresent: (enabled: boolean) => {
        dummyBootFromCdIfPresent = !!enabled;
      },
      setCdBootDrive: (drive: number) => {
        dummyCdBootDrive = drive >>> 0;
      },
      setBootDrive: (drive: number) => {
        dummyBootDrive = drive >>> 0;
      },
      set_primary_hdd_opfs_cow: async (_basePath: string, _overlayPath: string, _overlayBlockSizeBytes: number) => {
        // No-op: tests use this to exercise the boot-drive policy without touching real disks.
      },
      setPrimaryHddOpfsCow: async (_basePath: string, _overlayPath: string, _overlayBlockSizeBytes: number) => {
        // No-op: back-compat for older worker shims that use camelCase exports.
      },
      attach_install_media_opfs_iso: async (_path: string) => {
        dummyCdAttached = true;
      },
      aerogpu_complete_fence: (fence: bigint) => {
        try {
          ctx.postMessage({ type: "__test.machine_cpu.aerogpu_complete_fence", fence });
        } catch {
          void 0;
        }
      },
      aerogpu_drain_submissions: (): unknown[] => {
        const subs = testDummyAerogpuSubmissions;
        if (!subs || subs.length === 0) return [];
        const drained = subs.splice(0, subs.length);
        return drained.map((sub) => ({
          cmdStream: sub.cmdStream,
          signalFence: sub.signalFence,
          contextId: sub.contextId,
          ...(sub.flags !== undefined ? { flags: sub.flags } : {}),
          ...(sub.engineId !== undefined ? { engineId: sub.engineId } : {}),
          allocTable: sub.allocTable ?? null,
        }));
      },
      reset: () => {
        void dummyCdBootDrive; // keep lint quiet; drive number influences boot device only in real firmware.
        recomputeDummyActiveBootDevice();
      },
      restore_snapshot_from_opfs: async (_path: string) => {
        // Simulate a snapshot restore that came from a CD-booted session.
        dummyActiveBootDevice = 1;
      },
      restore_snapshot: (_bytes: Uint8Array) => {
        dummyActiveBootDevice = 1;
      },
      reattach_restored_disks_from_opfs: async () => {
        // No-op: dummy machine does not model disk backends.
      },
    };
    if (enableNetworkSpy) {
      dummy.attachL2TunnelFromIoIpcSab = function (sab: SharedArrayBuffer) {
        if (this !== dummy) {
          throw new Error("attachL2TunnelFromIoIpcSab called with wrong this binding");
        }
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.attachL2TunnelFromIoIpcSab", byteLength: sab.byteLength });
        } catch {
          void 0;
        }
      };
      dummy.detachNetwork = function () {
        if (this !== dummy) {
          throw new Error("detachNetwork called with wrong this binding");
        }
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.detachNetwork" });
        } catch {
          void 0;
        }
      };
    }
    if (enableBootDriveSpy) {
      const baseSetBootDrive = dummy.setBootDrive as (drive: number) => void;
      dummy.setBootDrive = (drive: number) => {
        baseSetBootDrive(drive);
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.setBootDrive", drive: drive >>> 0 });
        } catch {
          void 0;
        }
      };
      const baseAttachIso = dummy.attach_install_media_opfs_iso as (path: string) => Promise<void>;
      dummy.attach_install_media_opfs_iso = async (path: string) => {
        await baseAttachIso(path);
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.attachInstallMediaOpfsIso", path });
        } catch {
          void 0;
        }
      };
      dummy.reset = () => {
        recomputeDummyActiveBootDevice();
        try {
          ctx.postMessage({ kind: "__test.machine_cpu.reset" });
        } catch {
          void 0;
        }
      };
    }
    machine = dummy as unknown as InstanceType<WasmApi["Machine"]>;
    // The dummy machine is only used in Node `worker_threads` integration tests (WASM is disabled),
    // so enable AeroGPU submission-bridge semantics up-front to allow tests to validate the
    // fence-completion forwarding path without requiring a full guest/GPU worker setup.
    //
    // Some tests opt out of this default to validate the "don't drain submissions until GPU READY"
    // gating logic used by the real wasm Machine integration.
    aerogpuBridgeEnabled = enableAerogpuBridge;
    try {
      ctx.postMessage({ kind: "__test.machine_cpu.dummyMachineEnabled" });
    } catch {
      void 0;
    }
    return;
  }

  // Test-only hook (Node worker_threads): ask the dummy machine's next `run_slice()` invocation to
  // return a specific exit kind (e.g. ResetRequested) so tests can exercise coordinator/worker reset
  // plumbing.
  if (isNodeWorkerThreads() && (msg as { kind?: unknown }).kind === "__test.machine_cpu.setDummyNextRunExitKind") {
    const payload = msg as Partial<{ exitKind?: unknown }>;
    const raw = payload.exitKind;
    if (typeof raw === "string") {
      const kind = (runExitKindMap as unknown as Record<string, unknown>)[raw];
      if (typeof kind === "number") {
        testDummyNextRunExitKind = kind | 0;
        wakeRunLoop();
      }
    } else if (typeof raw === "number" && Number.isFinite(raw)) {
      testDummyNextRunExitKind = raw | 0;
      wakeRunLoop();
    }
    return;
  }

  // Test-only hook (Node worker_threads): enqueue synthetic AeroGPU submissions to be returned from
  // the dummy machine's `aerogpu_drain_submissions()` hook.
  if (isNodeWorkerThreads() && (msg as { kind?: unknown }).kind === "__test.machine_cpu.enqueueDummyAerogpuSubmission") {
    const queue = testDummyAerogpuSubmissions;
    if (!queue) return;

    const payload = msg as Partial<{
      cmdStream: unknown;
      allocTable: unknown;
      signalFence: unknown;
      contextId: unknown;
      flags: unknown;
      engineId: unknown;
    }>;

    const cmdStream = payload.cmdStream;
    if (!(cmdStream instanceof Uint8Array) || cmdStream.byteLength === 0) return;
    const signalFence = payload.signalFence;
    if (typeof signalFence !== "bigint") return;
    const contextIdRaw = payload.contextId;
    if (typeof contextIdRaw !== "number" || !Number.isFinite(contextIdRaw)) return;

    const allocTable = payload.allocTable instanceof Uint8Array ? payload.allocTable : null;
    const flags = typeof payload.flags === "number" && Number.isFinite(payload.flags) ? payload.flags >>> 0 : undefined;
    const engineId = typeof payload.engineId === "number" && Number.isFinite(payload.engineId) ? payload.engineId >>> 0 : undefined;

    queue.push({
      cmdStream,
      allocTable,
      signalFence,
      contextId: contextIdRaw >>> 0,
      ...(flags !== undefined ? { flags } : {}),
      ...(engineId !== undefined ? { engineId } : {}),
    });
    wakeRunLoop();
    return;
  }

  const aerogpuComplete = msg as Partial<AerogpuCompleteFenceMessage>;
  if (aerogpuComplete?.kind === "aerogpu.complete_fence") {
    const fence = aerogpuComplete.fence;
    if (typeof fence !== "bigint") return;
    // Keep the queue bounded so pathological worker restart loops cannot grow memory unbounded.
    if (pendingAerogpuFenceCompletions.length >= 4096) {
      pendingAerogpuFenceCompletions.shift();
    }
    pendingAerogpuFenceCompletions.push(fence);
    // Best-effort: process immediately when safe.
    processPendingAerogpuFenceCompletions();
    wakeRunLoop();
    return;
  }

  const input = msg as Partial<InputBatchMessage | InputBatchRecycleMessage>;
  if (input?.type === "in:input-batch") {
    const buffer = input.buffer;
    if (!(buffer instanceof ArrayBuffer)) return;
    const recycle = input.recycle === true;
    const st = status;
    if (st) Atomics.add(st, StatusIndex.IoInputBatchReceivedCounter, 1);

    // Don't call into WASM while snapshot-paused or while an async machine op is in flight
    // (wasm-bindgen `&mut self` reentrancy).
    if (vmSnapshotPaused || machineBusy) {
      queueInputBatch(buffer, recycle);
      return;
    }

    // If the machine isn't ready yet (or WASM init is skipped under Node), we cannot process input
    // batches. Still recycle the transferred buffer when requested so the main-thread pool does not
    // leak memory (and to satisfy worker_threads integration tests).
    if (!machine) {
      if (recycle) {
        postInputBatchRecycle(buffer);
      }
      return;
    }

    handleInputBatch(buffer);
    if (recycle) {
      postInputBatchRecycle(buffer);
    }
    // The CPU worker's main loop may be blocked waiting on the command ring (or throttling a
    // halted CPU). Wake it so queued batch processing + telemetry updates are applied promptly.
    wakeRunLoop();
    return;
  }
  if (input?.type === "in:input-batch-recycle") {
    return;
  }

  const vmSnapshot = msg as Partial<
    VmSnapshotPauseMessage | VmSnapshotResumeMessage | VmSnapshotMachineSaveToOpfsMessage | VmSnapshotMachineRestoreFromOpfsMessage
  >;
  if (typeof vmSnapshot.kind === "string" && vmSnapshot.kind.startsWith("vm.snapshot.")) {
    const requestId = typeof vmSnapshot.requestId === "number" ? vmSnapshot.requestId : -1;
    if (requestId < 0) return;

    switch (vmSnapshot.kind) {
      case "vm.snapshot.pause": {
        vmSnapshotPaused = true;
        if (!machineBusy) {
          postVmSnapshot({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
          return;
        }
        machineIdleWaiters.push(() => {
          postVmSnapshot({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
        });
        return;
      }
      case "vm.snapshot.resume": {
        vmSnapshotPaused = false;
        postVmSnapshot({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
        wakeRunLoop();
        return;
      }
      case "vm.snapshot.machine.saveToOpfs": {
        const path = typeof (vmSnapshot as Partial<VmSnapshotMachineSaveToOpfsMessage>).path === "string" ? vmSnapshot.path : "";
        if (!path) {
          postVmSnapshot({
            kind: "vm.snapshot.machine.saved",
            requestId,
            ok: false,
            error: serializeError(new Error("vm.snapshot.machine.saveToOpfs requires a non-empty path.")),
          } satisfies VmSnapshotMachineSavedMessage);
          return;
        }
        if (!vmSnapshotPaused) {
          postVmSnapshot({
            kind: "vm.snapshot.machine.saved",
            requestId,
            ok: false,
            error: serializeError(new Error("VM is not paused; call vm.snapshot.pause before saving.")),
          } satisfies VmSnapshotMachineSavedMessage);
          return;
        }
        pendingMachineOps.push({ kind: "vm.machine.saveToOpfs", requestId, path });
        wakeRunLoop();
        return;
      }
      case "vm.snapshot.machine.restoreFromOpfs": {
        const path =
          typeof (vmSnapshot as Partial<VmSnapshotMachineRestoreFromOpfsMessage>).path === "string" ? vmSnapshot.path : "";
        if (!path) {
          postVmSnapshot({
            kind: "vm.snapshot.machine.restored",
            requestId,
            ok: false,
            error: serializeError(new Error("vm.snapshot.machine.restoreFromOpfs requires a non-empty path.")),
          } satisfies VmSnapshotMachineRestoredMessage);
          return;
        }
        if (!vmSnapshotPaused) {
          postVmSnapshot({
            kind: "vm.snapshot.machine.restored",
            requestId,
            ok: false,
            error: serializeError(new Error("VM is not paused; call vm.snapshot.pause before restoring.")),
          } satisfies VmSnapshotMachineRestoredMessage);
          return;
        }
        pendingMachineOps.push({ kind: "vm.machine.restoreFromOpfs", requestId, path });
        wakeRunLoop();
        return;
      }
    }
  }

  const snapshot = msg as Partial<MachineSnapshotRestoreFromOpfsMessage | MachineSnapshotRestoreMessage>;
  if (snapshot?.kind === "machine.snapshot.restoreFromOpfs") {
    const requestId = typeof snapshot.requestId === "number" ? snapshot.requestId : -1;
    const path = typeof snapshot.path === "string" ? snapshot.path : "";
    if (requestId < 0 || !path) {
      postSnapshot({
        kind: "machine.snapshot.restored",
        requestId: requestId < 0 ? 0 : requestId,
        ok: false,
        error: serializeError(new Error("Invalid machine.snapshot.restoreFromOpfs message.")),
      });
      return;
    }
    pendingMachineOps.push({ kind: "machine.restoreFromOpfs", requestId, path });
    wakeRunLoop();
    return;
  }

  if (snapshot?.kind === "machine.snapshot.restore") {
    const requestId = typeof snapshot.requestId === "number" ? snapshot.requestId : -1;
    const bytes = snapshot.bytes;
    if (requestId < 0 || !(bytes instanceof ArrayBuffer)) {
      postSnapshot({
        kind: "machine.snapshot.restored",
        requestId: requestId < 0 ? 0 : requestId,
        ok: false,
        error: serializeError(new Error("Invalid machine.snapshot.restore message.")),
      });
      return;
    }
    pendingMachineOps.push({ kind: "machine.restore", requestId, bytes: new Uint8Array(bytes) });
    wakeRunLoop();
    return;
  }

  const bootDisks = normalizeSetBootDisksMessage(msg);
  if (bootDisks) {
    try {
      const warnings: string[] = [];
      if (bootDisks.hdd) warnings.push(...planMachineBootDiskAttachment(bootDisks.hdd, "hdd").warnings);
      if (bootDisks.cd) warnings.push(...planMachineBootDiskAttachment(bootDisks.cd, "cd").warnings);
      for (const w of warnings) {
        // eslint-disable-next-line no-console
        console.warn(`[machine_cpu.worker] ${w}`);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      post({ type: MessageType.ERROR, role, message, code: ErrorCode.BOOT_DISKS_INCOMPATIBLE } satisfies ProtocolMessage);
      return;
    }

    // Disk selection changes are keyed off mount IDs (DiskManager selection), not disk metadata.
    // The metadata objects can be null/late-loaded while the mount IDs still represent the user's
    // intent. Comparing mount IDs avoids unintentionally resetting boot-device policy when only
    // metadata changes.
    const prevHddId = typeof currentBootDisks?.mounts?.hddId === "string" ? currentBootDisks.mounts.hddId : "";
    const prevCdId = typeof currentBootDisks?.mounts?.cdId === "string" ? currentBootDisks.mounts.cdId : "";
    const nextHddId = typeof bootDisks.mounts?.hddId === "string" ? bootDisks.mounts.hddId : "";
    const nextCdId = typeof bootDisks.mounts?.cdId === "string" ? bootDisks.mounts.cdId : "";
    const disksChanged = prevHddId !== nextHddId || prevCdId !== nextCdId;

    const explicitBootDevice = bootDisks.bootDevice;
    if (explicitBootDevice === "cdrom" && nextCdId) {
      pendingBootDevice = "cdrom";
    } else if (explicitBootDevice === "hdd" && nextHddId) {
      pendingBootDevice = "hdd";
    } else if (disksChanged) {
      // Default boot-device policy for new disk selections:
      // - if install media is mounted, start with a CD boot so BIOS can El Torito boot it.
      // - otherwise boot HDD.
      pendingBootDevice = nextCdId ? "cdrom" : "hdd";
    }

    // Publish the selected policy for tests/debug tooling.
    postBootDeviceSelected(pendingBootDevice);

    pendingBootDisks = bootDisks;
    wakeRunLoop();
    return;
  }

  if ((msg as { kind?: unknown }).kind === "config.update") {
    const update = msg as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    networkWanted = isNetworkingEnabled(currentConfig);
    wakeRunLoop();
    post({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind === "init") {
    void initAndRun(init as WorkerInitMessage);
  }
};
