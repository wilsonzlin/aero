/// <reference lib="webworker" />

import { initWasmForContext, type WasmApi, type WasmVariant } from "../runtime/wasm_context";
import { serializeErrorForProtocol } from "../errors/serialize";
import { unrefBestEffort } from "../unrefSafe";
import type {
  DemoVmWorkerInitResult,
  DemoVmWorkerMessage,
  DemoVmWorkerRequest,
  DemoVmWorkerRpcResultErr,
  DemoVmWorkerSerializedError,
  DemoVmWorkerSerialOutputLenResult,
  DemoVmWorkerSerialStatsResult,
  DemoVmWorkerStepResult,
} from "./demo_vm_worker_protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let api: WasmApi | null = null;
let variant: WasmVariant | null = null;
let vm: InstanceType<WasmApi["DemoVm"]> | null = null;
let shouldClose = false;

let stepsTotal = 0;
let serialBytes: number | null = 0;
const savedSerialBytesByPath = new Map<string, number | null>();

let stepTimer: number | null = null;
let stepLoopPaused = false;
const STEPS_PER_TICK = 5_000;
const TICK_MS = 250;

// Serialize all commands that touch the VM so that async snapshot/restore calls
// cannot overlap (e.g. autosave timer firing while the user hits "Load").
let commandChain: Promise<void> = Promise.resolve();

function getSerialOutputLenFromVm(current: InstanceType<WasmApi["DemoVm"]>): number | null {
  const anyVm = current as unknown as Record<string, unknown>;
  const fn = anyVm.serial_output_len ?? anyVm.serialOutputLen;
  if (typeof fn !== "function") return null;
  try {
    const value = (fn as () => unknown).call(current);
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
    return value;
  } catch {
    return null;
  }
}

function post(msg: DemoVmWorkerMessage): void {
  ctx.postMessage(msg);
}

function postError(err: unknown): void {
  post({ type: "error", error: serializeErrorForProtocol(err) });
}

function ensureVm(): InstanceType<WasmApi["DemoVm"]> {
  if (!vm) throw new Error("DemoVm is not initialized. Call init first.");
  return vm;
}

function stopStepLoop(): void {
  stepLoopPaused = true;
  if (stepTimer !== null) {
    ctx.clearInterval(stepTimer);
    stepTimer = null;
  }
}

function startStepLoop(): void {
  stopStepLoop();
  stepLoopPaused = false;
  const timer = ctx.setInterval(() => {
    try {
      if (stepLoopPaused) return;
      const current = ensureVm();
      const anyVm = current as unknown as Record<string, unknown>;
      const runSteps = anyVm.run_steps ?? anyVm.runSteps;
      if (typeof runSteps !== "function") throw new Error("DemoVm missing run_steps/runSteps export.");
      (runSteps as (steps: number) => void).call(current, STEPS_PER_TICK);
      const maybeLen = getSerialOutputLenFromVm(current);
      if (maybeLen !== null) {
        // Demo VM writes one serial byte per step; treat serial length as a proxy
        // for total steps so restored snapshots report meaningful counters.
        stepsTotal = maybeLen;
        serialBytes = maybeLen;
      } else {
        stepsTotal += STEPS_PER_TICK;
        if (serialBytes !== null) serialBytes += STEPS_PER_TICK;
      }
      post({ type: "status", steps: stepsTotal, serialBytes });
    } catch (err) {
      postError(err);
      // If we fail while stepping, stop the loop so we don't spam errors.
      stopStepLoop();
    }
  }, TICK_MS);
  unrefBestEffort(timer);
  stepTimer = timer;
}

async function handleInit(ramBytes: number): Promise<DemoVmWorkerInitResult> {
  stopStepLoop();
  if (vm) {
    vm.free();
    vm = null;
  }

  const init = await initWasmForContext();
  const wasmApi = init.api;
  const wasmVariant = init.variant;
  api = wasmApi;
  variant = wasmVariant;

  const newVm = new wasmApi.DemoVm(ramBytes);
  vm = newVm;
  stepsTotal = 0;
  serialBytes = 0;
  savedSerialBytesByPath.clear();

  const fileHandleCtor = (globalThis as unknown as { FileSystemFileHandle?: unknown }).FileSystemFileHandle;
  const fileHandleProto = (fileHandleCtor as { prototype?: unknown } | undefined)?.prototype;
  const createSyncAccessHandle = (fileHandleProto as { createSyncAccessHandle?: unknown } | undefined)?.createSyncAccessHandle;
  const syncAccessHandles = typeof createSyncAccessHandle === "function";

  const streamingSnapshots =
    typeof (newVm as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs === "function" ||
    typeof (newVm as unknown as { snapshotFullToOpfs?: unknown }).snapshotFullToOpfs === "function";
  const streamingDirtySnapshots =
    typeof (newVm as unknown as { snapshot_dirty_to_opfs?: unknown }).snapshot_dirty_to_opfs === "function" ||
    typeof (newVm as unknown as { snapshotDirtyToOpfs?: unknown }).snapshotDirtyToOpfs === "function";
  const streamingRestore =
    typeof (newVm as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs === "function" ||
    typeof (newVm as unknown as { restoreSnapshotFromOpfs?: unknown }).restoreSnapshotFromOpfs === "function";

  const initialLen = getSerialOutputLenFromVm(newVm);
  if (initialLen !== null) {
    stepsTotal = initialLen;
    serialBytes = initialLen;
  }

  startStepLoop();

  return { wasmVariant: wasmVariant, syncAccessHandles, streamingSnapshots, streamingDirtySnapshots, streamingRestore };
}

async function handleRunSteps(steps: number): Promise<DemoVmWorkerStepResult> {
  const current = ensureVm();
  const anyVm = current as unknown as Record<string, unknown>;
  const runSteps = anyVm.run_steps ?? anyVm.runSteps;
  if (typeof runSteps !== "function") throw new Error("DemoVm missing run_steps/runSteps export.");
  (runSteps as (steps: number) => void).call(current, steps);
  const maybeLen = getSerialOutputLenFromVm(current);
  if (maybeLen !== null) {
    stepsTotal = maybeLen;
    serialBytes = maybeLen;
  } else {
    stepsTotal += steps;
    if (serialBytes !== null) serialBytes += steps;
  }
  const state = { steps: stepsTotal, serialBytes };
  post({ type: "status", ...state });
  return state;
}

async function handleSnapshotFullToOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
  const current = ensureVm();

  const fn =
    (current as unknown as { snapshot_full_to_opfs?: (path: string) => unknown }).snapshot_full_to_opfs ??
    (current as unknown as { snapshotFullToOpfs?: (path: string) => unknown }).snapshotFullToOpfs;
  if (typeof fn !== "function") {
    throw new Error(
      "DemoVm snapshotFullToOpfs export is unavailable (WASM build missing streaming snapshot exports).",
    );
  }

  stopStepLoop();
  let snapshotSerialBytes: number | null = serialBytes;
  try {
    await fn.call(current, path);
    snapshotSerialBytes = serialBytes;
    savedSerialBytesByPath.set(path, snapshotSerialBytes);
  } finally {
    startStepLoop();
  }

  return { serialBytes: snapshotSerialBytes };
}

async function handleSnapshotDirtyToOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
  const current = ensureVm();

  const fn =
    (current as unknown as { snapshot_dirty_to_opfs?: (path: string) => unknown }).snapshot_dirty_to_opfs ??
    (current as unknown as { snapshotDirtyToOpfs?: (path: string) => unknown }).snapshotDirtyToOpfs;
  if (typeof fn !== "function") {
    throw new Error(
      "DemoVm snapshotDirtyToOpfs export is unavailable (WASM build missing streaming dirty snapshot exports).",
    );
  }

  stopStepLoop();
  let snapshotSerialBytes: number | null = serialBytes;
  try {
    await fn.call(current, path);
    snapshotSerialBytes = serialBytes;
    savedSerialBytesByPath.set(path, snapshotSerialBytes);
  } finally {
    startStepLoop();
  }

  return { serialBytes: snapshotSerialBytes };
}

async function handleRestoreFromOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
  const current = ensureVm();

  const fn =
    (current as unknown as { restore_snapshot_from_opfs?: (path: string) => unknown }).restore_snapshot_from_opfs ??
    (current as unknown as { restoreSnapshotFromOpfs?: (path: string) => unknown }).restoreSnapshotFromOpfs;
  if (typeof fn !== "function") {
    throw new Error("DemoVm restoreSnapshotFromOpfs export is unavailable (WASM build missing streaming restore exports).");
  }

  stopStepLoop();
  try {
    await fn.call(current, path);
    const maybeLen = getSerialOutputLenFromVm(current);
    if (maybeLen !== null) {
      stepsTotal = maybeLen;
      serialBytes = maybeLen;
    } else {
      const saved = savedSerialBytesByPath.get(path);
      serialBytes = saved ?? null;
      if (typeof saved === "number") {
        stepsTotal = saved;
      }
    }
  } finally {
    startStepLoop();
  }

  const state = { serialBytes };
  post({ type: "status", steps: stepsTotal, serialBytes });
  return state;
}

async function handleGetSerialOutputLen(): Promise<DemoVmWorkerSerialOutputLenResult> {
  return { serialBytes };
}

async function handleGetSerialStats(): Promise<DemoVmWorkerSerialStatsResult> {
  return { steps: stepsTotal, serialBytes };
}

async function handleShutdown(): Promise<void> {
  stopStepLoop();
  if (vm) {
    vm.free();
    vm = null;
  }
  api = null;
  variant = null;
  shouldClose = true;
}

async function handleRequest(req: DemoVmWorkerRequest): Promise<unknown> {
  switch (req.type) {
    case "init":
      return await handleInit(req.ramBytes);
    case "runSteps":
      return await handleRunSteps(req.steps);
    case "snapshotFullToOpfs":
      return await handleSnapshotFullToOpfs(req.path);
    case "snapshotDirtyToOpfs":
      return await handleSnapshotDirtyToOpfs(req.path);
    case "restoreFromOpfs":
      return await handleRestoreFromOpfs(req.path);
    case "getSerialStats":
      return await handleGetSerialStats();
    case "getSerialOutputLen":
      return await handleGetSerialOutputLen();
    case "shutdown":
      return await handleShutdown();
    default: {
      const exhaustive: never = req;
      throw new Error(`Unknown request type: ${(exhaustive as { type?: unknown }).type}`);
    }
  }
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const req = ev.data as DemoVmWorkerRequest;
  if (!req || typeof req !== "object" || typeof (req as { type?: unknown }).type !== "string") {
    postError(new Error("Invalid message received by demo VM snapshot worker."));
    return;
  }

  commandChain = commandChain
    .then(async () => {
      try {
        const result = await handleRequest(req);
        post({ type: "rpcResult", id: req.id, ok: true, result } satisfies DemoVmWorkerMessage);
      } catch (err) {
        const serialized: DemoVmWorkerSerializedError = serializeErrorForProtocol(err);
        post({ type: "rpcResult", id: req.id, ok: false, error: serialized } satisfies DemoVmWorkerRpcResultErr);
        post({ type: "error", error: serialized } satisfies DemoVmWorkerMessage);
      } finally {
        if (shouldClose) {
          ctx.close();
        }
      }
    })
    .catch((err) => {
      // Defensive: this should be unreachable because we catch per-command errors
      // above, but keep the chain alive if something unexpected slips through.
      postError(err);
    });
};
