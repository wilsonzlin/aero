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

function normalizeReportItem(item: HidReportItem): NormalizedHidReportItem {
  return {
    usagePage: item.usagePage,
    usages: Array.from(item.usages),
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
    isRange: item.isRange,
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

