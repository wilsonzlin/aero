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
  streamingSnapshots: boolean;
  streamingRestore: boolean;
};

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let api: WasmApi | null = null;
let variant: WasmVariant | null = null;
let vm: InstanceType<WasmApi["DemoVm"]> | null = null;

let stepsTotal = 0;
let serialBytes: number | null = 0;
const savedSerialBytesByPath = new Map<string, number | null>();

let stepTimer: number | null = null;
const STEPS_PER_TICK = 5_000;
const TICK_MS = 250;

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
  if (stepTimer !== null) {
    ctx.clearInterval(stepTimer);
    stepTimer = null;
  }
}

function startStepLoop(): void {
  stopStepLoop();
  stepTimer = ctx.setInterval(() => {
    try {
      const current = ensureVm();
      current.run_steps(STEPS_PER_TICK);
      stepsTotal += STEPS_PER_TICK;
      if (serialBytes !== null) serialBytes += STEPS_PER_TICK;
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
  api = init.api;
  variant = init.variant;

  vm = new api.DemoVm(ramBytes);
  stepsTotal = 0;
  serialBytes = 0;
  savedSerialBytesByPath.clear();

  const streamingSnapshots = typeof (vm as unknown as { snapshot_full_to_opfs?: unknown }).snapshot_full_to_opfs === "function";
  const streamingRestore =
    typeof (vm as unknown as { restore_snapshot_from_opfs?: unknown }).restore_snapshot_from_opfs === "function";

  startStepLoop();

  return { wasmVariant: variant, streamingSnapshots, streamingRestore };
}

async function handleRunSteps(steps: number): Promise<{ steps: number; serialBytes: number | null }> {
  const current = ensureVm();
  current.run_steps(steps);
  stepsTotal += steps;
  if (serialBytes !== null) serialBytes += steps;
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
    serialBytes = savedSerialBytesByPath.get(path) ?? null;
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
  ctx.close();
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

  void (async () => {
    try {
      const result = await handleRequest(req);
      post({ type: "rpcResult", id: req.id, ok: true, result } satisfies RpcResult<unknown>);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      post({ type: "rpcResult", id: req.id, ok: false, error: message } satisfies RpcResultErr);
      postError(message);
    }
  })();
};
