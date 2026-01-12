import type { NormalizedHidCollectionInfo } from "./webhid_normalize";
import { isGuestUsbPath, type GuestUsbPath, type GuestUsbPort } from "../platform/hid_passthrough_protocol";

export type HidReportType = "output" | "feature";

export type HidAttachMessage = {
  type: "hid.attach";
  deviceId: number;
  vendorId: number;
  productId: number;
  productName?: string;
  /**
   * Optional hint for the guest-side USB attachment path.
   *
   * This is forward-compatible with the "external hub behind root port 0" topology
   * used by `WebHidPassthroughManager` (paths like `0.4`).
   */
  guestPath?: GuestUsbPath;
  /**
   * @deprecated Prefer {@link HidAttachMessage.guestPath}.
   *
   * Optional hint for which guest UHCI root port this device should be attached to.
   *
   * This is currently only used for forward-compatible guest USB wiring; the
   * passthrough bridge itself is keyed by `deviceId`.
   *
   * When `guestPath` is set, this should match `guestPath[0]`.
   */
  guestPort?: GuestUsbPort;
  collections: NormalizedHidCollectionInfo[];
  /**
   * True when the device declares any output reports. This is used by the
   * guest-side USB stack to decide whether it needs to expose an interrupt OUT
   * endpoint (feature reports are sent over the control endpoint).
   */
  hasInterruptOut: boolean;
};

export type HidDetachMessage = {
  type: "hid.detach";
  deviceId: number;
};

export type HidRingAttachMessage = {
  type: "hid.ringAttach";
  inputRing: SharedArrayBuffer;
  outputRing: SharedArrayBuffer;
};

export type HidRingInitMessage = {
  type: "hid.ring.init";
  sab: SharedArrayBuffer;
  offsetBytes: number;
};

export type HidInputReportMessage = {
  type: "hid.inputReport";
  deviceId: number;
  reportId: number;
  // This buffer is transferred between threads; it should always be backed by an ArrayBuffer
  // (not a SharedArrayBuffer).
  data: Uint8Array<ArrayBuffer>;
  /**
   * Optional timestamp (DOMHighResTimeStamp). `Event.timeStamp` is relative to
   * page start, so consumers should treat this as best-effort debugging data.
   */
  tsMs?: number;
};

export type HidSendReportMessage = {
  type: "hid.sendReport";
  deviceId: number;
  reportType: HidReportType;
  reportId: number;
  // This buffer is transferred between threads; it should always be backed by an ArrayBuffer
  // (not a SharedArrayBuffer).
  data: Uint8Array<ArrayBuffer>;
};

export type HidLogMessage = {
  type: "hid.log";
  message: string;
  deviceId?: number;
};

export type HidErrorMessage = {
  type: "hid.error";
  message: string;
  deviceId?: number;
};

export type HidProxyMessage =
  | HidAttachMessage
  | HidDetachMessage
  | HidRingAttachMessage
  | HidRingInitMessage
  | HidInputReportMessage
  | HidSendReportMessage
  | HidLogMessage
  | HidErrorMessage;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isOptionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

function isOptionalGuestUsbPort(value: unknown): value is GuestUsbPort | undefined {
  return value === undefined || value === 0 || value === 1;
}

function isOptionalGuestUsbPath(value: unknown): value is GuestUsbPath | undefined {
  return value === undefined || isGuestUsbPath(value);
}

function isBoolean(value: unknown): value is boolean {
  return typeof value === "boolean";
}

function isArrayBufferBackedUint8Array(value: unknown): value is Uint8Array<ArrayBuffer> {
  return value instanceof Uint8Array && value.buffer instanceof ArrayBuffer;
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every(isFiniteNumber);
}

function isHidReportItem(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return (
    isFiniteNumber(value.usagePage) &&
    isNumberArray(value.usages) &&
    isFiniteNumber(value.usageMinimum) &&
    isFiniteNumber(value.usageMaximum) &&
    isFiniteNumber(value.reportSize) &&
    isFiniteNumber(value.reportCount) &&
    isFiniteNumber(value.unitExponent) &&
    isFiniteNumber(value.unit) &&
    isFiniteNumber(value.logicalMinimum) &&
    isFiniteNumber(value.logicalMaximum) &&
    isFiniteNumber(value.physicalMinimum) &&
    isFiniteNumber(value.physicalMaximum) &&
    isNumberArray(value.strings) &&
    isFiniteNumber(value.stringMinimum) &&
    isFiniteNumber(value.stringMaximum) &&
    isNumberArray(value.designators) &&
    isFiniteNumber(value.designatorMinimum) &&
    isFiniteNumber(value.designatorMaximum) &&
    isBoolean(value.isAbsolute) &&
    isBoolean(value.isArray) &&
    isBoolean(value.isBufferedBytes) &&
    isBoolean(value.isConstant) &&
    isBoolean(value.isLinear) &&
    isBoolean(value.isRange) &&
    isBoolean(value.isRelative) &&
    isBoolean(value.isVolatile) &&
    isBoolean(value.hasNull) &&
    isBoolean(value.hasPreferredState) &&
    isBoolean(value.isWrapped)
  );
}

function isHidReportInfo(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return isFiniteNumber(value.reportId) && Array.isArray(value.items) && value.items.every(isHidReportItem);
}

function isNormalizedHidCollectionInfo(value: unknown): value is NormalizedHidCollectionInfo {
  if (!isRecord(value)) return false;
  if (!isFiniteNumber(value.usagePage) || !isFiniteNumber(value.usage)) return false;
  if (!isFiniteNumber(value.collectionType)) return false;
  const ct = value.collectionType;
  if (ct !== (ct | 0) || ct < 0 || ct > 6) return false;
  if (!Array.isArray(value.children) || !value.children.every(isNormalizedHidCollectionInfo)) return false;
  if (!Array.isArray(value.inputReports) || !value.inputReports.every(isHidReportInfo)) return false;
  if (!Array.isArray(value.outputReports) || !value.outputReports.every(isHidReportInfo)) return false;
  if (!Array.isArray(value.featureReports) || !value.featureReports.every(isHidReportInfo)) return false;
  return true;
}

export function isHidAttachMessage(value: unknown): value is HidAttachMessage {
  if (!isRecord(value) || value.type !== "hid.attach") return false;
  if (!isFiniteNumber(value.deviceId)) return false;
  if (!isFiniteNumber(value.vendorId) || !isFiniteNumber(value.productId)) return false;
  if (!isOptionalString(value.productName)) return false;

  const guestPath = value.guestPath;
  if (!isOptionalGuestUsbPath(guestPath)) return false;

  const guestPort = value.guestPort;
  if (!isOptionalGuestUsbPort(guestPort)) return false;

  if (guestPath !== undefined && guestPort !== undefined && guestPath[0] !== guestPort) return false;
  if (!Array.isArray(value.collections) || !value.collections.every(isNormalizedHidCollectionInfo)) return false;
  if (!isBoolean(value.hasInterruptOut)) return false;
  return true;
}

export function isHidDetachMessage(value: unknown): value is HidDetachMessage {
  if (!isRecord(value) || value.type !== "hid.detach") return false;
  return isFiniteNumber(value.deviceId);
}

export function isHidRingAttachMessage(value: unknown): value is HidRingAttachMessage {
  if (!isRecord(value) || value.type !== "hid.ringAttach") return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  if (!(value.inputRing instanceof SharedArrayBuffer)) return false;
  if (!(value.outputRing instanceof SharedArrayBuffer)) return false;
  return true;
}

export function isHidRingInitMessage(value: unknown): value is HidRingInitMessage {
  if (!isRecord(value) || value.type !== "hid.ring.init") return false;
  const offsetBytes = value.offsetBytes;
  if (!isFiniteNumber(offsetBytes) || !Number.isInteger(offsetBytes) || offsetBytes < 0) return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  return value.sab instanceof SharedArrayBuffer;
}

export function isHidInputReportMessage(value: unknown): value is HidInputReportMessage {
  if (!isRecord(value) || value.type !== "hid.inputReport") return false;
  if (!isFiniteNumber(value.deviceId) || !isFiniteNumber(value.reportId)) return false;
  if (!isArrayBufferBackedUint8Array(value.data)) return false;
  if (value.tsMs !== undefined && !isFiniteNumber(value.tsMs)) return false;
  return true;
}

export function isHidSendReportMessage(value: unknown): value is HidSendReportMessage {
  if (!isRecord(value) || value.type !== "hid.sendReport") return false;
  if (!isFiniteNumber(value.deviceId) || !isFiniteNumber(value.reportId)) return false;
  if (value.reportType !== "output" && value.reportType !== "feature") return false;
  if (!isArrayBufferBackedUint8Array(value.data)) return false;
  return true;
}

export function isHidLogMessage(value: unknown): value is HidLogMessage {
  if (!isRecord(value) || value.type !== "hid.log") return false;
  if (typeof value.message !== "string") return false;
  if (value.deviceId !== undefined && !isFiniteNumber(value.deviceId)) return false;
  return true;
}

export function isHidErrorMessage(value: unknown): value is HidErrorMessage {
  if (!isRecord(value) || value.type !== "hid.error") return false;
  if (typeof value.message !== "string") return false;
  if (value.deviceId !== undefined && !isFiniteNumber(value.deviceId)) return false;
  return true;
}

export function isHidProxyMessage(value: unknown): value is HidProxyMessage {
  return (
    isHidAttachMessage(value) ||
    isHidDetachMessage(value) ||
    isHidRingAttachMessage(value) ||
    isHidRingInitMessage(value) ||
    isHidInputReportMessage(value) ||
    isHidSendReportMessage(value) ||
    isHidLogMessage(value) ||
    isHidErrorMessage(value)
  );
}
