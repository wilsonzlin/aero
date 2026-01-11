export type DemoVmWorkerInitRequest = { id: number; type: "init"; ramBytes: number };
export type DemoVmWorkerRunStepsRequest = { id: number; type: "runSteps"; steps: number };
export type DemoVmWorkerSnapshotFullToOpfsRequest = { id: number; type: "snapshotFullToOpfs"; path: string };
export type DemoVmWorkerSnapshotDirtyToOpfsRequest = { id: number; type: "snapshotDirtyToOpfs"; path: string };
export type DemoVmWorkerRestoreFromOpfsRequest = { id: number; type: "restoreFromOpfs"; path: string };
export type DemoVmWorkerGetSerialStatsRequest = { id: number; type: "getSerialStats" };
export type DemoVmWorkerGetSerialOutputLenRequest = { id: number; type: "getSerialOutputLen" };
export type DemoVmWorkerShutdownRequest = { id: number; type: "shutdown" };

export type DemoVmWorkerRequest =
  | DemoVmWorkerInitRequest
  | DemoVmWorkerRunStepsRequest
  | DemoVmWorkerSnapshotFullToOpfsRequest
  | DemoVmWorkerSnapshotDirtyToOpfsRequest
  | DemoVmWorkerRestoreFromOpfsRequest
  | DemoVmWorkerGetSerialStatsRequest
  | DemoVmWorkerGetSerialOutputLenRequest
  | DemoVmWorkerShutdownRequest;

export type DemoVmWorkerRpcResultOk<T> = { type: "rpcResult"; id: number; ok: true; result: T };
export type DemoVmWorkerRpcResultErr = { type: "rpcResult"; id: number; ok: false; error: string };
export type DemoVmWorkerRpcResult<T> = DemoVmWorkerRpcResultOk<T> | DemoVmWorkerRpcResultErr;

export type DemoVmWorkerStatusMessage = { type: "status"; steps: number; serialBytes: number | null };
export type DemoVmWorkerErrorMessage = { type: "error"; message: string };

export type DemoVmWorkerMessage = DemoVmWorkerRpcResult<unknown> | DemoVmWorkerStatusMessage | DemoVmWorkerErrorMessage;

export type DemoVmWorkerInitResult = {
  wasmVariant: string;
  syncAccessHandles: boolean;
  streamingSnapshots: boolean;
  streamingDirtySnapshots: boolean;
  streamingRestore: boolean;
};

export type DemoVmWorkerStepResult = { steps: number; serialBytes: number | null };
export type DemoVmWorkerSerialOutputLenResult = { serialBytes: number | null };
export type DemoVmWorkerSerialStatsResult = { steps: number; serialBytes: number | null };

export function isDemoVmWorkerMessage(value: unknown): value is DemoVmWorkerMessage {
  if (!value || typeof value !== "object") return false;
  const type = (value as { type?: unknown }).type;
  return (
    type === "rpcResult" ||
    type === "status" ||
    type === "error"
  );
}
