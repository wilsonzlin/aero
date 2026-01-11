export const HID_INPUT_REPORT_RECORD_HEADER_BYTES = 12;

export type HidInputReportRingRecord = Readonly<{
  deviceId: number;
  reportId: number;
  tsMs: number;
  data: Uint8Array;
}>;

function toU32(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.floor(value)) >>> 0;
}

export function encodeHidInputReportRingRecord(opts: {
  deviceId: number;
  reportId: number;
  tsMs?: number;
  data: Uint8Array;
}): Uint8Array {
  const headerBytes = HID_INPUT_REPORT_RECORD_HEADER_BYTES;
  const out = new Uint8Array(headerBytes + opts.data.byteLength);
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint32(0, opts.deviceId >>> 0, true);
  view.setUint32(4, opts.reportId >>> 0, true);
  view.setUint32(8, toU32(opts.tsMs ?? 0), true);
  out.set(opts.data, headerBytes);
  return out;
}

export function decodeHidInputReportRingRecord(payload: Uint8Array): HidInputReportRingRecord | null {
  if (payload.byteLength < HID_INPUT_REPORT_RECORD_HEADER_BYTES) return null;
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const deviceId = view.getUint32(0, true);
  const reportId = view.getUint32(4, true);
  const tsMs = view.getUint32(8, true);
  return {
    deviceId,
    reportId,
    tsMs,
    data: payload.subarray(HID_INPUT_REPORT_RECORD_HEADER_BYTES),
  };
}

