import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import type { WasmApi } from "../runtime/wasm_context";

import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import { UhciHidTopologyManager } from "./uhci_hid_topology";

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

type WebHidPassthroughBridge = InstanceType<WasmApi["WebHidPassthroughBridge"]>;
type UsbHidPassthroughBridge = InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>;
type HidPassthroughBridge = WebHidPassthroughBridge | UsbHidPassthroughBridge;

export class WasmHidGuestBridge implements HidGuestBridge {
  readonly #bridges = new Map<number, HidPassthroughBridge>();

  constructor(
    private readonly api: WasmApi,
    private readonly host: HidHostSink,
    private readonly uhciTopology: UhciHidTopologyManager,
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
        bridge = new UsbBridge(
          msg.vendorId,
          msg.productId,
          undefined,
          msg.productName,
          undefined,
          reportDescriptorBytes,
          msg.hasInterruptOut,
          undefined,
          undefined,
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
        this.uhciTopology.attachDevice(msg.deviceId, guestPath, kind, bridge);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.host.error(`Failed to construct WebHID passthrough bridge: ${message}`, msg.deviceId);
      return;
    }
  }

  detach(msg: HidDetachMessage): void {
    this.uhciTopology.detachDevice(msg.deviceId);
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
      bridge.push_input_report(msg.reportId, msg.data);
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
