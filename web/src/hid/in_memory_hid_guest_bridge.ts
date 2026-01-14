import type { HidAttachMessage, HidDetachMessage, HidInputReportMessage } from "./hid_proxy_protocol";
import type { HidGuestBridge, HidHostSink } from "./wasm_hid_guest_bridge";

const MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE = 256;
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;

// `import.meta.env` is provided by Vite in browser builds, but is undefined when running
// worker entrypoints directly under Node (e.g. worker_threads unit tests).
const IS_DEV = (import.meta as unknown as { env?: { DEV?: unknown } }).env?.DEV === true;

export function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // HID proxy messages transfer the underlying ArrayBuffer between threads.
  // If a view is backed by a SharedArrayBuffer, it can't be transferred; copy.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

export class InMemoryHidGuestBridge implements HidGuestBridge {
  readonly devices = new Map<number, HidAttachMessage>();
  readonly inputReports = new Map<number, HidInputReportMessage[]>();

  #inputCount = 0;
  #host: HidHostSink;

  constructor(host: HidHostSink) {
    this.#host = host;
  }

  attach(msg: HidAttachMessage): void {
    const isReattach = this.devices.has(msg.deviceId);
    this.devices.set(msg.deviceId, msg);
    // Treat re-attach as a new session; clear any buffered reports. If an input
    // report raced ahead of the attach message (possible with SharedArrayBuffer
    // ring fast paths), preserve the pre-attach buffer for first attach so it
    // can still be replayed once WASM initializes.
    if (isReattach || !this.inputReports.has(msg.deviceId)) {
      this.inputReports.set(msg.deviceId, []);
    }
    const pathHint = msg.guestPath ? ` path=${msg.guestPath.join(".")}` : msg.guestPort === undefined ? "" : ` port=${msg.guestPort}`;
    this.#host.log(
      `hid.attach deviceId=${msg.deviceId}${pathHint} vid=0x${msg.vendorId.toString(16).padStart(4, "0")} pid=0x${msg.productId.toString(16).padStart(4, "0")}`,
      msg.deviceId,
    );
  }

  detach(msg: HidDetachMessage): void {
    this.devices.delete(msg.deviceId);
    this.inputReports.delete(msg.deviceId);
    this.#host.log(`hid.detach deviceId=${msg.deviceId}`, msg.deviceId);
  }

  inputReport(msg: HidInputReportMessage): void {
    let queue = this.inputReports.get(msg.deviceId);
    if (!queue) {
      queue = [];
      this.inputReports.set(msg.deviceId, queue);
    }
    // `HidInputReportMessage.data` is normally ArrayBuffer-backed because it's
    // transferred over postMessage. Some fast paths (SharedArrayBuffer rings)
    // can deliver views backed by SharedArrayBuffer; copy those so buffered
    // reports remain valid after the ring memory is reused.
    const clamped = (() => {
      if (msg.data.byteLength <= MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) return msg.data;
      // Defensive clamp: avoid copying arbitrarily large reports into the in-memory buffer
      // (or into the WASM bridge once replayed). Valid full-speed HID interrupt reports are
      // capped at 64 bytes of payload.
      const out = new Uint8Array(MAX_HID_INPUT_REPORT_PAYLOAD_BYTES);
      out.set(msg.data.subarray(0, MAX_HID_INPUT_REPORT_PAYLOAD_BYTES));
      return out as Uint8Array<ArrayBuffer>;
    })();
    const data = ensureArrayBufferBacked(clamped);
    queue.push({ ...msg, data });
    if (queue.length > MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE) {
      queue.splice(0, queue.length - MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE);
    }

    this.#inputCount += 1;
    if (IS_DEV && (this.#inputCount & 0xff) === 0) {
      this.#host.log(
        `hid.inputReport deviceId=${msg.deviceId} reportId=${msg.reportId} bytes=${msg.data.byteLength}`,
        msg.deviceId,
      );
    }
  }
}
