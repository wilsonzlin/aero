/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
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

let wasmApi: WasmApi | null = null;
let wasmMachine: InstanceType<WasmApi["Machine"]> | null = null;
let snapshotOpChain: Promise<void> = Promise.resolve();

function post(msg: ProtocolMessage | ConfigAckMessage): void {
  ctx.postMessage(msg);
}

function postSnapshot(msg: MachineSnapshotRestoredMessage): void {
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

ctx.onmessage = (ev) => {
  const msg = ev.data as unknown;

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
    };

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
  // `initWasmForContext` (via `wasm_loader.ts`) relies on Vite-only `import.meta.glob`.
  // When this worker is executed directly under Node (e.g. in worker_threads tests),
  // `import.meta.glob` is not defined, so skip WASM init entirely.
  if (typeof (import.meta as unknown as { glob?: unknown }).glob !== "function") return;

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
