import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";

export type GuestUsbRootPort = 0 | 1;

/**
 * @deprecated Prefer {@link GuestUsbPath}. This only represents a root port index and cannot
 * express hub-backed attachment paths.
 */
export type GuestUsbPort = GuestUsbRootPort;

/**
 * Guest-side USB attachment path.
 *
 * - `guestPath[0]` is the root port index (0-based).
 * - `guestPath[1..]` are downstream hub port numbers (1-based, per USB spec).
 */
export type GuestUsbPath = number[];

/**
 * Optional message used to attach a virtual hub device before attaching
 * passthrough devices behind it.
 */
export type HidAttachHubMessage = {
  type: "hid:attachHub";
  /**
   * Path to attach the hub at. For the current guest USB topology this is `[0]` or `[1]`.
   */
  guestPath: GuestUsbPath;
  /**
   * Optional downstream port count hint. Guest implementations may ignore this.
   */
  portCount?: number;
};

export function isGuestUsbPath(value: unknown): value is GuestUsbPath {
  if (!Array.isArray(value)) return false;
  if (value.length === 0) return false;

  for (let i = 0; i < value.length; i += 1) {
    const part = value[i];
    if (typeof part !== "number") return false;
    if (!Number.isInteger(part)) return false;

    if (i === 0) {
      if (part !== 0 && part !== 1) return false;
      continue;
    }

    if (part < 1 || part > 255) return false;
  }

  return true;
}

type HidAttachMessageV0 = {
  type: "hid:attach";
  deviceId: string;
  /**
   * Optional numeric ID used by the guest-side passthrough bridges.
   *
   * When provided, the I/O worker can use this value directly instead of allocating
   * its own numeric IDs, enabling SharedArrayBuffer ring-buffer fast paths for
   * high-frequency input reports.
   */
  numericDeviceId?: number;
  guestPort: GuestUsbPort;
  /**
   * Optional for transition/interop with newer senders.
   */
  guestPath?: GuestUsbPath;
  vendorId: number;
  productId: number;
  productName?: string;
  collections: NormalizedHidCollectionInfo[];
};

type HidAttachMessageV1 = {
  type: "hid:attach";
  deviceId: string;
  /**
   * Optional numeric ID used by the guest-side passthrough bridges.
   *
   * When provided, the I/O worker can use this value directly instead of allocating
   * its own numeric IDs, enabling SharedArrayBuffer ring-buffer fast paths for
   * high-frequency input reports.
   */
  numericDeviceId?: number;
  guestPath: GuestUsbPath;
  /**
   * @deprecated Present for backwards compatibility. When `guestPath` is set,
   * this should match `guestPath[0]`.
   */
  guestPort?: GuestUsbPort;
  vendorId: number;
  productId: number;
  productName?: string;
  collections: NormalizedHidCollectionInfo[];
};

export type HidAttachMessage = HidAttachMessageV0 | HidAttachMessageV1;

export type HidDetachMessage = {
  type: "hid:detach";
  deviceId: string;
  guestPort?: GuestUsbPort;
  /**
   * Optional for transition/interop with newer senders.
   */
  guestPath?: GuestUsbPath;
};

export type HidInputReportMessage = {
  type: "hid:inputReport";
  deviceId: string;
  reportId: number;
  data: ArrayBuffer;
};

export type HidReportType = "output" | "feature";

export type HidSendReportMessage = {
  type: "hid:sendReport";
  deviceId: string;
  reportType: HidReportType;
  reportId: number;
  data: ArrayBuffer;
};

export type HidGetFeatureReportMessage = {
  /**
   * I/O worker -> main: request `HIDDevice.receiveFeatureReport(reportId)`.
   */
  type: "hid:getFeatureReport";
  deviceId: string;
  requestId: number;
  reportId: number;
};

export type HidFeatureReportResultMessage = {
  /**
   * Main -> I/O worker: response to {@link HidGetFeatureReportMessage}.
   */
  type: "hid:featureReportResult";
  deviceId: string;
  requestId: number;
  reportId: number;
  ok: boolean;
  data?: ArrayBuffer;
  error?: string;
};

export type HidPassthroughMessage =
  | HidAttachHubMessage
  | HidAttachMessage
  | HidDetachMessage
  | HidInputReportMessage
  | HidSendReportMessage
  | HidGetFeatureReportMessage
  | HidFeatureReportResultMessage;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isUint32(value: unknown): value is number {
  return isFiniteNumber(value) && Number.isInteger(value) && value >= 0 && value <= 0xffff_ffff;
}

function isUint8(value: unknown): value is number {
  return isFiniteNumber(value) && Number.isInteger(value) && value >= 0 && value <= 0xff;
}

function isArrayBuffer(value: unknown): value is ArrayBuffer {
  return value instanceof ArrayBuffer;
}

function isGuestUsbPort(value: unknown): value is GuestUsbPort {
  return value === 0 || value === 1;
}

function isHidReportType(value: unknown): value is HidReportType {
  return value === "output" || value === "feature";
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every(isFiniteNumber);
}

function isNormalizedHidReportItem(value: unknown): boolean {
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
    typeof value.isAbsolute === "boolean" &&
    typeof value.isArray === "boolean" &&
    typeof value.isBufferedBytes === "boolean" &&
    typeof value.isConstant === "boolean" &&
    typeof value.isLinear === "boolean" &&
    typeof value.isRange === "boolean" &&
    typeof value.isRelative === "boolean" &&
    typeof value.isVolatile === "boolean" &&
    typeof value.hasNull === "boolean" &&
    typeof value.hasPreferredState === "boolean" &&
    typeof value.isWrapped === "boolean"
  );
}

function isNormalizedHidReportInfo(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return isUint8(value.reportId) && Array.isArray(value.items) && value.items.every(isNormalizedHidReportItem);
}

function isNormalizedHidCollectionInfo(value: unknown): value is NormalizedHidCollectionInfo {
  if (!isRecord(value)) return false;
  if (!isFiniteNumber(value.usagePage) || !isFiniteNumber(value.usage)) return false;
  if (!isFiniteNumber(value.collectionType)) return false;
  const t = value.collectionType;
  if (t !== 0 && t !== 1 && t !== 2 && t !== 3 && t !== 4 && t !== 5 && t !== 6) return false;
  return (
    Array.isArray(value.children) &&
    value.children.every(isNormalizedHidCollectionInfo) &&
    Array.isArray(value.inputReports) &&
    value.inputReports.every(isNormalizedHidReportInfo) &&
    Array.isArray(value.outputReports) &&
    value.outputReports.every(isNormalizedHidReportInfo) &&
    Array.isArray(value.featureReports) &&
    value.featureReports.every(isNormalizedHidReportInfo)
  );
}

export function isHidAttachHubMessage(value: unknown): value is HidAttachHubMessage {
  if (!isRecord(value) || value.type !== "hid:attachHub") return false;
  if (!isGuestUsbPath(value.guestPath)) return false;
  if (value.portCount === undefined) return true;
  return isFiniteNumber(value.portCount) && Number.isInteger(value.portCount) && value.portCount > 0;
}

export function isHidAttachMessage(value: unknown): value is HidAttachMessage {
  if (!isRecord(value) || value.type !== "hid:attach") return false;
  if (typeof value.deviceId !== "string") return false;
  if (value.numericDeviceId !== undefined) {
    if (!isUint32(value.numericDeviceId)) return false;
  }
  const guestPort = value.guestPort;
  const guestPath = value.guestPath;
  if (guestPort === undefined && guestPath === undefined) return false;
  if (guestPort !== undefined && !isGuestUsbPort(guestPort)) return false;
  if (guestPath !== undefined && !isGuestUsbPath(guestPath)) return false;
  if (guestPort !== undefined && guestPath !== undefined && guestPath[0] !== guestPort) return false;
  if (!isFiniteNumber(value.vendorId) || !isFiniteNumber(value.productId)) return false;
  if (value.productName !== undefined && typeof value.productName !== "string") return false;
  if (!Array.isArray(value.collections) || !value.collections.every(isNormalizedHidCollectionInfo)) return false;
  return true;
}

export function isHidDetachMessage(value: unknown): value is HidDetachMessage {
  if (!isRecord(value) || value.type !== "hid:detach") return false;
  if (typeof value.deviceId !== "string") return false;
  const guestPort = value.guestPort;
  const guestPath = value.guestPath;
  if (guestPort !== undefined && !isGuestUsbPort(guestPort)) return false;
  if (guestPath !== undefined && !isGuestUsbPath(guestPath)) return false;
  if (guestPort !== undefined && guestPath !== undefined && guestPath[0] !== guestPort) return false;
  return true;
}

export function isHidInputReportMessage(value: unknown): value is HidInputReportMessage {
  if (!isRecord(value) || value.type !== "hid:inputReport") return false;
  if (typeof value.deviceId !== "string") return false;
  if (!isUint8(value.reportId)) return false;
  return isArrayBuffer(value.data);
}

export function isHidSendReportMessage(value: unknown): value is HidSendReportMessage {
  if (!isRecord(value) || value.type !== "hid:sendReport") return false;
  if (typeof value.deviceId !== "string") return false;
  if (!isHidReportType(value.reportType)) return false;
  if (!isUint8(value.reportId)) return false;
  return isArrayBuffer(value.data);
}

export function isHidGetFeatureReportMessage(value: unknown): value is HidGetFeatureReportMessage {
  if (!isRecord(value) || value.type !== "hid:getFeatureReport") return false;
  if (typeof value.deviceId !== "string") return false;
  if (!isUint32(value.requestId)) return false;
  return isUint8(value.reportId);
}

export function isHidFeatureReportResultMessage(value: unknown): value is HidFeatureReportResultMessage {
  if (!isRecord(value) || value.type !== "hid:featureReportResult") return false;
  if (typeof value.deviceId !== "string") return false;
  if (!isUint32(value.requestId)) return false;
  if (!isUint8(value.reportId)) return false;
  if (typeof value.ok !== "boolean") return false;
  if (value.ok) {
    return isArrayBuffer(value.data);
  }
  return value.data === undefined && (value.error === undefined || typeof value.error === "string");
}

export function isHidPassthroughMessage(value: unknown): value is HidPassthroughMessage {
  return (
    isHidAttachHubMessage(value) ||
    isHidAttachMessage(value) ||
    isHidDetachMessage(value) ||
    isHidInputReportMessage(value) ||
    isHidSendReportMessage(value) ||
    isHidGetFeatureReportMessage(value) ||
    isHidFeatureReportResultMessage(value)
  );
}
