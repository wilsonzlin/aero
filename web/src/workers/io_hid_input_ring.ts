import type { HidInputReportMessage } from "../hid/hid_proxy_protocol";
import { decodeHidInputReportRingRecord } from "../hid/hid_input_report_ring";
import type { RingBuffer } from "../ipc/ring_buffer";

export const IO_HID_INPUT_RING_MAX_RECORDS_PER_TICK = 256;
export const IO_HID_INPUT_RING_MAX_BYTES_PER_TICK = 64 * 1024;

export type IoHidInputRingDrainResult = Readonly<{
  forwarded: number;
  invalid: number;
  bytes: number;
}>;

export function drainIoHidInputRing(
  ring: RingBuffer,
  onReport: (msg: HidInputReportMessage) => void,
  opts: { maxRecords?: number; maxBytes?: number } = {},
): IoHidInputRingDrainResult {
  const maxRecords = Math.max(0, opts.maxRecords ?? IO_HID_INPUT_RING_MAX_RECORDS_PER_TICK);
  const maxBytes = Math.max(0, opts.maxBytes ?? IO_HID_INPUT_RING_MAX_BYTES_PER_TICK);

  let forwarded = 0;
  let invalid = 0;
  let bytes = 0;

  while (forwarded + invalid < maxRecords && bytes < maxBytes) {
    const consumed = ring.consumeNext((payload) => {
      bytes += payload.byteLength;

      const record = decodeHidInputReportRingRecord(payload);
      if (!record) {
        invalid += 1;
        return;
      }

      try {
        onReport({
          type: "hid.inputReport",
          deviceId: record.deviceId,
          reportId: record.reportId,
          // SAB-backed views can't be transferred over postMessage, but the guest-side
          // WASM bridge accepts Uint8Array views regardless of buffer type. If a caller
          // needs to retain the data beyond this synchronous callback, it must copy it.
          data: record.data as unknown as Uint8Array<ArrayBuffer>,
          ...(record.tsMs !== 0 ? { tsMs: record.tsMs } : {}),
        });
        forwarded += 1;
      } catch {
        // Treat consumer errors as dropped records so the ring doesn't wedge.
        invalid += 1;
      }
    });
    if (!consumed) break;
  }

  return { forwarded, invalid, bytes };
}
