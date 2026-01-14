/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { UART_COM1 } from "../io/devices/uart16550";
import { InputEventType } from "../input/event_queue";
import { normalizeSetBootDisksMessage, type SetBootDisksMessage } from "../runtime/boot_disks_protocol";
import { planMachineBootDiskAttachment } from "../runtime/machine_disk_attach";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
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

/**
 * Canonical `api.Machine` CPU worker entrypoint.
 *
 * This worker participates in the coordinator's standard `config.update` + `init` protocol and
 * runs the canonical `api.Machine` (shared guest RAM) when WASM assets are available.
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

let pendingBootDisks: SetBootDisksMessage | null = null;

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

const HEARTBEAT_INTERVAL_MS = 250;
const RUN_SLICE_MAX_INSTS = 50_000;

const MAX_INPUT_BATCHES_PER_TICK = 8;
const MAX_QUEUED_INPUT_BATCH_BYTES = 4 * 1024 * 1024;
let queuedInputBatchBytes = 0;
const queuedInputBatches: Array<{ buffer: ArrayBuffer; recycle: boolean }> = [];

// Avoid per-event allocations when falling back to `inject_keyboard_bytes` (older WASM builds).
// Preallocate small scancode buffers for len=1..4.
const packedScancodeScratch = [new Uint8Array(0), new Uint8Array(1), new Uint8Array(2), new Uint8Array(3), new Uint8Array(4)];

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

function post(msg: ProtocolMessage | ConfigAckMessage): void {
  ctx.postMessage(msg);
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

function pushEventBlocking(evt: Event, timeoutMs?: number): void {
  const ring = eventRing;
  if (!ring) return;
  try {
    ring.pushBlocking(encodeEvent(evt), timeoutMs);
  } catch {
    // best-effort
  }
}

function serializeError(err: unknown): MachineSnapshotSerializedError {
  return serializeVmSnapshotError(err);
}

function isNodeWorkerThreads(): boolean {
  // Avoid referencing `process` directly so this file remains valid in browser builds without polyfills.
  const p = (globalThis as unknown as { process?: unknown }).process as { versions?: { node?: unknown } } | undefined;
  return typeof p?.versions?.node === "string";
}

async function maybeAwait(result: unknown): Promise<unknown> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const then = (result as any)?.then;
  if (typeof then === "function") {
    return await (result as Promise<unknown>);
  }
  return result;
}

function isNetworkingEnabled(config: AeroConfig | null): boolean {
  // Option C (L2 tunnel) is enabled when proxyUrl is configured.
  return !!(config?.proxyUrl && config.proxyUrl.trim().length > 0);
}

function detachMachineNetwork(): void {
  const m = machine;
  if (!m) return;
  try {
    const fn =
      (m as unknown as { detach_network?: unknown }).detach_network ??
      (m as unknown as { detach_net_rings?: unknown }).detach_net_rings;
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
    const attachFromSab = (m as unknown as { attach_l2_tunnel_from_io_ipc_sab?: unknown }).attach_l2_tunnel_from_io_ipc_sab;
    if (typeof attachFromSab === "function") {
      (attachFromSab as (sab: SharedArrayBuffer) => void).call(m, sab);
      networkAttached = true;
      return;
    }

    const attachRings =
      (m as unknown as { attach_l2_tunnel_rings?: unknown }).attach_l2_tunnel_rings ??
      (m as unknown as { attach_net_rings?: unknown }).attach_net_rings;
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

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const anyApi = api as any;
  const Machine = anyApi.Machine;
  if (!Machine) {
    throw new Error("Machine wasm export is unavailable; cannot start machine_cpu.worker.");
  }

  type Candidate = { name: string; fn: unknown; thisArg: unknown };
  const candidates: Candidate[] = [
    { name: "Machine.new_win7_storage_shared", fn: Machine.new_win7_storage_shared, thisArg: Machine },
    { name: "Machine.new_shared", fn: Machine.new_shared, thisArg: Machine },

    // Back-compat shims for intermediate builds.
    { name: "Machine.new_win7_storage_shared_guest_memory", fn: Machine.new_win7_storage_shared_guest_memory, thisArg: Machine },
    { name: "Machine.new_shared_guest_memory_win7_storage", fn: Machine.new_shared_guest_memory_win7_storage, thisArg: Machine },
    { name: "Machine.new_shared_guest_memory", fn: Machine.new_shared_guest_memory, thisArg: Machine },
    { name: "Machine.from_shared_guest_memory_win7_storage", fn: Machine.from_shared_guest_memory_win7_storage, thisArg: Machine },
    { name: "Machine.from_shared_guest_memory", fn: Machine.from_shared_guest_memory, thisArg: Machine },

    // Free-function factories (older wasm-bindgen exports).
    { name: "create_win7_machine_shared_guest_memory", fn: anyApi.create_win7_machine_shared_guest_memory, thisArg: anyApi },
    { name: "create_machine_win7_shared_guest_memory", fn: anyApi.create_machine_win7_shared_guest_memory, thisArg: anyApi },
    { name: "create_machine_shared_guest_memory_win7", fn: anyApi.create_machine_shared_guest_memory_win7, thisArg: anyApi },
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

  throw new Error(
    "Shared-guest-memory Machine constructor is unavailable in this WASM build. " +
      "Expected Machine.new_win7_storage_shared(guestBase, guestSize) (or an equivalent factory).",
  );
}

function postInputBatchRecycle(buffer: ArrayBuffer): void {
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
  } else if (recycle) {
    // Drop excess input to keep memory bounded; best-effort recycle the transferred buffer.
    postInputBatchRecycle(buffer);
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  const m = machine;
  if (!m) return;

  const decoded = validateInputBatchBuffer(buffer);
  if (!decoded.ok) return;

  const { words, count } = decoded;
  const base = INPUT_BATCH_HEADER_WORDS;
  for (let i = 0; i < count; i += 1) {
    const off = base + i * INPUT_BATCH_WORDS_PER_EVENT;
    const type = words[off] >>> 0;
    if (type === InputEventType.KeyScancode) {
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
    } else if (type === InputEventType.MouseMove) {
      const dx = words[off + 2] | 0;
      const dyPs2 = words[off + 3] | 0;
      try {
        if (typeof m.inject_ps2_mouse_motion === "function") {
          m.inject_ps2_mouse_motion(dx, dyPs2, 0);
        } else if (typeof m.inject_mouse_motion === "function") {
          m.inject_mouse_motion(dx, -dyPs2, 0);
        }
      } catch {
        // ignore
      }
    } else if (type === InputEventType.MouseWheel) {
      const dz = words[off + 2] | 0;
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
    } else if (type === InputEventType.MouseButtons) {
      const buttons = words[off + 2] & 0xff;
      const mask = buttons & 0x1f;
      try {
        if (typeof m.inject_mouse_buttons_mask === "function") {
          m.inject_mouse_buttons_mask(mask);
        } else if (typeof m.inject_ps2_mouse_buttons === "function") {
          m.inject_ps2_mouse_buttons(mask);
        }
      } catch {
        // ignore
      }
    }
  }
}

async function applyBootDisks(msg: SetBootDisksMessage): Promise<void> {
  const m = machine;
  if (!m) return;

  let changed = false;

  if (msg.hdd) {
    const cow = diskMetaToOpfsCowPaths(msg.hdd);
    if (!cow) {
      throw new Error("setBootDisks: HDD is not OPFS-backed (cannot attach in machine_cpu.worker).");
    }

    const setPrimary = (m as unknown as { set_primary_hdd_opfs_cow?: unknown }).set_primary_hdd_opfs_cow;
    if (typeof setPrimary !== "function") {
      throw new Error("Machine.set_primary_hdd_opfs_cow is unavailable in this WASM build.");
    }

    // Some builds may extend the API to accept a block size hint; preserve compatibility.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const anyFn = setPrimary as any;
    const arity = typeof anyFn.length === "number" ? (anyFn.length as number) : 0;
    const res = arity >= 3 ? anyFn.call(m, cow.basePath, cow.overlayPath, 1024 * 1024) : anyFn.call(m, cow.basePath, cow.overlayPath);
    await maybeAwait(res);
    changed = true;
  }

  if (msg.cd) {
    const plan = planMachineBootDiskAttachment(msg.cd, "cd");
    const isoPath = plan.opfsPath;

    const attachIso =
      (m as unknown as { attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref?: unknown })
        .attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref ??
      (m as unknown as { attach_ide_secondary_master_iso_opfs_existing?: unknown }).attach_ide_secondary_master_iso_opfs_existing ??
      (m as unknown as { attach_install_media_iso_opfs_and_set_overlay_ref?: unknown }).attach_install_media_iso_opfs_and_set_overlay_ref ??
      (m as unknown as { attach_install_media_iso_opfs?: unknown }).attach_install_media_iso_opfs ??
      (m as unknown as { attach_install_media_opfs_iso?: unknown }).attach_install_media_opfs_iso;

    if (typeof attachIso !== "function") {
      throw new Error("Machine install-media ISO OPFS attach export is unavailable in this WASM build.");
    }

    await maybeAwait((attachIso as (path: string) => unknown).call(m, isoPath));

    // Best-effort overlay ref: some attach APIs do not set DISKS refs; try to do it here when available.
    try {
      const setRef = (m as unknown as { set_ide_secondary_master_atapi_overlay_ref?: unknown }).set_ide_secondary_master_atapi_overlay_ref;
      if (typeof setRef === "function") {
        (setRef as (base: string, overlay: string) => void).call(m, isoPath, "");
      }
    } catch {
      // ignore
    }

    changed = true;
  }

  if (changed) {
    try {
      m.reset();
    } catch {
      // ignore
    }
  }
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

  pushEvent({ kind: "serialOutput", port: UART_COM1.basePort, data: bytes });
}

function handleRunExit(exit: unknown): void {
  const kind = (exit as unknown as { kind?: unknown }).kind;
  const detail = (exit as unknown as { detail?: unknown }).detail;
  const kindNum = typeof kind === "number" ? (kind | 0) : -1;
  const detailStr = typeof detail === "string" ? detail : "";

  if (kindNum === 2) {
    pushEventBlocking({ kind: "resetRequest" }, 250);
    running = false;
    return;
  }

  if (kindNum === 5) {
    if (/triplefault/i.test(detailStr)) {
      pushEventBlocking({ kind: "tripleFault" }, 250);
    } else {
      pushEventBlocking({ kind: "panic", message: `CPU exit: ${detailStr || "unknown"}` }, 250);
    }
    running = false;
    return;
  }

  if (kindNum === 4) {
    pushEventBlocking({ kind: "panic", message: `Exception: ${detailStr || "unknown"}` }, 250);
    running = false;
    return;
  }
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

  machineBusy = true;
  detachMachineNetwork();

  try {
    if (op.kind === "vm.machine.saveToOpfs") {
      if (!vmSnapshotPaused) {
        throw new Error("VM is not paused; call vm.snapshot.pause before saving.");
      }
      const fn =
        (m as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs ??
        (m as unknown as { snapshot_dirty_to_opfs?: unknown }).snapshot_dirty_to_opfs;
      if (typeof fn !== "function") {
        throw new Error("Machine.snapshot_full_to_opfs(path) is unavailable in this WASM build.");
      }
      await maybeAwait((fn as (path: string) => unknown).call(m, op.path));
      postVmSnapshot({ kind: "vm.snapshot.machine.saved", requestId: op.requestId, ok: true } satisfies VmSnapshotMachineSavedMessage);
      return;
    }

    if (op.kind === "vm.machine.restoreFromOpfs") {
      if (!vmSnapshotPaused) {
        throw new Error("VM is not paused; call vm.snapshot.pause before restoring.");
      }
      await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine: m, path: op.path, logPrefix: "machine_cpu.worker" });
      postVmSnapshot({ kind: "vm.snapshot.machine.restored", requestId: op.requestId, ok: true } satisfies VmSnapshotMachineRestoredMessage);
      return;
    }

    if (op.kind === "machine.restoreFromOpfs") {
      await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine: m, path: op.path, logPrefix: "machine_cpu.worker" });
      postSnapshot({ kind: "machine.snapshot.restored", requestId: op.requestId, ok: true });
      return;
    }

    await restoreMachineSnapshotAndReattachDisks({ api, machine: m, bytes: op.bytes, logPrefix: "machine_cpu.worker" });
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
    machineBusy = false;
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
      if (pendingBootDisks && machine && !machineBusy) {
        const msg = pendingBootDisks;
        pendingBootDisks = null;
        machineBusy = true;
        try {
          await applyBootDisks(msg);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          pushEvent({ kind: "log", level: "warn", message: `[machine_cpu] setBootDisks failed: ${message}` });
        } finally {
          machineBusy = false;
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

      if (!running || !machine || vmSnapshotPaused || machineBusy) {
        await ring.waitForDataAsync(HEARTBEAT_INTERVAL_MS);
        continue;
      }

      const exit = machine.run_slice(RUN_SLICE_MAX_INSTS);
      handleRunExit(exit);
      drainSerialOutput();
      try {
        (exit as unknown as { free?: () => void }).free?.();
      } catch {
        // ignore
      }

      await new Promise((resolve) => {
        const timer = setTimeout(resolve, 0);
        (timer as unknown as { unref?: () => void }).unref?.();
      });
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

    // Attach optional network backend if enabled.
    if (networkWanted) {
      attachMachineNetwork();
    }

    try {
      machine.reset();
    } catch {
      // ignore
    }
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

  const input = msg as Partial<InputBatchMessage | InputBatchRecycleMessage>;
  if (input?.type === "in:input-batch") {
    const buffer = input.buffer;
    if (!(buffer instanceof ArrayBuffer)) return;
    const recycle = input.recycle === true;

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
        postVmSnapshot({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
        return;
      }
      case "vm.snapshot.resume": {
        vmSnapshotPaused = false;
        postVmSnapshot({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
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
      post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
      return;
    }

    pendingBootDisks = bootDisks;
    return;
  }

  if ((msg as { kind?: unknown }).kind === "config.update") {
    const update = msg as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    networkWanted = isNetworkingEnabled(currentConfig);
    post({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind === "init") {
    void initAndRun(init as WorkerInitMessage);
  }
};
