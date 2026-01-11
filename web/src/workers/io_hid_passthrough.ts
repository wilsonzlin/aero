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

/**
 * IO worker-side glue for managing WebHID passthrough devices.
 *
 * This is separated from `io.worker.ts` so it can be unit-tested without spinning up a real Worker
 * (or instantiating real WASM).
 */
export class IoWorkerHidPassthrough {
  readonly #wasm: WasmApi;
  readonly #postMessage: (msg: HidSendReportMessage) => void;
  readonly #devices = new Map<number, HidPassthroughBridge>();

  constructor(wasm: WasmApi, postMessage: (msg: HidSendReportMessage) => void) {
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
    bridge.push_input_report(msg.reportId, msg.data);
  }

  tick(): void {
    for (const [deviceId, bridge] of this.#devices) {
      if (!bridge.configured()) continue;
      while (true) {
        const next = bridge.drain_next_output_report() as unknown;
        if (next === null) break;
        if (!isOutputReport(next)) break;
        this.#postMessage({
          type: "hid.sendReport",
          deviceId,
          reportType: next.reportType,
          reportId: next.reportId,
          data: next.data as Uint8Array<ArrayBuffer>,
        });
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
