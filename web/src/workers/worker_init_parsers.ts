import type { IoWorkerInitOptions } from "./io_worker_runtime.ts";

import { queueKind } from "../ipc/layout.ts";
import {
  hasOwnPropBestEffort,
  isSharedArrayBufferValue,
  tryGetOwnPropBestEffort,
  tryGetSafeIntegerBestEffort,
  tryGetStringArrayBestEffort,
  tryGetStringBestEffort,
} from "./worker_message_safe.ts";

export function parseIoWorkerInitMessage(data: unknown): IoWorkerInitOptions | null {
  const type = tryGetStringBestEffort(data, "type");
  if (type !== "init") return null;

  const requestRing = tryGetOwnPropBestEffort(data, "requestRing");
  const responseRing = tryGetOwnPropBestEffort(data, "responseRing");
  if (!isSharedArrayBufferValue(requestRing) || !isSharedArrayBufferValue(responseRing)) return null;

  const tickIntervalMsRaw = tryGetSafeIntegerBestEffort(data, "tickIntervalMs");
  const tickIntervalMs = tickIntervalMsRaw !== undefined && tickIntervalMsRaw > 0 ? tickIntervalMsRaw : undefined;

  const devicesProvided = hasOwnPropBestEffort(data, "devices");
  const devicesRaw = tryGetStringArrayBestEffort(data, "devices");
  const devices = devicesProvided ? (devicesRaw ?? undefined) : undefined;

  const stopSignal = tryGetOwnPropBestEffort(data, "stopSignal");
  const stopSignalValue = isSharedArrayBufferValue(stopSignal) ? stopSignal : undefined;

  return {
    requestRing,
    responseRing,
    ...(tickIntervalMs !== undefined ? { tickIntervalMs } : {}),
    ...(devices !== undefined ? { devices } : {}),
    ...(stopSignalValue ? { stopSignal: stopSignalValue } : {}),
  };
}

export type IoAipcWorkerInitOptions = {
  ipcBuffer: SharedArrayBuffer;
  cmdKind: number;
  evtKind: number;
  tickIntervalMs: number;
  devices: string[];
};

export function parseIoAipcWorkerInitMessage(data: unknown): IoAipcWorkerInitOptions | null {
  const type = tryGetStringBestEffort(data, "type");
  if (type !== "init") return null;

  const ipcBuffer = tryGetOwnPropBestEffort(data, "ipcBuffer");
  if (!isSharedArrayBufferValue(ipcBuffer)) return null;

  const cmdKindRaw = tryGetSafeIntegerBestEffort(data, "cmdKind");
  const cmdKind = cmdKindRaw !== undefined ? cmdKindRaw : queueKind.CMD;

  const evtKindRaw = tryGetSafeIntegerBestEffort(data, "evtKind");
  const evtKind = evtKindRaw !== undefined ? evtKindRaw : queueKind.EVT;

  const tickIntervalMsRaw = tryGetSafeIntegerBestEffort(data, "tickIntervalMs");
  const tickIntervalMs = tickIntervalMsRaw !== undefined && tickIntervalMsRaw > 0 ? tickIntervalMsRaw : 5;

  const devicesProvided = hasOwnPropBestEffort(data, "devices");
  const devicesRaw = tryGetStringArrayBestEffort(data, "devices");
  const devices = devicesProvided ? (devicesRaw ?? ["i8042"]) : ["i8042"];

  return { ipcBuffer, cmdKind, evtKind, tickIntervalMs, devices };
}

export type SerialDemoCpuWorkerInitOptions = {
  requestRing: SharedArrayBuffer;
  responseRing: SharedArrayBuffer;
  text: string;
};

export function parseSerialDemoCpuWorkerInitMessage(data: unknown): SerialDemoCpuWorkerInitOptions | null {
  const type = tryGetStringBestEffort(data, "type");
  if (type !== "init") return null;

  const requestRing = tryGetOwnPropBestEffort(data, "requestRing");
  const responseRing = tryGetOwnPropBestEffort(data, "responseRing");
  if (!isSharedArrayBufferValue(requestRing) || !isSharedArrayBufferValue(responseRing)) return null;

  const textRaw = tryGetStringBestEffort(data, "text");
  const text = textRaw !== undefined ? textRaw : "Hello from COM1!\r\n";

  return { requestRing, responseRing, text };
}

