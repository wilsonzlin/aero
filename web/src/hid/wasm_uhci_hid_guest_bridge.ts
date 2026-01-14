import type { GuestUsbPath, GuestUsbPort } from "../platform/hid_passthrough_protocol";
import { EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort } from "../usb/uhci_external_hub";

import type { HidAttachMessage, HidDetachMessage, HidFeatureReportResultMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { HidGuestBridge, HidHostSink } from "./wasm_hid_guest_bridge";

const MAX_HID_OUTPUT_REPORTS_PER_TICK = 64;
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;
const MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK = 16;

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
  /**
   * Complete a guest `GET_REPORT (Feature)` request.
   *
   * API compatibility notes (older ↔ newer WASM builds):
   *
   * - Newer builds: `webhid_complete_feature_report_request(deviceId, requestId, reportId, ok, data?) -> boolean`
   * - Older builds: `webhid_complete_feature_report_request(deviceId, requestId, reportId, data) -> void` +
   *   optional `webhid_fail_feature_report_request(deviceId, requestId, reportId) -> void`
   */
  webhid_complete_feature_report_request?:
    | ((deviceId: number, requestId: number, reportId: number, ok: boolean, data?: Uint8Array) => boolean)
    | ((deviceId: number, requestId: number, reportId: number, data: Uint8Array) => void);
  webhid_fail_feature_report_request?(deviceId: number, requestId: number, reportId: number): void;

  /**
   * Legacy completion API (pre `webhid_complete_feature_report_request`) used by older WASM builds.
   */
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
  // don't clobber the built-in synthetic HID devices.
  return [EXTERNAL_HUB_ROOT_PORT, remapLegacyRootPortToExternalHubPort(rootPort)];
}

export class WasmUhciHidGuestBridge implements HidGuestBridge {
  readonly #uhci: UhciRuntimeHidApi;
  readonly #host: HidHostSink;
  readonly #attached = new Set<number>();
  #pendingFeatureRequests: Array<{ deviceId: number; requestId: number; reportId: number }> = [];
  #pendingFeatureRequestsHead = 0;

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
    const completeFeatureRequest = this.#uhci.webhid_complete_feature_report_request;
    const legacyComplete = this.#uhci.webhid_push_feature_report_result;
    // The underlying runtime drain is destructive; only attempt it when supported and when we can
    // complete requests.
    if (
      typeof drainFeatureRequests !== "function" ||
      (typeof completeFeatureRequest !== "function" && typeof legacyComplete !== "function")
    )
      return;

    let drainedFeature: Array<{ deviceId: number; requestId: number; reportId: number }> | null = null;
    try {
      drainedFeature = drainFeatureRequests.call(this.#uhci);
    } catch {
      return;
    }

    if (Array.isArray(drainedFeature) && drainedFeature.length) {
      // `webhid_drain_feature_report_requests` is destructive; buffer results so we can enforce a
      // per-tick cap without losing guest requests.
      if (this.#pendingFeatureRequestsHead >= this.#pendingFeatureRequests.length) {
        this.#pendingFeatureRequests = [];
        this.#pendingFeatureRequestsHead = 0;
      }
      this.#pendingFeatureRequests.push(...drainedFeature);
    }

    let remainingFeature = MAX_HID_FEATURE_REPORT_REQUESTS_PER_TICK;
    while (remainingFeature > 0) {
      if (this.#pendingFeatureRequestsHead >= this.#pendingFeatureRequests.length) {
        this.#pendingFeatureRequests = [];
        this.#pendingFeatureRequestsHead = 0;
        break;
      }
      remainingFeature -= 1;
      const req = this.#pendingFeatureRequests[this.#pendingFeatureRequestsHead]!;
      this.#pendingFeatureRequestsHead += 1;
      this.#host.requestFeatureReport({
        deviceId: req.deviceId >>> 0,
        requestId: req.requestId >>> 0,
        reportId: req.reportId >>> 0,
      });
    }
  }

  completeFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; data: Uint8Array }): boolean {
    try {
      const complete = this.#uhci.webhid_complete_feature_report_request;
      const fail = this.#uhci.webhid_fail_feature_report_request;
      if (typeof complete === "function") {
        // Split API (success+fail are separate): complete(deviceId, requestId, reportId, data)
        if (typeof fail === "function" || complete.length === 4) {
          (complete as (deviceId: number, requestId: number, reportId: number, data: Uint8Array) => void).call(
            this.#uhci,
            msg.deviceId >>> 0,
            msg.requestId >>> 0,
            msg.reportId >>> 0,
            msg.data,
          );
          return true;
        }

        // Unified API: complete(deviceId, requestId, reportId, ok, data?) -> boolean
        const res = (complete as (deviceId: number, requestId: number, reportId: number, ok: boolean, data?: Uint8Array) => boolean).call(
          this.#uhci,
          msg.deviceId >>> 0,
          msg.requestId >>> 0,
          msg.reportId >>> 0,
          true,
          msg.data,
        );
        return typeof res === "boolean" ? res : true;
      }
      const legacy = this.#uhci.webhid_push_feature_report_result;
      if (typeof legacy === "function") {
        legacy.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, true, msg.data);
        return true;
      }
      return false;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime completeFeatureReportRequest failed: ${message}`, msg.deviceId);
      return false;
    }
  }

  failFeatureReportRequest(msg: { deviceId: number; requestId: number; reportId: number; error?: string }): boolean {
    try {
      const fail = this.#uhci.webhid_fail_feature_report_request;
      if (typeof fail === "function") {
        fail.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0);
        return true;
      }

      const complete = this.#uhci.webhid_complete_feature_report_request;
      if (typeof complete === "function") {
        // Unified API (ok flag), or a runtime that doesn't provide a separate `fail` entrypoint.
        // Best-effort: attempt the unified call shape and fall back to legacy helpers if it throws.
        try {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const res = (complete as any).call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, false);
          return typeof res === "boolean" ? res : true;
        } catch {
          // Fall through to legacy helpers (if any).
        }
      }
      const legacy = this.#uhci.webhid_push_feature_report_result;
      if (typeof legacy === "function") {
        legacy.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, false);
        return true;
      }
      return false;
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
