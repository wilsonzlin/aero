/// <reference lib="webworker" />

import { initWasmForContext, type WasmApi, type WasmVariant } from "../runtime/wasm_context";

type InitRequest = { id: number; type: "init"; ramBytes: number };
type RunStepsRequest = { id: number; type: "runSteps"; steps: number };
type SnapshotFullToOpfsRequest = { id: number; type: "snapshotFullToOpfs"; path: string };
type RestoreFromOpfsRequest = { id: number; type: "restoreFromOpfs"; path: string };
type GetSerialStatsRequest = { id: number; type: "getSerialStats" };
type GetSerialOutputLenRequest = { id: number; type: "getSerialOutputLen" };
type ShutdownRequest = { id: number; type: "shutdown" };

type WorkerRequest =
  | InitRequest
  | RunStepsRequest
  | SnapshotFullToOpfsRequest
  | RestoreFromOpfsRequest
  | GetSerialStatsRequest
  | GetSerialOutputLenRequest
  | ShutdownRequest;

type RpcResultOk<T> = { type: "rpcResult"; id: number; ok: true; result: T };
type RpcResultErr = { type: "rpcResult"; id: number; ok: false; error: string };
type RpcResult<T> = RpcResultOk<T> | RpcResultErr;

type StatusUpdateMessage = { type: "status"; steps: number; serialBytes: number | null };
type ErrorMessage = { type: "error"; message: string };

type WorkerToMainMessage =
  | RpcResult<unknown>
  | StatusUpdateMessage
  | ErrorMessage;

type InitResult = {
  wasmVariant: WasmVariant;
  syncAccessHandles: boolean;
  streamingSnapshots: boolean;
  streamingRestore: boolean;
};

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
  const fn = (current as unknown as { serial_output_len?: () => number }).serial_output_len;
  if (typeof fn !== "function") return null;
  try {
    const value = fn.call(current);
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
    return value;
  } catch {
    return null;
  }
}

function post(msg: WorkerToMainMessage): void {
  ctx.postMessage(msg);
}

function postError(message: string): void {
  post({ type: "error", message });
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
  stepTimer = ctx.setInterval(() => {
    try {
      if (stepLoopPaused) return;
      const current = ensureVm();
      current.run_steps(STEPS_PER_TICK);
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
      postError(err instanceof Error ? err.message : String(err));
      // If we fail while stepping, stop the loop so we don't spam errors.
      stopStepLoop();
    }
  }, TICK_MS);
}

async function handleInit(ramBytes: number): Promise<InitResult> {
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

  const syncAccessHandles =
    typeof (globalThis as unknown as { FileSystemFileHandle?: unknown }).FileSystemFileHandle !== "undefined" &&
    typeof (
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).FileSystemFileHandle?.prototype?.createSyncAccessHandle
    ) === "function";

  const streamingSnapshots =
    typeof (newVm as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs === "function";
  const streamingRestore =
    typeof (newVm as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs === "function";

  const initialLen = getSerialOutputLenFromVm(newVm);
  if (initialLen !== null) {
    stepsTotal = initialLen;
    serialBytes = initialLen;
  }

  startStepLoop();

  return { wasmVariant: wasmVariant, syncAccessHandles, streamingSnapshots, streamingRestore };
}

async function handleRunSteps(steps: number): Promise<{ steps: number; serialBytes: number | null }> {
  const current = ensureVm();
  current.run_steps(steps);
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

async function handleSnapshotFullToOpfs(path: string): Promise<void> {
  const current = ensureVm();

  const fn = (current as unknown as { snapshot_full_to_opfs?: (path: string) => unknown }).snapshot_full_to_opfs;
  if (typeof fn !== "function") {
    throw new Error("DemoVm.snapshot_full_to_opfs is unavailable (WASM build missing streaming snapshot exports).");
  }

  stopStepLoop();
  try {
    await fn.call(current, path);
    savedSerialBytesByPath.set(path, serialBytes);
  } finally {
    startStepLoop();
  }
}

async function handleRestoreFromOpfs(path: string): Promise<{ serialBytes: number | null }> {
  const current = ensureVm();

  const fn = (current as unknown as { restore_snapshot_from_opfs?: (path: string) => unknown }).restore_snapshot_from_opfs;
  if (typeof fn !== "function") {
    throw new Error("DemoVm.restore_snapshot_from_opfs is unavailable (WASM build missing streaming restore exports).");
  }

  stopStepLoop();
  try {
    await fn.call(current, path);
    const maybeLen = getSerialOutputLenFromVm(current);
    if (maybeLen !== null) {
      stepsTotal = maybeLen;
      serialBytes = maybeLen;
    } else {
      serialBytes = savedSerialBytesByPath.get(path) ?? null;
    }
  } finally {
    startStepLoop();
  }

  const state = { serialBytes };
  post({ type: "status", steps: stepsTotal, serialBytes });
  return state;
}

async function handleGetSerialOutputLen(): Promise<{ serialBytes: number | null }> {
  return { serialBytes };
}

async function handleGetSerialStats(): Promise<{ steps: number; serialBytes: number | null }> {
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

async function handleRequest(req: WorkerRequest): Promise<unknown> {
  switch (req.type) {
    case "init":
      return await handleInit(req.ramBytes);
    case "runSteps":
      return await handleRunSteps(req.steps);
    case "snapshotFullToOpfs":
      return await handleSnapshotFullToOpfs(req.path);
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
  const req = ev.data as WorkerRequest;
  if (!req || typeof req !== "object" || typeof (req as { type?: unknown }).type !== "string") {
    postError("Invalid message received by demo VM snapshot worker.");
    return;
  }

  commandChain = commandChain
    .then(async () => {
      try {
        const result = await handleRequest(req);
        post({ type: "rpcResult", id: req.id, ok: true, result } satisfies RpcResult<unknown>);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        post({ type: "rpcResult", id: req.id, ok: false, error: message } satisfies RpcResultErr);
        postError(message);
      } finally {
        if (shouldClose) {
          ctx.close();
        }
      }
    })
    .catch((err) => {
      // Defensive: this should be unreachable because we catch per-command errors
      // above, but keep the chain alive if something unexpected slips through.
      postError(err instanceof Error ? err.message : String(err));
    });
};
