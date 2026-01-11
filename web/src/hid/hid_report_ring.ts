import {
  HID_INPUT_REPORT_RECORD_HEADER_BYTES,
  HID_INPUT_REPORT_RECORD_MAGIC,
  HID_INPUT_REPORT_RECORD_VERSION,
  decodeHidInputReportRingRecord,
  encodeHidInputReportRingRecord,
} from "./hid_input_report_ring";

// Stable, versioned encoding for high-frequency WebHID input reports passed via
// a SharedArrayBuffer ring buffer.
//
// This module is a thin wrapper around `hid_input_report_ring.ts`, exposing an
// API aligned with the higher-level “input report record” concept.

export const HID_REPORT_RING_MAGIC = HID_INPUT_REPORT_RECORD_MAGIC;
export const HID_REPORT_RING_VERSION = HID_INPUT_REPORT_RECORD_VERSION;
export const HID_REPORT_RING_HEADER_BYTES = HID_INPUT_REPORT_RECORD_HEADER_BYTES;

export type HidInputReportRecord = Readonly<{
  deviceId: number;
  reportId: number;
  tsMs?: number;
  data: Uint8Array;
}>;

export function encodeInputReportRecord(
  deviceId: number,
  reportId: number,
  data: Uint8Array,
  tsMs?: number,
): Uint8Array {
  return encodeHidInputReportRingRecord({ deviceId, reportId, tsMs, data });
}

export function decodeInputReportRecord(bytes: Uint8Array): HidInputReportRecord {
  const record = decodeHidInputReportRingRecord(bytes);
  if (!record) {
    throw new Error("invalid HID input report record");
  }
  return {
    deviceId: record.deviceId,
    reportId: record.reportId,
    ...(record.tsMs !== 0 ? { tsMs: record.tsMs } : {}),
    data: record.data,
  };
}

