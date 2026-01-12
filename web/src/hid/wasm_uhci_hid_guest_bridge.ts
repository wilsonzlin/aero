import type { GuestUsbPath, GuestUsbPort } from "../platform/hid_passthrough_protocol";
import { EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort } from "../usb/uhci_external_hub";

import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { HidGuestBridge, HidHostSink } from "./wasm_hid_guest_bridge";

const MAX_HID_OUTPUT_REPORTS_PER_TICK = 64;

export type UhciRuntimeHidApi = {
  webhid_attach(
    deviceId: number,
    vendorId: number,
    productId: number,
    productName: string | undefined,
    collectionsJson: unknown,
    preferredPort?: number,
  ): number;

  /**
   * Optional newer entrypoint that supports WebHID passthrough devices behind the external hub
   * topology (e.g. guest paths like `0.3`).
   */
  webhid_attach_at_path?(
    deviceId: number,
    vendorId: number,
    productId: number,
    productName: string | undefined,
    collectionsJson: unknown,
    guestPath: GuestUsbPath,
  ): void;

  webhid_detach(deviceId: number): void;
  webhid_push_input_report(deviceId: number, reportId: number, data: Uint8Array): void;
  webhid_drain_output_reports(): Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }>;
};

function normalizePreferredPort(path: GuestUsbPath | undefined, guestPort: GuestUsbPort | undefined): number | undefined {
  const preferredPort = path?.[0] ?? guestPort;
  return preferredPort === undefined ? undefined : (preferredPort >>> 0);
}

function normalizeExternalHubGuestPath(
  path: GuestUsbPath | undefined,
  guestPort: GuestUsbPort | undefined,
): GuestUsbPath | null {
  if (path && path.length >= 2) return path;

  const rootPort = path?.[0] ?? guestPort;
  if (rootPort !== 0 && rootPort !== 1) return null;
  // Backwards-compatible mapping for root-port-only hints: translate `[0]` / `[1]` onto stable
  // hub-backed paths behind root port 0. Offset by the reserved synthetic ports so legacy callers
  // don't clobber the built-in keyboard/mouse/gamepad devices.
  return [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(rootPort)];
}

export class WasmUhciHidGuestBridge implements HidGuestBridge {
  readonly #uhci: UhciRuntimeHidApi;
  readonly #host: HidHostSink;
  readonly #attached = new Set<number>();

  constructor(opts: { uhci: UhciRuntimeHidApi; host: HidHostSink }) {
    this.#uhci = opts.uhci;
    this.#host = opts.host;
  }

  attach(msg: HidAttachMessage): void {
    this.detach({ type: "hid.detach", deviceId: msg.deviceId });

      const preferredPort = normalizePreferredPort(msg.guestPath, msg.guestPort);
      try {
        if (typeof this.#uhci.webhid_attach_at_path === "function") {
        const normalizedPath = normalizeExternalHubGuestPath(msg.guestPath, msg.guestPort);
        if (normalizedPath) {
          this.#uhci.webhid_attach_at_path(
            msg.deviceId >>> 0,
            msg.vendorId >>> 0,
            msg.productId >>> 0,
            msg.productName,
            msg.collections,
            normalizedPath,
          );
        } else {
          this.#uhci.webhid_attach(
            msg.deviceId >>> 0,
            msg.vendorId >>> 0,
            msg.productId >>> 0,
            msg.productName,
            msg.collections,
            preferredPort,
          );
        }
      } else {
        this.#uhci.webhid_attach(
          msg.deviceId >>> 0,
          msg.vendorId >>> 0,
          msg.productId >>> 0,
          msg.productName,
          msg.collections,
          preferredPort,
        );
      }
      this.#attached.add(msg.deviceId);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime hid.attach failed: ${message}`, msg.deviceId);
    }
  }

  detach(msg: HidDetachMessage): void {
    this.#attached.delete(msg.deviceId);
    try {
      this.#uhci.webhid_detach(msg.deviceId >>> 0);
    } catch {
      // ignore
    }
  }

  inputReport(msg: HidInputReportMessage): void {
    try {
      this.#uhci.webhid_push_input_report(msg.deviceId >>> 0, msg.reportId >>> 0, msg.data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime hid.inputReport failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    let drained: Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }> = [];
    try {
      drained = this.#uhci.webhid_drain_output_reports();
    } catch {
      return;
    }

    if (!Array.isArray(drained) || drained.length === 0) return;
    let remaining = MAX_HID_OUTPUT_REPORTS_PER_TICK;
    for (const report of drained) {
      if (remaining <= 0) return;
      remaining -= 1;
      this.#host.sendReport({
        deviceId: report.deviceId >>> 0,
        reportType: report.reportType,
        reportId: report.reportId >>> 0,
        data: report.data,
      });
    }
  }

  destroy(): void {
    for (const deviceId of Array.from(this.#attached)) {
      this.detach({ type: "hid.detach", deviceId });
    }
  }
}
