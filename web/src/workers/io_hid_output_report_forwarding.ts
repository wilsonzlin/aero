import type { HidSendReportMessage } from "../hid/hid_proxy_protocol";
import { ensureArrayBufferBacked } from "../hid/in_memory_hid_guest_bridge";
import type { HidHostSink } from "../hid/wasm_hid_guest_bridge";
import { HidReportRing, HidReportType as HidRingReportType } from "../usb/hid_report_ring";

export type HidHostSendReportPayload = Parameters<HidHostSink["sendReport"]>[0];

export type HidSendReportForwardingResult =
  | { path: "ring" }
  | {
      path: "postMessage";
      /**
       * True when the output SharedArrayBuffer ring was present, but could not accept the report
       * (full or report too large).
       */
      ringFailed: boolean;
    };

export function forwardHidSendReportToMainThread(
  payload: HidHostSendReportPayload,
  opts: {
    outputRing: HidReportRing | null;
    postMessage: (msg: HidSendReportMessage, transfer: Transferable[]) => void;
  },
): HidSendReportForwardingResult {
  const ring = opts.outputRing;
  if (ring) {
    const ty = payload.reportType === "feature" ? HidRingReportType.Feature : HidRingReportType.Output;
    const ok = ring.push(payload.deviceId >>> 0, ty, payload.reportId >>> 0, payload.data);
    if (ok) return { path: "ring" };
  }

  const data = ensureArrayBufferBacked(payload.data);
  const outputRingTail = (() => {
    if (!ring) return undefined;
    try {
      return ring.debugState().tail;
    } catch {
      return undefined;
    }
  })();
  const msg: HidSendReportMessage = {
    type: "hid.sendReport",
    ...payload,
    data,
    ...(outputRingTail !== undefined ? { outputRingTail } : {}),
  };
  opts.postMessage(msg, [data.buffer]);
  return { path: "postMessage", ringFailed: !!ring };
}
