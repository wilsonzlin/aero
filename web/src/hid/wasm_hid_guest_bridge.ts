import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import type { WasmApi } from "../runtime/wasm_context";

import type { HidAttachMessage, HidDetachMessage, HidFeatureReportResultMessage, HidInputReportMessage } from "./hid_proxy_protocol";

export type HidTopologyManager = {
  attachDevice(deviceId: number, path: GuestUsbPath, kind: "webhid" | "usb-hid-passthrough", device: unknown): void;
  detachDevice(deviceId: number): void;
};

export type HidHostSink = {
  sendReport: (msg: { deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }) => void;
  requestFeatureReport: (msg: { deviceId: number; requestId: number; reportId: number }) => void;
  log: (message: string, deviceId?: number) => void;
  error: (message: string, deviceId?: number) => void;
};

export interface HidGuestBridge {
  attach(msg: HidAttachMessage): void;
  detach(msg: HidDetachMessage): void;
  inputReport(msg: HidInputReportMessage): void;
  featureReportResult?(msg: HidFeatureReportResultMessage): void;
  poll?(): void;
  completeFeatureReportRequest?(msg: { deviceId: number; requestId: number; reportId: number; data: Uint8Array }): boolean;
  failFeatureReportRequest?(msg: { deviceId: number; requestId: number; reportId: number; error?: string }): boolean;
  destroy?(): void;
}

const MAX_HID_OUTPUT_REPORTS_PER_TICK = 64;
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;
const MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK = 16;

type WebHidPassthroughBridge = InstanceType<WasmApi["WebHidPassthroughBridge"]>;
type UsbHidPassthroughBridge = InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>;
type HidPassthroughBridge = WebHidPassthroughBridge | UsbHidPassthroughBridge;

type HidPassthroughBridgeCompat = {
  bridge: HidPassthroughBridge;
  pushInputReport: (reportId: number, data: Uint8Array) => void;
  drainNextOutputReport: () => { reportType: "output" | "feature"; reportId: number; data: Uint8Array } | null;
  drainNextFeatureReportRequest?: () => unknown;
  completeFeatureReportRequest?: (requestId: number, reportId: number, data: Uint8Array) => boolean;
  failFeatureReportRequest?: (requestId: number, reportId: number, error?: string) => boolean;
  configured: () => boolean;
  free: () => void;
};

function resolveHidPassthroughBridgeCompat(bridge: HidPassthroughBridge): HidPassthroughBridgeCompat {
  const anyBridge = bridge as unknown as Record<string, unknown>;

  // Backwards compatibility: accept camelCase method names from older wasm-bindgen outputs / shims.
  const pushInputReport = anyBridge.push_input_report ?? anyBridge.pushInputReport;
  const drainNextOutputReport = anyBridge.drain_next_output_report ?? anyBridge.drainNextOutputReport;
  const drainNextFeatureReportRequest =
    anyBridge.drain_next_feature_report_request ?? anyBridge.drainNextFeatureReportRequest;
  const completeFeatureReportRequest =
    anyBridge.complete_feature_report_request ?? anyBridge.completeFeatureReportRequest;
  const failFeatureReportRequest = anyBridge.fail_feature_report_request ?? anyBridge.failFeatureReportRequest;
  const configured = anyBridge.configured;
  const free = anyBridge.free;

  if (typeof pushInputReport !== "function") throw new Error("HID passthrough bridge missing push_input_report/pushInputReport.");
  if (typeof drainNextOutputReport !== "function")
    throw new Error("HID passthrough bridge missing drain_next_output_report/drainNextOutputReport.");
  if (typeof configured !== "function") throw new Error("HID passthrough bridge missing configured().");
  if (typeof free !== "function") throw new Error("HID passthrough bridge missing free().");

  return {
    bridge,
    pushInputReport: pushInputReport as HidPassthroughBridgeCompat["pushInputReport"],
    drainNextOutputReport: drainNextOutputReport as HidPassthroughBridgeCompat["drainNextOutputReport"],
    ...(typeof drainNextFeatureReportRequest === "function"
      ? { drainNextFeatureReportRequest: drainNextFeatureReportRequest as HidPassthroughBridgeCompat["drainNextFeatureReportRequest"] }
      : {}),
    ...(typeof completeFeatureReportRequest === "function"
      ? {
          completeFeatureReportRequest: completeFeatureReportRequest as HidPassthroughBridgeCompat["completeFeatureReportRequest"],
        }
      : {}),
    ...(typeof failFeatureReportRequest === "function"
      ? { failFeatureReportRequest: failFeatureReportRequest as HidPassthroughBridgeCompat["failFeatureReportRequest"] }
      : {}),
    configured: configured as HidPassthroughBridgeCompat["configured"],
    free: free as HidPassthroughBridgeCompat["free"],
  };
}

const HID_COLLECTION_TYPE_APPLICATION = 1;
const HID_USAGE_PAGE_GENERIC_DESKTOP = 0x01;
const HID_USAGE_GENERIC_DESKTOP_MOUSE = 0x02;
const HID_USAGE_GENERIC_DESKTOP_KEYBOARD = 0x06;
const HID_INTERFACE_SUBCLASS_BOOT = 0x01;
const HID_INTERFACE_PROTOCOL_KEYBOARD = 0x01;
const HID_INTERFACE_PROTOCOL_MOUSE = 0x02;

function inferHidBootInterfaceFromCollections(
  collections: HidAttachMessage["collections"],
): { interface_subclass?: number; interface_protocol?: number } {
  let hasKeyboard = false;
  let hasMouse = false;
  for (const col of collections) {
    if (col.collectionType !== HID_COLLECTION_TYPE_APPLICATION) continue;
    if (col.usagePage !== HID_USAGE_PAGE_GENERIC_DESKTOP) continue;
    if (col.usage === HID_USAGE_GENERIC_DESKTOP_KEYBOARD) {
      hasKeyboard = true;
    } else if (col.usage === HID_USAGE_GENERIC_DESKTOP_MOUSE) {
      hasMouse = true;
    }
  }

  // If the device exposes both keyboard and mouse collections, don't guess: the USB HID Boot
  // interface protocol can only represent one.
  if (hasKeyboard && !hasMouse) {
    return { interface_subclass: HID_INTERFACE_SUBCLASS_BOOT, interface_protocol: HID_INTERFACE_PROTOCOL_KEYBOARD };
  }
  if (hasMouse && !hasKeyboard) {
    return { interface_subclass: HID_INTERFACE_SUBCLASS_BOOT, interface_protocol: HID_INTERFACE_PROTOCOL_MOUSE };
  }
  return {};
}

type FeatureReportRequest = { requestId: number; reportId: number };

function isFeatureReportRequest(value: unknown): value is FeatureReportRequest {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return typeof v.requestId === "number" && typeof v.reportId === "number";
}

export class WasmHidGuestBridge implements HidGuestBridge {
  readonly #bridges = new Map<number, HidPassthroughBridgeCompat>();
  #api: WasmApi;
  #host: HidHostSink;
  #topology: HidTopologyManager;

  constructor(api: WasmApi, host: HidHostSink, topology: HidTopologyManager) {
    this.#api = api;
    this.#host = host;
    this.#topology = topology;
  }

  attach(msg: HidAttachMessage): void {
    this.detach({ type: "hid.detach", deviceId: msg.deviceId });
    const guestPathHint = msg.guestPath ?? (msg.guestPort !== undefined ? ([msg.guestPort] as GuestUsbPath) : undefined);
    const guestPath = guestPathHint;

    try {
      const UsbBridge = this.#api.UsbHidPassthroughBridge;
      const apiAny = this.#api as unknown as Record<string, unknown>;
      const synthesize =
        (apiAny.synthesize_webhid_report_descriptor ??
          apiAny.synthesizeWebhidReportDescriptor ??
          apiAny.synthesizeWebHidReportDescriptor) as WasmApi["synthesize_webhid_report_descriptor"] | undefined;

      let bridge: HidPassthroughBridge;
      let kind: "webhid" | "usb-hid-passthrough" = "webhid";
      if (UsbBridge && synthesize) {
        const reportDescriptorBytes = synthesize(msg.collections);
        const { interface_subclass, interface_protocol } = inferHidBootInterfaceFromCollections(msg.collections);
        bridge = new UsbBridge(
          msg.vendorId,
          msg.productId,
          undefined,
          msg.productName,
          undefined,
          reportDescriptorBytes,
          msg.hasInterruptOut,
          interface_subclass,
          interface_protocol,
        );
        kind = "usb-hid-passthrough";
      } else {
        bridge = new this.#api.WebHidPassthroughBridge(
          msg.vendorId,
          msg.productId,
          undefined,
          msg.productName,
          undefined,
          msg.collections,
        );
      }

      const compat = resolveHidPassthroughBridgeCompat(bridge);
      this.#bridges.set(msg.deviceId, compat);
      if (guestPath) {
        this.#topology.attachDevice(msg.deviceId, guestPath, kind, compat.bridge);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`hid.attach failed: ${message}`, msg.deviceId);
      return;
    }
  }

  detach(msg: HidDetachMessage): void {
    this.#topology.detachDevice(msg.deviceId);
    const existing = this.#bridges.get(msg.deviceId);
    if (!existing) return;

    this.#bridges.delete(msg.deviceId);
    try {
      existing.free.call(existing.bridge);
    } catch {
      // ignore
    }
  }

  inputReport(msg: HidInputReportMessage): void {
    const entry = this.#bridges.get(msg.deviceId);
    if (!entry) return;
    try {
      const data = (() => {
        if (msg.data.byteLength <= MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) return msg.data;
        // Defensive clamp: WebHID input reports are validated/synthesized as full-speed interrupt
        // payloads (<= 64 bytes). If a buggy browser/device delivers a larger report (or if the
        // SharedArrayBuffer ring is corrupted), avoid copying the entire payload into WASM memory.
        const out = new Uint8Array(MAX_HID_INPUT_REPORT_PAYLOAD_BYTES);
        out.set(msg.data.subarray(0, MAX_HID_INPUT_REPORT_PAYLOAD_BYTES));
        return out as Uint8Array<ArrayBuffer>;
      })();
      entry.pushInputReport.call(entry.bridge, msg.reportId, data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`WebHID push_input_report failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    let remainingReports = MAX_HID_OUTPUT_REPORTS_PER_TICK;
    let remainingFeatureRequests = MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK;
    for (const [deviceId, entry] of this.#bridges) {
      if (remainingReports <= 0 && remainingFeatureRequests <= 0) return;
      let configured = false;
      try {
        configured = entry.configured.call(entry.bridge);
      } catch {
        configured = false;
      }

      if (configured) {
        while (remainingReports > 0) {
          let report: { reportType: "output" | "feature"; reportId: number; data: Uint8Array } | null = null;
          try {
            report = entry.drainNextOutputReport.call(entry.bridge);
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            this.#host.error(`drain_next_output_report failed: ${message}`, deviceId);
            break;
          }
          if (!report) break;

          remainingReports -= 1;
          this.#host.sendReport({
            deviceId,
            reportType: report.reportType,
            reportId: report.reportId,
            data: report.data,
          });
        }
      }

      const drainFeatureReportRequest = entry.drainNextFeatureReportRequest;
      if (typeof drainFeatureReportRequest === "function") {
        // Feature report reads are delivered over the control endpoint and can occur before the
        // guest has configured the USB device. Always drain them (independent of `configured`).
        while (remainingFeatureRequests > 0) {
          let req: FeatureReportRequest | null = null;
          try {
            const next = drainFeatureReportRequest.call(entry.bridge) as unknown;
            if (next === null) break;
            if (!isFeatureReportRequest(next)) break;
            req = next;
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            this.#host.error(`drain_next_feature_report_request failed: ${message}`, deviceId);
            break;
          }
          if (!req) break;
          remainingFeatureRequests -= 1;
          this.#host.requestFeatureReport({ deviceId, requestId: req.requestId, reportId: req.reportId });
        }
      }
    }
  }

  completeFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; data: Uint8Array }): boolean {
    const entry = this.#bridges.get(msg.deviceId);
    if (!entry) return false;
    const fn = entry.completeFeatureReportRequest;
    if (typeof fn !== "function") return false;
    try {
      return fn.call(entry.bridge, msg.requestId >>> 0, msg.reportId >>> 0, msg.data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`complete_feature_report_request failed: ${message}`, msg.deviceId);
      return false;
    }
  }

  failFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; error?: string }): boolean {
    const entry = this.#bridges.get(msg.deviceId);
    if (!entry) return false;
    const fn = entry.failFeatureReportRequest;
    if (typeof fn === "function") {
      try {
        return fn.call(entry.bridge, msg.requestId >>> 0, msg.reportId >>> 0, msg.error);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        this.#host.error(`fail_feature_report_request failed: ${message}`, msg.deviceId);
        return false;
      }
    }
    // Best-effort: if the WASM build doesn't expose an explicit failure method, complete with an
    // empty payload so the guest control transfer doesn't hang indefinitely.
    const complete = entry.completeFeatureReportRequest;
    if (typeof complete !== "function") return false;
    try {
      return complete.call(entry.bridge, msg.requestId >>> 0, msg.reportId >>> 0, new Uint8Array());
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`complete_feature_report_request (fail fallback) failed: ${message}`, msg.deviceId);
      return false;
    }
  }

  destroy(): void {
    for (const deviceId of Array.from(this.#bridges.keys())) {
      this.detach({ type: "hid.detach", deviceId });
    }
  }
}
