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

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

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
    const payload = ring.tryPop();
    if (!payload) break;
    bytes += payload.byteLength;

    const record = decodeHidInputReportRingRecord(payload);
    if (!record) {
      invalid += 1;
      continue;
    }

    onReport({
      type: "hid.inputReport",
      deviceId: record.deviceId,
      reportId: record.reportId,
      data: ensureArrayBufferBacked(record.data),
      tsMs: record.tsMs,
    });
    forwarded += 1;
  }

  return { forwarded, invalid, bytes };
}
