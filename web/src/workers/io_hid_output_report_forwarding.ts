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

const MAX_HID_SEND_REPORT_PAYLOAD_BYTES = 0xffff;

function toTransferableArrayBufferBacked(view: Uint8Array): Uint8Array<ArrayBuffer> {
  // We transfer `data.buffer` across threads. Ensure the view's backing buffer is:
  // - an ArrayBuffer (not SharedArrayBuffer), AND
  // - tightly sized (so we don't transfer a huge underlying buffer for a small slice).
  if (view.buffer instanceof ArrayBuffer && view.byteOffset === 0 && view.byteLength === view.buffer.byteLength) {
    return view as unknown as Uint8Array<ArrayBuffer>;
  }
  const out = new Uint8Array(view.byteLength);
  out.set(view);
  return out;
}

export function forwardHidSendReportToMainThread(
  payload: HidHostSendReportPayload,
  opts: {
    outputRing: HidReportRing | null;
    postMessage: (msg: HidSendReportMessage, transfer: Transferable[]) => void;
  },
): HidSendReportForwardingResult {
  // Hard cap so a buggy/malicious guest can't trick us into copying/transferring absurdly large
  // output/feature report payloads. USB control transfers have a u16 `wLength`, so larger payloads
  // are not representable anyway.
  const clamped = payload.data.byteLength > MAX_HID_SEND_REPORT_PAYLOAD_BYTES ? payload.data.subarray(0, MAX_HID_SEND_REPORT_PAYLOAD_BYTES) : payload.data;

  const ring = opts.outputRing;
  if (ring) {
    const ty = payload.reportType === "feature" ? HidRingReportType.Feature : HidRingReportType.Output;
    const ok = ring.push(payload.deviceId >>> 0, ty, payload.reportId >>> 0, clamped);
    if (ok) return { path: "ring" };
  }

  // `ensureArrayBufferBacked` rejects SharedArrayBuffer-backed views, but it may still return a
  // slice into a much larger ArrayBuffer (e.g. views into WASM memory). Copy so the transferred
  // ArrayBuffer is bounded and does not detach/transfer unrelated memory.
  const data = toTransferableArrayBufferBacked(ensureArrayBufferBacked(clamped));
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
