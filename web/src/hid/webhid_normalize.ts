export type HidCollectionType =
  | "physical"
  | "application"
  | "logical"
  | "report"
  | "namedArray"
  | "usageSwitch"
  | "usageModifier";

// HID collection type codes (HID 1.11, Collection main item payload).
export type HidCollectionTypeCode = 0 | 1 | 2 | 3 | 4 | 5 | 6;

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

export interface NormalizedHidCollectionInfo {
  usagePage: number;
  usage: number;
  collectionType: HidCollectionTypeCode;
  children: readonly NormalizedHidCollectionInfo[];
  inputReports: readonly NormalizedHidReportInfo[];
  outputReports: readonly NormalizedHidReportInfo[];
  featureReports: readonly NormalizedHidReportInfo[];
}
export type NormalizedHidReportInfo = HidReportInfo;
export type NormalizedHidReportItem = HidReportItem;

const MAX_RANGE_CONTIGUITY_CHECK_LEN = 4096;

function normalizeCollectionType(type: HidCollectionType): HidCollectionTypeCode {
  switch (type) {
    case "physical":
      return 0;
    case "application":
      return 1;
    case "logical":
      return 2;
    case "report":
      return 3;
    case "namedArray":
      return 4;
    case "usageSwitch":
      return 5;
    case "usageModifier":
      return 6;
    default: {
      const _exhaustive: never = type;
      throw new Error(`unknown HID collection type: ${_exhaustive}`);
    }
  }
}

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
  // WebHID uses `isRange` + expanded `usages` lists. For normalized metadata we emit
  // compact ranges (`[min, max]`) to keep the JSON contract bounded and deterministic.
  //
  // Be robust to malformed input: if `isRange` is true but a *small* `usages` list is
  // not contiguous, downgrade to explicit usages.
  //
  // IMPORTANT: When the list is huge, do not clone/copy it just to check contiguity.
  const isRange =
    item.isRange &&
    (item.usages.length > MAX_RANGE_CONTIGUITY_CHECK_LEN || isContiguousUsageRange(item.usages));

  const usages = isRange
    ? item.usageMinimum === item.usageMaximum
      ? [item.usageMinimum]
      : [item.usageMinimum, item.usageMaximum]
    : Array.from(item.usages);

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
    collectionType: normalizeCollectionType(collection.type),
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
