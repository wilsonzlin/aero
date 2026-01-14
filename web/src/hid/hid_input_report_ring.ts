export const HID_INPUT_REPORT_RECORD_MAGIC = 0x5244_4948; // "HIDR" LE
export const HID_INPUT_REPORT_RECORD_VERSION = 1;

export const HID_INPUT_REPORT_RECORD_HEADER_BYTES = 24;
// WebHID passthrough models devices behind a full-speed USB controller (UHCI). Full-speed interrupt
// endpoints have a 64-byte max packet size, and HID input reports must fit within a single
// interrupt IN transaction.
const MAX_HID_INPUT_REPORT_PAYLOAD_BYTES = 64;

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

export function writeHidInputReportRingRecord(
  dest: Uint8Array,
  opts: {
    deviceId: number;
    reportId: number;
    tsMs?: number;
    data: Uint8Array;
  },
): void {
  const headerBytes = HID_INPUT_REPORT_RECORD_HEADER_BYTES;
  if (
    typeof opts.reportId !== "number" ||
    !Number.isInteger(opts.reportId) ||
    opts.reportId < 0 ||
    opts.reportId > 0xff
  ) {
    throw new Error(`invalid reportId: ${String(opts.reportId)}`);
  }
  const dataLen = opts.data.byteLength >>> 0;
  if (dataLen > MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) {
    throw new Error(`input report payload too large (max ${MAX_HID_INPUT_REPORT_PAYLOAD_BYTES}, got ${dataLen})`);
  }
  if (dest.byteLength !== headerBytes + dataLen) {
    throw new Error(`dest length mismatch (expected ${headerBytes + dataLen}, got ${dest.byteLength})`);
  }
  const view = new DataView(dest.buffer, dest.byteOffset, dest.byteLength);
  view.setUint32(0, HID_INPUT_REPORT_RECORD_MAGIC, true);
  view.setUint32(4, HID_INPUT_REPORT_RECORD_VERSION, true);
  view.setUint32(8, opts.deviceId >>> 0, true);
  view.setUint32(12, opts.reportId, true);
  view.setUint32(16, toU32(opts.tsMs ?? 0), true);
  view.setUint32(20, dataLen, true);
  dest.set(opts.data, headerBytes);
}

export function encodeHidInputReportRingRecord(opts: {
  deviceId: number;
  reportId: number;
  tsMs?: number;
  data: Uint8Array;
}): Uint8Array {
  const headerBytes = HID_INPUT_REPORT_RECORD_HEADER_BYTES;
  const dataLen = opts.data.byteLength >>> 0;
  const out = new Uint8Array(headerBytes + dataLen);
  writeHidInputReportRingRecord(out, opts);
  return out;
}

export function decodeHidInputReportRingRecord(payload: Uint8Array): HidInputReportRingRecord | null {
  if (payload.byteLength < HID_INPUT_REPORT_RECORD_HEADER_BYTES) return null;
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const magic = view.getUint32(0, true);
  if (magic !== HID_INPUT_REPORT_RECORD_MAGIC) return null;
  const version = view.getUint32(4, true);
  if (version !== HID_INPUT_REPORT_RECORD_VERSION) return null;
  const deviceId = view.getUint32(8, true);
  const reportId = view.getUint32(12, true);
  const tsMs = view.getUint32(16, true);
  const len = view.getUint32(20, true) >>> 0;
  if (reportId > 0xff) return null;
  if (len > MAX_HID_INPUT_REPORT_PAYLOAD_BYTES) return null;
  if (payload.byteLength !== HID_INPUT_REPORT_RECORD_HEADER_BYTES + len) return null;
  return {
    deviceId,
    reportId,
    tsMs,
    data: payload.subarray(HID_INPUT_REPORT_RECORD_HEADER_BYTES),
  };
}
