import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage, HidSendReportMessage } from "../hid/hid_proxy_protocol";
import type { WasmApi } from "../runtime/wasm_context";

type WebHidPassthroughBridgeCtor = WasmApi["WebHidPassthroughBridge"];
type WebHidPassthroughBridge = InstanceType<NonNullable<WebHidPassthroughBridgeCtor>>;
type UsbHidPassthroughBridgeCtor = WasmApi["UsbHidPassthroughBridge"];
type UsbHidPassthroughBridge = InstanceType<NonNullable<UsbHidPassthroughBridgeCtor>>;
type HidPassthroughBridge = WebHidPassthroughBridge | UsbHidPassthroughBridge;

type OutputReportResult = ReturnType<WebHidPassthroughBridge["drain_next_output_report"]>;

function isOutputReport(value: unknown): value is NonNullable<OutputReportResult> {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return (
    (v.reportType === "output" || v.reportType === "feature") &&
    typeof v.reportId === "number" &&
    v.data instanceof Uint8Array
  );
}

const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;
const MAX_HID_CONTROL_TRANSFER_BYTES = 0xffff;

function maxHidControlPayloadBytes(reportId: number): number {
  // USB control transfers have a u16 `wLength`. When `reportId != 0` the on-wire report includes a
  // 1-byte reportId prefix, so the payload must be <= 0xfffe.
  return (reportId >>> 0) === 0 ? MAX_HID_CONTROL_TRANSFER_BYTES : MAX_HID_CONTROL_TRANSFER_BYTES - 1;
}

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

/**
 * IO worker-side glue for managing WebHID passthrough devices.
 *
 * This is separated from `io.worker.ts` so it can be unit-tested without spinning up a real Worker
 * (or instantiating real WASM).
 */
export class IoWorkerHidPassthrough {
  readonly #wasm: WasmApi;
  readonly #postMessage: (msg: HidSendReportMessage, transfer: Transferable[]) => void;
  readonly #devices = new Map<number, HidPassthroughBridge>();

  constructor(wasm: WasmApi, postMessage: (msg: HidSendReportMessage, transfer: Transferable[]) => void) {
    this.#wasm = wasm;
    this.#postMessage = postMessage;
  }

  attach(msg: HidAttachMessage): void {
    const UsbBridge = this.#wasm.UsbHidPassthroughBridge;
    const synthesize = this.#wasm.synthesize_webhid_report_descriptor;
    const WebBridge = this.#wasm.WebHidPassthroughBridge;

    // Replace any existing handle for this ID.
    const prev = this.#devices.get(msg.deviceId);
    if (prev) prev.free();

    let bridge: HidPassthroughBridge;
    if (UsbBridge && synthesize) {
      const reportDescriptorBytes = synthesize(msg.collections);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      bridge = new (UsbBridge as any)(
        msg.vendorId,
        msg.productId,
        undefined,
        msg.productName,
        undefined,
        reportDescriptorBytes,
        msg.hasInterruptOut,
        undefined,
        undefined,
      ) as UsbHidPassthroughBridge;
    } else {
      if (typeof WebBridge !== "function") {
        throw new Error("WASM export WebHidPassthroughBridge is unavailable.");
      }
      // wasm-bindgen doesn't expose overloads; pass explicit args matching the Rust constructor:
      // (vendorId, productId, manufacturer?, product?, serial?, collectionsJson).
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      bridge = new (WebBridge as any)(
        msg.vendorId,
        msg.productId,
        undefined,
        msg.productName,
        undefined,
        msg.collections,
      ) as WebHidPassthroughBridge;
    }

    this.#devices.set(msg.deviceId, bridge);
  }

  detach(msg: HidDetachMessage): void {
    const bridge = this.#devices.get(msg.deviceId);
    if (!bridge) return;
    bridge.free();
    this.#devices.delete(msg.deviceId);
  }

  inputReport(msg: HidInputReportMessage): void {
    const bridge = this.#devices.get(msg.deviceId);
    if (!bridge) return;
    const data = (() => {
      if (msg.data.byteLength <= MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) return msg.data;
      const out = new Uint8Array(MAX_HID_INPUT_REPORT_PAYLOAD_BYTES);
      out.set(msg.data.subarray(0, MAX_HID_INPUT_REPORT_PAYLOAD_BYTES));
      return out as Uint8Array<ArrayBuffer>;
    })();
    bridge.push_input_report(msg.reportId, data);
  }

  tick(): void {
    for (const [deviceId, bridge] of this.#devices) {
      if (!bridge.configured()) continue;
      while (true) {
        const next = bridge.drain_next_output_report() as unknown;
        if (next === null) break;
        if (!isOutputReport(next)) break;

        const reportId = next.reportId >>> 0;
        const maxPayloadBytes = maxHidControlPayloadBytes(reportId);
        const clamped = next.data.byteLength > maxPayloadBytes ? next.data.subarray(0, maxPayloadBytes) : next.data;
        const data = toTransferableArrayBufferBacked(clamped);
        const msg: HidSendReportMessage = {
          type: "hid.sendReport",
          deviceId,
          reportType: next.reportType,
          reportId,
          data,
        };
        this.#postMessage(msg, [data.buffer]);
      }
    }
  }

  destroy(): void {
    for (const bridge of this.#devices.values()) {
      bridge.free();
    }
    this.#devices.clear();
  }
}
