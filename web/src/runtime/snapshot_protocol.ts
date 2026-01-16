/**
 * Low-frequency `postMessage` protocol used to orchestrate VM snapshot save/restore
 * across the browser's multi-worker runtime (CPU + IO + NET workers).
 *
 * High-frequency traffic (port/mmio/disk I/O) continues to use the AIPC command/event
 * rings (`web/src/ipc/*`).
 */
import { serializeErrorForProtocol } from "../errors/serialize";

export type VmSnapshotRequestId = number;

export type VmSnapshotSerializedError = {
  name: string;
  message: string;
  stack?: string;
};

export type VmSnapshotOk = { ok: true };
export type VmSnapshotErr = { ok: false; error: VmSnapshotSerializedError };
export type VmSnapshotResult = VmSnapshotOk | VmSnapshotErr;

export type VmSnapshotPauseMessage = {
  kind: "vm.snapshot.pause";
  requestId: VmSnapshotRequestId;
};

export type VmSnapshotPausedMessage = {
  kind: "vm.snapshot.paused";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type VmSnapshotResumeMessage = {
  kind: "vm.snapshot.resume";
  requestId: VmSnapshotRequestId;
};

export type VmSnapshotResumedMessage = {
  kind: "vm.snapshot.resumed";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type VmSnapshotGetCpuStateMessage = {
  kind: "vm.snapshot.getCpuState";
  requestId: VmSnapshotRequestId;
};

export type VmSnapshotCpuStateMessage =
  | ({
      kind: "vm.snapshot.cpuState";
      requestId: VmSnapshotRequestId;
    } & VmSnapshotErr)
  | {
      kind: "vm.snapshot.cpuState";
      requestId: VmSnapshotRequestId;
      ok: true;
      cpu: ArrayBuffer;
      mmu: ArrayBuffer;
      /**
       * Optional CPU-owned device blobs (e.g. CPU_INTERNAL) that must be persisted alongside CPU/MMU.
       *
       * Backwards compatibility: older CPU workers omit this field.
       */
      devices?: VmSnapshotDeviceBlob[];
    };

export type VmSnapshotSetCpuStateMessage = {
  kind: "vm.snapshot.setCpuState";
  requestId: VmSnapshotRequestId;
  cpu: ArrayBuffer;
  mmu: ArrayBuffer;
  /**
   * Optional CPU-owned device blobs (e.g. CPU_INTERNAL) recovered from the snapshot file.
   *
   * Backwards compatibility: older coordinators omit this field.
   */
  devices?: VmSnapshotDeviceBlob[];
};

export type VmSnapshotCpuStateSetMessage = {
  kind: "vm.snapshot.cpuStateSet";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type VmSnapshotDeviceBlob = {
  /** Device type identifier; treated as an opaque discriminator by the coordinator. */
  kind: string;
  bytes: ArrayBuffer;
};

export type VmSnapshotSaveToOpfsMessage = {
  kind: "vm.snapshot.saveToOpfs";
  requestId: VmSnapshotRequestId;
  path: string;
  cpu: ArrayBuffer;
  mmu: ArrayBuffer;
  /**
   * Optional device blobs supplied by the coordinator (typically CPU-owned device state such as CPU_INTERNAL).
   *
   * Backwards compatibility: older coordinators omit this field.
   */
  devices?: VmSnapshotDeviceBlob[];
};

export type VmSnapshotSavedMessage = {
  kind: "vm.snapshot.saved";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type VmSnapshotRestoreFromOpfsMessage = {
  kind: "vm.snapshot.restoreFromOpfs";
  requestId: VmSnapshotRequestId;
  path: string;
};

export type VmSnapshotRestoredMessage =
  | ({
      kind: "vm.snapshot.restored";
      requestId: VmSnapshotRequestId;
    } & VmSnapshotErr)
  | {
      kind: "vm.snapshot.restored";
      requestId: VmSnapshotRequestId;
      ok: true;
      cpu: ArrayBuffer;
      mmu: ArrayBuffer;
      /**
       * Optional device state blobs recovered from the snapshot file.
       *
       * The IO worker applies relevant blobs locally (e.g. USB + networking), but the raw blobs
       * are still returned so the coordinator can dispatch additional device state
       * to other workers in follow-up tasks.
       */
      devices?: VmSnapshotDeviceBlob[];
    };

export type VmSnapshotMachineSaveToOpfsMessage = {
  kind: "vm.snapshot.machine.saveToOpfs";
  requestId: VmSnapshotRequestId;
  path: string;
};

export type VmSnapshotMachineSavedMessage = {
  kind: "vm.snapshot.machine.saved";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type VmSnapshotMachineRestoreFromOpfsMessage = {
  kind: "vm.snapshot.machine.restoreFromOpfs";
  requestId: VmSnapshotRequestId;
  path: string;
};

export type VmSnapshotMachineRestoredMessage = {
  kind: "vm.snapshot.machine.restored";
  requestId: VmSnapshotRequestId;
} & VmSnapshotResult;

export type CoordinatorToWorkerSnapshotMessage =
  | VmSnapshotPauseMessage
  | VmSnapshotResumeMessage
  | VmSnapshotGetCpuStateMessage
  | VmSnapshotSetCpuStateMessage
  | VmSnapshotSaveToOpfsMessage
  | VmSnapshotRestoreFromOpfsMessage
  | VmSnapshotMachineSaveToOpfsMessage
  | VmSnapshotMachineRestoreFromOpfsMessage;

export type WorkerToCoordinatorSnapshotMessage =
  | VmSnapshotPausedMessage
  | VmSnapshotResumedMessage
  | VmSnapshotCpuStateMessage
  | VmSnapshotCpuStateSetMessage
  | VmSnapshotSavedMessage
  | VmSnapshotRestoredMessage
  | VmSnapshotMachineSavedMessage
  | VmSnapshotMachineRestoredMessage;

export function serializeVmSnapshotError(err: unknown): VmSnapshotSerializedError {
  return serializeErrorForProtocol(err);
}

export function isWorkerToCoordinatorSnapshotMessage(value: unknown): value is WorkerToCoordinatorSnapshotMessage {
  if (!value || typeof value !== "object") return false;
  const msg = value as { kind?: unknown; requestId?: unknown };
  if (typeof msg.kind !== "string") return false;
  if (typeof msg.requestId !== "number") return false;
  return (
    msg.kind === "vm.snapshot.paused" ||
    msg.kind === "vm.snapshot.resumed" ||
    msg.kind === "vm.snapshot.cpuState" ||
    msg.kind === "vm.snapshot.cpuStateSet" ||
    msg.kind === "vm.snapshot.saved" ||
    msg.kind === "vm.snapshot.restored" ||
    msg.kind === "vm.snapshot.machine.saved" ||
    msg.kind === "vm.snapshot.machine.restored"
  );
}
