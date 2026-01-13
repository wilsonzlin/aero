import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import type { WasmApi } from "../runtime/wasm_context";

import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage } from "./hid_proxy_protocol";

export type HidTopologyManager = {
  attachDevice(deviceId: number, path: GuestUsbPath, kind: "webhid" | "usb-hid-passthrough", device: unknown): void;
  detachDevice(deviceId: number): void;
};

export type HidHostSink = {
  sendReport: (msg: { deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }) => void;
  log: (message: string, deviceId?: number) => void;
  error: (message: string, deviceId?: number) => void;
};

export interface HidGuestBridge {
  attach(msg: HidAttachMessage): void;
  detach(msg: HidDetachMessage): void;
  inputReport(msg: HidInputReportMessage): void;
  poll?(): void;
  destroy?(): void;
}

const MAX_HID_OUTPUT_REPORTS_PER_TICK = 64;
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;

type WebHidPassthroughBridge = InstanceType<WasmApi["WebHidPassthroughBridge"]>;
type UsbHidPassthroughBridge = InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>;
type HidPassthroughBridge = WebHidPassthroughBridge | UsbHidPassthroughBridge;

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

export class WasmHidGuestBridge implements HidGuestBridge {
  readonly #bridges = new Map<number, HidPassthroughBridge>();

  constructor(
    private readonly api: WasmApi,
    private readonly host: HidHostSink,
    private readonly topology: HidTopologyManager,
  ) {}

  attach(msg: HidAttachMessage): void {
    this.detach({ type: "hid.detach", deviceId: msg.deviceId });
    const guestPathHint = msg.guestPath ?? (msg.guestPort !== undefined ? ([msg.guestPort] as GuestUsbPath) : undefined);
    const guestPath = guestPathHint;

    try {
      const UsbBridge = this.api.UsbHidPassthroughBridge;
      const synthesize = this.api.synthesize_webhid_report_descriptor;

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
        bridge = new this.api.WebHidPassthroughBridge(
          msg.vendorId,
          msg.productId,
          undefined,
          msg.productName,
          undefined,
          msg.collections,
        );
      }

      this.#bridges.set(msg.deviceId, bridge);
      if (guestPath) {
        this.topology.attachDevice(msg.deviceId, guestPath, kind, bridge);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.host.error(`Failed to construct WebHID passthrough bridge: ${message}`, msg.deviceId);
      return;
    }
  }

  detach(msg: HidDetachMessage): void {
    this.topology.detachDevice(msg.deviceId);
    const existing = this.#bridges.get(msg.deviceId);
    if (!existing) return;

    this.#bridges.delete(msg.deviceId);
    try {
      existing.free();
    } catch {
      // ignore
    }
  }

  inputReport(msg: HidInputReportMessage): void {
    const bridge = this.#bridges.get(msg.deviceId);
    if (!bridge) return;
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
      bridge.push_input_report(msg.reportId, data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.host.error(`WebHID push_input_report failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    let remainingReports = MAX_HID_OUTPUT_REPORTS_PER_TICK;
    for (const [deviceId, bridge] of this.#bridges) {
      if (remainingReports <= 0) return;
      let configured = false;
      try {
        configured = bridge.configured();
      } catch {
        configured = false;
      }
      if (!configured) continue;

      while (remainingReports > 0) {
        let report: { reportType: "output" | "feature"; reportId: number; data: Uint8Array } | null = null;
        try {
          report = bridge.drain_next_output_report();
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          this.host.error(`drain_next_output_report failed: ${message}`, deviceId);
          break;
        }
        if (!report) break;

        remainingReports -= 1;
        this.host.sendReport({
          deviceId,
          reportType: report.reportType,
          reportId: report.reportId,
          data: report.data,
        });
      }
    }
  }

  destroy(): void {
    for (const deviceId of Array.from(this.#bridges.keys())) {
      this.detach({ type: "hid.detach", deviceId });
    }
  }
}
