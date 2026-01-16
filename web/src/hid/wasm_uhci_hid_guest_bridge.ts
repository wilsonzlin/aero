import type { GuestUsbPath, GuestUsbPort } from "../platform/hid_passthrough_protocol";
import {
  EXTERNAL_HUB_ROOT_PORT,
  WEBUSB_GUEST_ROOT_PORT,
  remapLegacyRootPortToExternalHubPort,
} from "../usb/uhci_external_hub";

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
   * Legacy completion API (pre `webhid_complete_feature_report_request` / `webhid_fail_feature_report_request`)
   * used by older WASM builds.
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
  if (rootPort !== EXTERNAL_HUB_ROOT_PORT && rootPort !== WEBUSB_GUEST_ROOT_PORT) return null;
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

  readonly #webhidAttach: UhciRuntimeHidApi["webhid_attach"];
  readonly #webhidAttachAtPath: UhciRuntimeHidApi["webhid_attach_at_path"] | null;
  readonly #webhidDetach: UhciRuntimeHidApi["webhid_detach"];
  readonly #webhidPushInputReport: UhciRuntimeHidApi["webhid_push_input_report"];
  readonly #webhidDrainOutputReports: UhciRuntimeHidApi["webhid_drain_output_reports"];
  readonly #webhidDrainFeatureReportRequests: UhciRuntimeHidApi["webhid_drain_feature_report_requests"] | null;
  readonly #webhidCompleteFeatureReportRequest: UhciRuntimeHidApi["webhid_complete_feature_report_request"] | null;
  readonly #webhidFailFeatureReportRequest: UhciRuntimeHidApi["webhid_fail_feature_report_request"] | null;
  readonly #webhidPushFeatureReportResult: UhciRuntimeHidApi["webhid_push_feature_report_result"] | null;

  constructor(opts: { uhci: UhciRuntimeHidApi; host: HidHostSink }) {
    this.#uhci = opts.uhci;
    this.#host = opts.host;

    const uhciAny = opts.uhci as unknown as Record<string, unknown>;
    const attach = uhciAny.webhid_attach ?? uhciAny.webhidAttach;
    const attachAtPath = uhciAny.webhid_attach_at_path ?? uhciAny.webhidAttachAtPath;
    const detach = uhciAny.webhid_detach ?? uhciAny.webhidDetach;
    const pushInput = uhciAny.webhid_push_input_report ?? uhciAny.webhidPushInputReport;
    const drainOutput = uhciAny.webhid_drain_output_reports ?? uhciAny.webhidDrainOutputReports;
    const drainFeature = uhciAny.webhid_drain_feature_report_requests ?? uhciAny.webhidDrainFeatureReportRequests;
    const completeFeature = uhciAny.webhid_complete_feature_report_request ?? uhciAny.webhidCompleteFeatureReportRequest;
    const failFeature = uhciAny.webhid_fail_feature_report_request ?? uhciAny.webhidFailFeatureReportRequest;
    const legacyFeature = uhciAny.webhid_push_feature_report_result ?? uhciAny.webhidPushFeatureReportResult;

    if (typeof attach !== "function") throw new Error("UHCI runtime missing `webhid_attach` (or `webhidAttach`).");
    if (typeof detach !== "function") throw new Error("UHCI runtime missing `webhid_detach` (or `webhidDetach`).");
    if (typeof pushInput !== "function")
      throw new Error("UHCI runtime missing `webhid_push_input_report` (or `webhidPushInputReport`).");
    if (typeof drainOutput !== "function")
      throw new Error("UHCI runtime missing `webhid_drain_output_reports` (or `webhidDrainOutputReports`).");

    this.#webhidAttach = attach as UhciRuntimeHidApi["webhid_attach"];
    this.#webhidAttachAtPath = typeof attachAtPath === "function" ? (attachAtPath as UhciRuntimeHidApi["webhid_attach_at_path"]) : null;
    this.#webhidDetach = detach as UhciRuntimeHidApi["webhid_detach"];
    this.#webhidPushInputReport = pushInput as UhciRuntimeHidApi["webhid_push_input_report"];
    this.#webhidDrainOutputReports = drainOutput as UhciRuntimeHidApi["webhid_drain_output_reports"];
    this.#webhidDrainFeatureReportRequests =
      typeof drainFeature === "function" ? (drainFeature as UhciRuntimeHidApi["webhid_drain_feature_report_requests"]) : null;
    this.#webhidCompleteFeatureReportRequest =
      typeof completeFeature === "function"
        ? (completeFeature as UhciRuntimeHidApi["webhid_complete_feature_report_request"])
        : null;
    this.#webhidFailFeatureReportRequest =
      typeof failFeature === "function" ? (failFeature as UhciRuntimeHidApi["webhid_fail_feature_report_request"]) : null;
    this.#webhidPushFeatureReportResult =
      typeof legacyFeature === "function" ? (legacyFeature as UhciRuntimeHidApi["webhid_push_feature_report_result"]) : null;
  }

  attach(msg: HidAttachMessage): void {
    this.detach({ type: "hid.detach", deviceId: msg.deviceId });

    const preferredPort = normalizePreferredPort(msg.guestPath, msg.guestPort);
    try {
      const attachAtPath = this.#webhidAttachAtPath;
      if (typeof attachAtPath === "function") {
        const normalizedPath = normalizeExternalHubGuestPath(msg.guestPath, msg.guestPort);
        if (normalizedPath) {
          attachAtPath.call(
            this.#uhci,
            msg.deviceId >>> 0,
            msg.vendorId >>> 0,
            msg.productId >>> 0,
            msg.productName,
            msg.collections,
            normalizedPath,
          );
        } else {
          this.#webhidAttach.call(
            this.#uhci,
            msg.deviceId >>> 0,
            msg.vendorId >>> 0,
            msg.productId >>> 0,
            msg.productName,
            msg.collections,
            preferredPort,
          );
        }
      } else {
        this.#webhidAttach.call(
          this.#uhci,
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
      const detail =
        (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true &&
        err instanceof Error &&
        typeof err.stack === "string" &&
        err.stack.length
          ? `${message}\n${err.stack}`
          : message;
      this.#host.error(`UHCI runtime hid.attach failed: ${detail}`, msg.deviceId);
    }
  }

  detach(msg: HidDetachMessage): void {
    this.#attached.delete(msg.deviceId);
    try {
      this.#webhidDetach.call(this.#uhci, msg.deviceId >>> 0);
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
      this.#webhidPushInputReport.call(this.#uhci, msg.deviceId >>> 0, msg.reportId >>> 0, data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.#host.error(`UHCI runtime hid.inputReport failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    let drained: Array<{ deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }> | null = null;
    try {
      drained = this.#webhidDrainOutputReports.call(this.#uhci);
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

    const drainFeatureRequests = this.#webhidDrainFeatureReportRequests;
    const completeFeatureRequest = this.#webhidCompleteFeatureReportRequest;
    const legacyComplete = this.#webhidPushFeatureReportResult;
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
      const complete = this.#webhidCompleteFeatureReportRequest;
      const fail = this.#webhidFailFeatureReportRequest;
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
      const legacy = this.#webhidPushFeatureReportResult;
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
      const fail = this.#webhidFailFeatureReportRequest;
      if (typeof fail === "function") {
        fail.call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0);
        return true;
      }

      const complete = this.#webhidCompleteFeatureReportRequest;
      if (typeof complete === "function") {
        // Unified API (ok flag), or a runtime that doesn't provide a separate `fail` entrypoint.
        // Best-effort: attempt the unified call shape and fall back to legacy helpers if it throws.
        try {
          const res = (
            complete as (deviceId: number, requestId: number, reportId: number, ok: boolean, data?: Uint8Array) => boolean
          ).call(this.#uhci, msg.deviceId >>> 0, msg.requestId >>> 0, msg.reportId >>> 0, false);
          return typeof res === "boolean" ? res : true;
        } catch {
          // Fall through to legacy helpers (if any).
        }
      }
      const legacy = this.#webhidPushFeatureReportResult;
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
