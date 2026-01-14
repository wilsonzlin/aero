import type { NormalizedHidCollectionInfo, NormalizedHidReportInfo } from "./webhid_normalize";

function reportBits(report: NormalizedHidReportInfo): number {
  let total = 0;
  for (const item of report.items) {
    total += item.reportSize * item.reportCount;
  }
  return total;
}

/**
 * Compute expected input report payload byte lengths for a WebHID device.
 *
 * The returned lengths exclude any reportId prefix byte, since WebHID surfaces
 * `reportId` separately from `HIDInputReportEvent.data`.
 *
 * Sizes are aggregated across collections: multiple input reports with the same
 * `reportId` contribute to the same payload length.
 */
export function computeInputReportPayloadByteLengths(
  collections: readonly NormalizedHidCollectionInfo[],
): Map<number, number> {
  const bitsByReportId = new Map<number, number>();
  const stack: NormalizedHidCollectionInfo[] = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    for (const report of node.inputReports) {
      const prev = bitsByReportId.get(report.reportId) ?? 0;
      bitsByReportId.set(report.reportId, prev + reportBits(report));
    }
    for (const child of node.children) stack.push(child);
  }

  const out = new Map<number, number>();
  for (const [reportId, bits] of bitsByReportId) {
    out.set(reportId, Math.ceil(bits / 8));
  }
  return out;
}

/**
 * Compute expected feature report payload byte lengths for a WebHID device.
 *
 * The returned lengths exclude any reportId prefix byte: WebHID callers provide the reportId
 * separately to `receiveFeatureReport(reportId)` / `sendFeatureReport(reportId, data)`.
 *
 * Sizes are aggregated across collections: multiple feature reports with the same `reportId`
 * contribute to the same payload length.
 */
export function computeFeatureReportPayloadByteLengths(
  collections: readonly NormalizedHidCollectionInfo[],
): Map<number, number> {
  const bitsByReportId = new Map<number, number>();
  const stack: NormalizedHidCollectionInfo[] = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    for (const report of node.featureReports) {
      const prev = bitsByReportId.get(report.reportId) ?? 0;
      bitsByReportId.set(report.reportId, prev + reportBits(report));
    }
    for (const child of node.children) stack.push(child);
  }

  const out = new Map<number, number>();
  for (const [reportId, bits] of bitsByReportId) {
    out.set(reportId, Math.ceil(bits / 8));
  }
  return out;
}

/**
 * Compute expected output report payload byte lengths for a WebHID device.
 *
 * The returned lengths exclude any reportId prefix byte: WebHID callers provide the reportId
 * separately to `sendReport(reportId, data)`.
 *
 * Sizes are aggregated across collections: multiple output reports with the same `reportId`
 * contribute to the same payload length.
 */
export function computeOutputReportPayloadByteLengths(
  collections: readonly NormalizedHidCollectionInfo[],
): Map<number, number> {
  const bitsByReportId = new Map<number, number>();
  const stack: NormalizedHidCollectionInfo[] = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    for (const report of node.outputReports) {
      const prev = bitsByReportId.get(report.reportId) ?? 0;
      bitsByReportId.set(report.reportId, prev + reportBits(report));
    }
    for (const child of node.children) stack.push(child);
  }

  const out = new Map<number, number>();
  for (const [reportId, bits] of bitsByReportId) {
    out.set(reportId, Math.ceil(bits / 8));
  }
  return out;
}

/**
 * Compute the maximum *on-wire* byte length of any output report defined by a WebHID device.
 *
 * The returned size includes the optional reportId prefix byte (when reportId != 0).
 *
 * Sizes are aggregated across collections: multiple output reports with the same `reportId`
 * contribute to the same on-wire report length.
 */
export function computeMaxOutputReportBytesOnWire(collections: readonly NormalizedHidCollectionInfo[]): number {
  const bitsByReportId = new Map<number, number>();
  const stack: NormalizedHidCollectionInfo[] = [...collections];
  while (stack.length) {
    const node = stack.pop()!;
    for (const report of node.outputReports) {
      const prev = bitsByReportId.get(report.reportId) ?? 0;
      bitsByReportId.set(report.reportId, prev + reportBits(report));
    }
    for (const child of node.children) stack.push(child);
  }

  let max = 0;
  for (const [reportId, bits] of bitsByReportId) {
    const dataBytes = Math.ceil(bits / 8);
    const onWireBytes = dataBytes + (reportId !== 0 ? 1 : 0);
    max = Math.max(max, onWireBytes);
  }
  return max;
}
