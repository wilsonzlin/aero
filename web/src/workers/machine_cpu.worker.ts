/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { InputEventType } from "../input/event_queue";
import {
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  StatusIndex,
  type SharedMemorySegments,
  type WorkerRole,
} from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import type { WasmApi } from "../runtime/wasm_loader";
import {
  restoreMachineSnapshotAndReattachDisks,
  restoreMachineSnapshotFromOpfsAndReattachDisks,
} from "../runtime/machine_snapshot_disks";
import { normalizeSetBootDisksMessage, type SetBootDisksMessage } from "../runtime/boot_disks_protocol";
import { INPUT_BATCH_HEADER_WORDS, INPUT_BATCH_WORDS_PER_EVENT, validateInputBatchBuffer } from "./io_input_batch";

/**
 * Minimal "machine CPU" worker entrypoint.
 *
 * This worker participates in the coordinator's standard `config.update` + `init` protocol and
 * must be robust in environments where WASM builds are unavailable (e.g. CI runs with `--skip-wasm`).
 *
 * NOTE: The current implementation only validates bootstrap wiring and does not yet drive a
 * full-system VM loop; it exists so the worker lifecycle and init contract can be tested via
 * `node:worker_threads` without depending on a built WASM module.
 */

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: WorkerRole = "cpu";
let status: Int32Array | null = null;
let commandRing: RingBuffer | null = null;
let eventRing: RingBuffer | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

// Boot disk selection (shared protocol with the legacy IO worker).
// The machine CPU worker does not yet use this to attach disks, but it accepts the
// message so coordinators/harnesses can share a single schema.
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

type MachineSnapshotSerializedError = { name: string; message: string; stack?: string };
type MachineSnapshotResultOk = { ok: true };
type MachineSnapshotResultErr = { ok: false; error: MachineSnapshotSerializedError };

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

type VmSnapshotPauseMessage = {
  kind: "vm.snapshot.pause";
  requestId: number;
};

type VmSnapshotResumeMessage = {
  kind: "vm.snapshot.resume";
  requestId: number;
};

type VmSnapshotSaveToOpfsMessage = {
  kind: "vm.snapshot.saveToOpfs";
  requestId: number;
  path: string;
};

type VmSnapshotRestoreFromOpfsMessage = {
  kind: "vm.snapshot.restoreFromOpfs";
  requestId: number;
  path: string;
};

type VmSnapshotPausedMessage = { kind: "vm.snapshot.paused"; requestId: number } & (MachineSnapshotResultOk | MachineSnapshotResultErr);
type VmSnapshotResumedMessage = { kind: "vm.snapshot.resumed"; requestId: number } & (MachineSnapshotResultOk | MachineSnapshotResultErr);
type VmSnapshotSavedMessage = { kind: "vm.snapshot.saved"; requestId: number } & (MachineSnapshotResultOk | MachineSnapshotResultErr);
type VmSnapshotRestoredMessage =
  | ({ kind: "vm.snapshot.restored"; requestId: number } & MachineSnapshotResultErr)
  | { kind: "vm.snapshot.restored"; requestId: number; ok: true; cpu: ArrayBuffer; mmu: ArrayBuffer; devices?: unknown[] };

let wasmApi: WasmApi | null = null;
let wasmMachine: InstanceType<WasmApi["Machine"]> | null = null;
let snapshotOpChain: Promise<void> = Promise.resolve();
let vmSnapshotPaused = false;

function post(msg: ProtocolMessage | ConfigAckMessage): void {
  ctx.postMessage(msg);
}

function postSnapshot(msg: MachineSnapshotRestoredMessage): void {
  ctx.postMessage(msg);
}

function postVmSnapshot(msg: VmSnapshotPausedMessage | VmSnapshotResumedMessage | VmSnapshotSavedMessage | VmSnapshotRestoredMessage): void {
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

function serializeError(err: unknown): MachineSnapshotSerializedError {
  if (err instanceof Error) return { name: err.name || "Error", message: err.message, stack: err.stack };
  return { name: "Error", message: String(err) };
}

function getMachineRamSizeBytes(): number {
  const mib = currentConfig?.guestMemoryMiB;
  if (typeof mib === "number" && Number.isFinite(mib) && mib > 0) {
    const bytes = mib * 1024 * 1024;
    if (Number.isFinite(bytes) && bytes > 0) return bytes >>> 0;
  }
  // Fallback for tests/harnesses that have not yet wired `config.update` -> `Machine` sizing.
  return 1 * 1024 * 1024;
}

function ensureWasmMachine(): { api: WasmApi; machine: InstanceType<WasmApi["Machine"]> } {
  const api = wasmApi;
  if (!api) throw new Error("WASM is not initialized; cannot restore machine snapshot.");
  if (!api.Machine) throw new Error("Machine export unavailable in this WASM build.");
  if (!wasmMachine) {
    wasmMachine = new api.Machine(getMachineRamSizeBytes());
  }
  return { api, machine: wasmMachine };
}

function enqueueSnapshotOp(op: () => Promise<void>): void {
  snapshotOpChain = snapshotOpChain.then(op).catch(() => undefined);
}

function postInputBatchRecycle(buffer: ArrayBuffer): void {
  // `buffer` should be transferable in normal InputCapture usage. Avoid crashing the worker if the
  // buffer cannot be transferred for some reason; fall back to a structured clone.
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

function handleInputBatch(buffer: ArrayBuffer): void {
  const machine = wasmMachine;
  if (!machine) {
    // CI `--skip-wasm` or early startup: nothing to inject into.
    return;
  }

  const decoded = validateInputBatchBuffer(buffer);
  if (!decoded.ok) {
    // Drop malformed input batches; keep the worker alive.
    return;
  }

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
        if (typeof machine.inject_key_scancode_bytes === "function") {
          machine.inject_key_scancode_bytes(packed, len);
        } else if (typeof machine.inject_keyboard_bytes === "function") {
          const bytes = new Uint8Array(len);
          for (let j = 0; j < len; j++) bytes[j] = (packed >>> (j * 8)) & 0xff;
          machine.inject_keyboard_bytes(bytes);
        }
      } catch {
        // ignore
      }
    } else if (type === InputEventType.MouseMove) {
      const dx = words[off + 2] | 0;
      const dyPs2 = words[off + 3] | 0;
      try {
        if (typeof machine.inject_ps2_mouse_motion === "function") {
          machine.inject_ps2_mouse_motion(dx, dyPs2, 0);
        } else if (typeof machine.inject_mouse_motion === "function") {
          // Machine expects browser-style coordinates (+Y down).
          machine.inject_mouse_motion(dx, -dyPs2, 0);
        }
      } catch {
        // ignore
      }
    } else if (type === InputEventType.MouseWheel) {
      const dz = words[off + 2] | 0;
      if (dz === 0) continue;
      try {
        if (typeof machine.inject_ps2_mouse_motion === "function") {
          machine.inject_ps2_mouse_motion(0, 0, dz);
        } else if (typeof machine.inject_mouse_motion === "function") {
          machine.inject_mouse_motion(0, 0, dz);
        }
      } catch {
        // ignore
      }
    } else if (type === InputEventType.MouseButtons) {
      // DOM `MouseEvent.buttons` bitfield:
      // - bit0 left, bit1 right, bit2 middle, bit3 back, bit4 forward.
      //
      // The canonical Machine PS/2 mouse model can surface back/forward (IntelliMouse
      // Explorer extensions) when the guest enables it, so preserve the low 5 bits.
      const buttons = words[off + 2] & 0xff;
      const mask = buttons & 0x1f;
      try {
        if (typeof machine.inject_mouse_buttons_mask === "function") {
          machine.inject_mouse_buttons_mask(mask);
        } else if (typeof machine.inject_ps2_mouse_buttons === "function") {
          machine.inject_ps2_mouse_buttons(mask);
        }
      } catch {
        // ignore
      }
    }
  }
}

ctx.onmessage = (ev) => {
  const msg = ev.data as unknown;

  const input = msg as Partial<InputBatchMessage | InputBatchRecycleMessage>;
  if (input?.type === "in:input-batch") {
    const buffer = input.buffer;
    if (!(buffer instanceof ArrayBuffer)) return;
    handleInputBatch(buffer);
    if (input.recycle === true) {
      postInputBatchRecycle(buffer);
    }
    return;
  }
  // `in:input-batch-recycle` is normally sent from this worker back to the input producer, but
  // accept it as a no-op so callers can proxy recycle messages through multiple hops if needed.
  if (input?.type === "in:input-batch-recycle") {
    return;
  }

  const vmSnapshot = msg as Partial<
    VmSnapshotPauseMessage | VmSnapshotResumeMessage | VmSnapshotSaveToOpfsMessage | VmSnapshotRestoreFromOpfsMessage
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
      case "vm.snapshot.saveToOpfs": {
        const path = typeof (vmSnapshot as Partial<VmSnapshotSaveToOpfsMessage>).path === "string" ? vmSnapshot.path : "";
        if (!path) {
          postVmSnapshot({
            kind: "vm.snapshot.saved",
            requestId,
            ok: false,
            error: serializeError(new Error("vm.snapshot.saveToOpfs requires a non-empty path.")),
          } satisfies VmSnapshotSavedMessage);
          return;
        }

        enqueueSnapshotOp(async () => {
          try {
            const { machine } = ensureWasmMachine();
            const fn = (machine as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs;
            if (typeof fn !== "function") {
              throw new Error("Machine.snapshot_full_to_opfs(path) is unavailable in this WASM build.");
            }
            await Promise.resolve((fn as (path: string) => unknown).call(machine, path));
            postVmSnapshot({ kind: "vm.snapshot.saved", requestId, ok: true } satisfies VmSnapshotSavedMessage);
          } catch (err) {
            postVmSnapshot({ kind: "vm.snapshot.saved", requestId, ok: false, error: serializeError(err) } satisfies VmSnapshotSavedMessage);
          }
        });
        return;
      }
      case "vm.snapshot.restoreFromOpfs": {
        const path =
          typeof (vmSnapshot as Partial<VmSnapshotRestoreFromOpfsMessage>).path === "string" ? vmSnapshot.path : "";
        if (!path) {
          postVmSnapshot({
            kind: "vm.snapshot.restored",
            requestId,
            ok: false,
            error: serializeError(new Error("vm.snapshot.restoreFromOpfs requires a non-empty path.")),
          } satisfies VmSnapshotRestoredMessage);
          return;
        }

        enqueueSnapshotOp(async () => {
          try {
            const { api, machine } = ensureWasmMachine();
            // Snapshot restore intentionally drops host-side disk backends (OPFS handles) and only
            // preserves overlay refs as *OPFS path strings* (relative to `navigator.storage.getDirectory()`).
            //
            // After restoring, re-open those OPFS images and reattach them to the canonical machine.
            await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path, logPrefix: "machine_cpu.worker" });
            postVmSnapshot({
              kind: "vm.snapshot.restored",
              requestId,
              ok: true,
              // Machine runtime owns full snapshot restore; coordinator should ignore cpu/mmu fields.
              cpu: new ArrayBuffer(0),
              mmu: new ArrayBuffer(0),
            } satisfies VmSnapshotRestoredMessage);
          } catch (err) {
            postVmSnapshot({
              kind: "vm.snapshot.restored",
              requestId,
              ok: false,
              error: serializeError(err),
            } satisfies VmSnapshotRestoredMessage);
          }
        });
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

    enqueueSnapshotOp(async () => {
      try {
        const { api, machine } = ensureWasmMachine();
        // Snapshot restore intentionally drops host-side disk backends (OPFS handles) and only
        // preserves overlay refs as *OPFS path strings* (relative to `navigator.storage.getDirectory()`).
        //
        // After restoring, re-open those OPFS images and reattach them to the canonical machine.
        await restoreMachineSnapshotFromOpfsAndReattachDisks({ api, machine, path, logPrefix: "machine_cpu.worker" });
        postSnapshot({ kind: "machine.snapshot.restored", requestId, ok: true });
      } catch (err) {
        postSnapshot({ kind: "machine.snapshot.restored", requestId, ok: false, error: serializeError(err) });
      }
    });
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

    enqueueSnapshotOp(async () => {
      try {
        const { api, machine } = ensureWasmMachine();
        await restoreMachineSnapshotAndReattachDisks({
          api,
          machine,
          bytes: new Uint8Array(bytes),
          logPrefix: "machine_cpu.worker",
        });
        postSnapshot({ kind: "machine.snapshot.restored", requestId, ok: true });
      } catch (err) {
        postSnapshot({ kind: "machine.snapshot.restored", requestId, ok: false, error: serializeError(err) });
      }
    });
    return;
  }

  const bootDisks = normalizeSetBootDisksMessage(msg);
  if (bootDisks) {
    pendingBootDisks = bootDisks;
    return;
  }

  if ((msg as { kind?: unknown }).kind === "config.update") {
    const update = msg as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    post({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind === "init") {
    void initAndRun(init as WorkerInitMessage);
  }
};

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

    const regions = ringRegionsForWorker(role);
    commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
    eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

    // Emit READY immediately; WASM initialization is best-effort and should not prevent the
    // worker from participating in the coordinator lifecycle (mirrors cpu.worker.ts behavior).
    setReadyFlag(status, role, true);
    post({ type: MessageType.READY, role } satisfies ProtocolMessage);

    // Kick off WASM init in the background. It may fail when the wasm-pack output is absent
    // (e.g. CI runs with `--skip-wasm`); keep the worker alive regardless.
    void initWasmInBackground(init, init.guestMemory);

    void runLoop();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (status) setReadyFlag(status, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  }
}

async function runLoop(): Promise<void> {
  const ring = commandRing;
  const st = status;
  if (!ring || !st) return;

  try {
    while (Atomics.load(st, StatusIndex.StopRequested) !== 1) {
      // Drain all pending commands.
      while (true) {
        const payload = ring.tryPop();
        if (!payload) break;
        let cmd: Command;
        try {
          cmd = decodeCommand(payload);
        } catch {
          // Corrupt or unknown command; ignore.
          continue;
        }

        switch (cmd.kind) {
          case "nop":
            pushEvent({ kind: "ack", seq: cmd.seq } satisfies Event);
            break;
          case "shutdown":
            Atomics.store(st, StatusIndex.StopRequested, 1);
            break;
          default:
            // Ignore other commands for now; the machine CPU worker currently only exists to
            // validate worker lifecycle wiring under Node worker_threads.
            break;
        }
      }

      await ring.waitForDataAsync(250);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEvent({ kind: "panic", message } satisfies Event);
    setReadyFlag(st, role, false);
    post({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
  } finally {
    setReadyFlag(st, role, false);
    if (wasmMachine) {
      try {
        (wasmMachine as unknown as { free?: () => void }).free?.();
      } catch {
        // ignore
      }
      wasmMachine = null;
    }
  }
}

async function initWasmInBackground(init: WorkerInitMessage, guestMemory: WebAssembly.Memory): Promise<void> {
  // This worker is used primarily for worker_threads lifecycle tests. Those tests execute the
  // TypeScript sources directly under Node (no Vite transforms, and usually without the
  // wasm-pack output present). Even though `wasm_loader.ts` has a Node-safe fallback for
  // `import.meta.glob`, initializing WASM here would still either:
  // - fail with a "Missing WASM package" error (no generated output), or
  // - introduce unnecessary work/noise into otherwise lightweight tests.
  //
  // In real browser/Vite builds, `process` is typically undefined (or lacks `versions.node`),
  // so WASM init proceeds.
  const isNode = typeof process !== "undefined" && typeof process.versions?.node === "string";
  if (isNode) return;

  try {
    const { initWasmForContext } = await import("../runtime/wasm_context");
    const { api, variant } = await initWasmForContext({
      variant: init.wasmVariant,
      module: init.wasmModule,
      memory: guestMemory,
    });
    wasmApi = api;
    const value = typeof api.add === "function" ? api.add(20, 22) : 0;
    const st = status;
    if (st && Atomics.load(st, StatusIndex.StopRequested) === 1) return;
    post({ type: MessageType.WASM_READY, role, variant, value } satisfies ProtocolMessage);
  } catch (err) {
    // Best-effort; do not crash on missing wasm assets.
    // Use a guarded log to avoid throwing in environments without console.
    try {
      // eslint-disable-next-line no-console
      console.warn("[machine_cpu.worker] WASM init failed (continuing without WASM):", err);
    } catch {
      // ignore
    }
  }
}
