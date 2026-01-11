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
export type DemoVmWorkerSerializedError = { name: string; message: string; stack?: string };
export type DemoVmWorkerRpcResultErr = { type: "rpcResult"; id: number; ok: false; error: DemoVmWorkerSerializedError };
export type DemoVmWorkerRpcResult<T> = DemoVmWorkerRpcResultOk<T> | DemoVmWorkerRpcResultErr;

export type DemoVmWorkerStatusMessage = { type: "status"; steps: number; serialBytes: number | null };
export type DemoVmWorkerErrorMessage = { type: "error"; error: DemoVmWorkerSerializedError };

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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const msg = value as any;
  switch (msg.type) {
    case "rpcResult": {
      if (typeof msg.id !== "number" || typeof msg.ok !== "boolean") return false;
      if (msg.ok) return true;
      const err = msg.error;
      return (
        !!err &&
        typeof err === "object" &&
        typeof err.name === "string" &&
        typeof err.message === "string" &&
        (typeof err.stack === "string" || typeof err.stack === "undefined")
      );
    }
    case "status":
      return typeof msg.steps === "number" && (typeof msg.serialBytes === "number" || msg.serialBytes === null);
    case "error": {
      const err = msg.error;
      return (
        !!err &&
        typeof err === "object" &&
        typeof err.name === "string" &&
        typeof err.message === "string" &&
        (typeof err.stack === "string" || typeof err.stack === "undefined")
      );
    }
    default:
      return false;
  }
}
