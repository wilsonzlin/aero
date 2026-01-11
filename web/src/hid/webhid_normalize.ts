export type HidCollectionType =
  | "physical"
  | "application"
  | "logical"
  | "report"
  | "namedArray"
  | "usageSwitch"
  | "usageModifier";

export interface HidReportItem {
  usagePage: number;
  usages: readonly number[];
  usageMinimum: number;
  usageMaximum: number;
  reportSize: number;
  reportCount: number;
  unitExponent: number;
  unit: number;
  logicalMinimum: number;
  logicalMaximum: number;
  physicalMinimum: number;
  physicalMaximum: number;
  strings: readonly number[];
  stringMinimum: number;
  stringMaximum: number;
  designators: readonly number[];
  designatorMinimum: number;
  designatorMaximum: number;

  // Boolean properties surfaced by WebHID.
  isAbsolute: boolean;
  isArray: boolean;
  isBufferedBytes: boolean;
  isConstant: boolean;
  isLinear: boolean;
  isRange: boolean;
  isRelative: boolean;
  isVolatile: boolean;
  hasNull: boolean;
  hasPreferredState: boolean;
  isWrapped: boolean;
}

export interface HidReportInfo {
  reportId: number;
  items: readonly HidReportItem[];
}

export interface HidCollectionInfo {
  usagePage: number;
  usage: number;
  type: HidCollectionType;
  children: readonly HidCollectionInfo[];
  inputReports: readonly HidReportInfo[];
  outputReports: readonly HidReportInfo[];
  featureReports: readonly HidReportInfo[];
}

export type NormalizedHidCollectionInfo = HidCollectionInfo;
export type NormalizedHidReportInfo = HidReportInfo;
export type NormalizedHidReportItem = HidReportItem;

const MAX_RANGE_CONTIGUITY_CHECK_LEN = 4096;

function isContiguousUsageRange(usages: readonly number[]): boolean {
  if (usages.length === 0) return true;

  const sorted = Array.from(new Set(usages)).sort((a, b) => a - b);
  const min = sorted[0]!;
  const max = sorted[sorted.length - 1]!;
  if (min === max) return true;

  // Support the legacy `[min, max]` representation.
  if (sorted.length === 2) return true;

  const span = max - min + 1;
  if (span !== sorted.length) return false;
  for (let i = 0; i < sorted.length; i++) {
    if (sorted[i] !== min + i) return false;
  }
  return true;
}

function normalizeReportItem(item: HidReportItem): NormalizedHidReportItem {
  const usages = Array.from(item.usages);

  // WebHID uses `isRange` + expanded `usages` lists. Be robust to malformed input:
  // if `isRange` is true but the list is not contiguous, downgrade to explicit usages.
  const isRange =
    item.isRange && (usages.length > MAX_RANGE_CONTIGUITY_CHECK_LEN || isContiguousUsageRange(usages));

  return {
    usagePage: item.usagePage,
    usages,
    usageMinimum: item.usageMinimum,
    usageMaximum: item.usageMaximum,
    reportSize: item.reportSize,
    reportCount: item.reportCount,
    unitExponent: item.unitExponent,
    unit: item.unit,
    logicalMinimum: item.logicalMinimum,
    logicalMaximum: item.logicalMaximum,
    physicalMinimum: item.physicalMinimum,
    physicalMaximum: item.physicalMaximum,
    strings: Array.from(item.strings),
    stringMinimum: item.stringMinimum,
    stringMaximum: item.stringMaximum,
    designators: Array.from(item.designators),
    designatorMinimum: item.designatorMinimum,
    designatorMaximum: item.designatorMaximum,

    isAbsolute: item.isAbsolute,
    isArray: item.isArray,
    isBufferedBytes: item.isBufferedBytes,
    isConstant: item.isConstant,
    isLinear: item.isLinear,
    isRange,
    isRelative: item.isRelative,
    isVolatile: item.isVolatile,
    hasNull: item.hasNull,
    hasPreferredState: item.hasPreferredState,
    isWrapped: item.isWrapped,
  };
}

function normalizeReportInfo(report: HidReportInfo): NormalizedHidReportInfo {
  return {
    reportId: report.reportId,
    items: report.items.map(normalizeReportItem),
  };
}

function normalizeCollection(collection: HidCollectionInfo): NormalizedHidCollectionInfo {
  return {
    usagePage: collection.usagePage,
    usage: collection.usage,
    type: collection.type,
    children: collection.children.map(normalizeCollection),
    inputReports: collection.inputReports.map(normalizeReportInfo),
    outputReports: collection.outputReports.map(normalizeReportInfo),
    featureReports: collection.featureReports.map(normalizeReportInfo),
  };
}

export function normalizeCollections(
  collections: readonly HidCollectionInfo[],
): NormalizedHidCollectionInfo[] {
  return collections.map(normalizeCollection);
}
