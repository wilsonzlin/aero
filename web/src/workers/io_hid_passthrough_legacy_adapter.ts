import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";
import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage } from "../hid/hid_proxy_protocol";
import type {
  GuestUsbPath,
  GuestUsbPort,
  HidAttachMessage as HidPassthroughAttachMessage,
  HidDetachMessage as HidPassthroughDetachMessage,
  HidInputReportMessage as HidPassthroughInputReportMessage,
  HidSendReportMessage as HidPassthroughSendReportMessage,
} from "../platform/hid_passthrough_protocol";

const DEFAULT_LEGACY_DEVICE_ID_BASE = 0x4000_0000;

export function computeHasInterruptOut(collections: NormalizedHidCollectionInfo[]): boolean {
  const stack = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    // Feature reports are transferred over the control endpoint (SET_REPORT/GET_REPORT) and do
    // not require an interrupt OUT endpoint. Only output reports imply an interrupt OUT endpoint.
    if (node.outputReports.length > 0) return true;
    for (const child of node.children) stack.push(child);
  }
  return false;
}

function arrayBufferFromView(view: Uint8Array): ArrayBuffer {
  if (view.buffer instanceof ArrayBuffer) {
    if (view.byteOffset === 0 && view.byteLength === view.buffer.byteLength) return view.buffer;
    return view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength);
  }

  const out = new Uint8Array(view.byteLength);
  out.set(view);
  return out.buffer;
}

/**
 * Adapter for the legacy `hid:*` passthrough protocol.
 *
 * The legacy protocol uses string device IDs (WebHID `HIDDevice.id`), while the guest-side bridges
 * use numeric IDs. This helper assigns a stable numeric ID to each legacy string ID and can
 * translate messages into the newer `hid.*` protocol.
 */
export class IoWorkerLegacyHidPassthroughAdapter {
  #nextDeviceId: number;

  readonly #numericIdByLegacyId = new Map<string, number>();
  readonly #legacyIdByNumericId = new Map<number, string>();
  readonly #attachedNumericIds = new Set<number>();

  constructor(options: { firstDeviceId?: number } = {}) {
    this.#nextDeviceId = options.firstDeviceId ?? DEFAULT_LEGACY_DEVICE_ID_BASE;
  }

  attach(msg: HidPassthroughAttachMessage): HidAttachMessage {
    const existing = this.#numericIdByLegacyId.get(msg.deviceId);
    const deviceId = existing ?? this.#nextDeviceId++;
    if (existing === undefined) {
      this.#numericIdByLegacyId.set(msg.deviceId, deviceId);
      this.#legacyIdByNumericId.set(deviceId, msg.deviceId);
    }

    const guestPath: GuestUsbPath = msg.guestPath ?? [msg.guestPort as GuestUsbPort];
    const guestPort: GuestUsbPort = guestPath[0] as GuestUsbPort;

    this.#attachedNumericIds.add(deviceId);

    return {
      type: "hid.attach",
      deviceId,
      vendorId: msg.vendorId,
      productId: msg.productId,
      ...(msg.productName ? { productName: msg.productName } : {}),
      guestPath,
      guestPort,
      collections: msg.collections,
      hasInterruptOut: computeHasInterruptOut(msg.collections),
    };
  }

  detach(msg: HidPassthroughDetachMessage): HidDetachMessage | null {
    const deviceId = this.#numericIdByLegacyId.get(msg.deviceId);
    if (deviceId === undefined) return null;
    this.#attachedNumericIds.delete(deviceId);
    return { type: "hid.detach", deviceId };
  }

  inputReport(msg: HidPassthroughInputReportMessage): HidInputReportMessage | null {
    const deviceId = this.#numericIdByLegacyId.get(msg.deviceId);
    if (deviceId === undefined) return null;
    if (!this.#attachedNumericIds.has(deviceId)) return null;

    return {
      type: "hid.inputReport",
      deviceId,
      reportId: msg.reportId,
      data: new Uint8Array(msg.data) as Uint8Array<ArrayBuffer>,
    };
  }

  sendReport(payload: { deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }): HidPassthroughSendReportMessage | null {
    const legacyId = this.#legacyIdByNumericId.get(payload.deviceId);
    if (!legacyId) return null;

    const buffer = arrayBufferFromView(payload.data);
    return {
      type: "hid:sendReport",
      deviceId: legacyId,
      reportType: payload.reportType,
      reportId: payload.reportId,
      data: buffer,
    };
  }
}
