import type { GuestUsbPath, GuestUsbPort } from "../platform/hid_passthrough_protocol";
import { EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort } from "../usb/uhci_external_hub";

import type { HidAttachMessage, HidDetachMessage, HidFeatureReportResultMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { HidGuestBridge, HidHostSink } from "./wasm_hid_guest_bridge";

const MAX_HID_OUTPUT_REPORTS_PER_TICK = 64;
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;
const MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK = 64;

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
   * topology (e.g. guest paths like `0.5`).
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
  webhid_drain_output_reports(): Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }> | null;

  webhid_drain_feature_report_requests?(): Array<{ deviceId: number; requestId: number; reportId: number }> | null;
  webhid_push_feature_report_result?(
    deviceId: number,
    requestId: number,
    reportId: number,
    ok: boolean,
    data?: Uint8Array,
  ): void;
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
  readonly #pendingFeatureRequests: Array<{ deviceId: number; requestId: number; reportId: number }> = [];

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
      const data = (() => {
        if (msg.data.byteLength <= MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) return msg.data;
        // Defensive clamp: UHCI WebHID passthrough is modeled as full-speed interrupt transfers
        // (<= 64 bytes payload). Avoid copying oversized reports into WASM memory.
        const out = new Uint8Array(MAX_HID_INPUT_REPORT_PAYLOAD_BYTES);
        out.set(msg.data.subarray(0, MAX_HID_INPUT_REPORT_PAYLOAD_BYTES));
        return out as Uint8Array<ArrayBuffer>;
      })();
      this.#uhci.webhid_push_input_report(msg.deviceId >>> 0, msg.reportId >>> 0, data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime hid.inputReport failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    let drained: Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }> | null = null;
    try {
      drained = this.#uhci.webhid_drain_output_reports();
    } catch {
      return;
    }

    if (Array.isArray(drained) && drained.length) {
      let remaining = MAX_HID_OUTPUT_REPORTS_PER_TICK;
      for (const report of drained) {
        if (remaining <= 0) break;
        remaining -= 1;
        this.#host.sendReport({
          deviceId: report.deviceId >>> 0,
          reportType: report.reportType,
          reportId: report.reportId >>> 0,
          data: report.data,
        });
      }
    }

    const drainFeatureRequests = this.#uhci.webhid_drain_feature_report_requests;
    // The underlying runtime drain is destructive; only attempt it when supported.
    if (typeof drainFeatureRequests !== "function") return;

    let drainedFeature: Array<{ deviceId: number; requestId: number; reportId: number }> | null = null;
    try {
      drainedFeature = drainFeatureRequests.call(this.#uhci);
    } catch {
      return;
    }

    if (Array.isArray(drainedFeature) && drainedFeature.length) {
      this.#pendingFeatureRequests.push(...drainedFeature);
    }

    let remainingFeature = MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK;
    while (remainingFeature > 0 && this.#pendingFeatureRequests.length > 0) {
      remainingFeature -= 1;
      const req = this.#pendingFeatureRequests.shift()!;
      this.#host.requestFeatureReport({
        deviceId: req.deviceId >>> 0,
        requestId: req.requestId >>> 0,
        reportId: req.reportId >>> 0,
      });
    }
  }

  completeFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; data: Uint8Array }): boolean {
    const apply = this.#uhci.webhid_push_feature_report_result;
    if (typeof apply !== "function") return false;
    try {
      apply.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, true, msg.data);
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime completeFeatureReportRequest failed: ${message}`, msg.deviceId);
      return false;
    }
  }

  failFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; error?: string }): boolean {
    const apply = this.#uhci.webhid_push_feature_report_result;
    if (typeof apply !== "function") return false;
    try {
      apply.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, false);
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime failFeatureReportRequest failed: ${message}`, msg.deviceId);
      return false;
    }
  }

  featureReportResult(msg: HidFeatureReportResultMessage): void {
    if (msg.ok) {
      this.completeFeatureReportRequest({
        deviceId: msg.deviceId,
        requestId: msg.requestId,
        reportId: msg.reportId,
        data: msg.data ?? new Uint8Array(),
      });
    } else {
      this.failFeatureReportRequest({ deviceId: msg.deviceId, requestId: msg.requestId, reportId: msg.reportId, error: msg.error });
    }
  }

  destroy(): void {
    for (const deviceId of Array.from(this.#attached)) {
      this.detach({ type: "hid.detach", deviceId });
    }
  }
}
