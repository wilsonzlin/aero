// WebHID spec: https://wicg.github.io/webhid/
//
// WebHID exposes collections/reports/items as platform objects (with FrozenArray properties)
// that are not stable to serialize or send across postMessage/WASM.
//
// TypeScript note: WebHID types are provided via `@types/w3c-web-hid` (referenced by
// `web/src/vite-env.d.ts`).
//
// Normalize `HIDDevice.collections` into plain JSON-compatible objects:
// - deep-copied (no retained references to platform objects)
// - arrays are real JS arrays (via Array.from)
// - shape matches the Rust `HidCollectionInfo`/`HidReportInfo`/`HidReportItem`
//   structs in `crates/aero-usb/src/hid/webhid.rs` and is locked down by fixtures
//   under `tests/fixtures/hid/` (the native emulator stack mirrors the same schema
//   under `crates/emulator/src/io/usb/hid/webhid.rs`).

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
export type HidCollectionTypeLike = HidCollectionType | HidCollectionTypeCode;

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
  //
  // Most of these correspond directly to HID main-item (Input/Output/Feature) flag bits.
  // See `docs/webhid-hid-report-descriptor-synthesis.md` for the full mapping, including the
  // Input-vs-Output/Feature differences around bit7/bit8.
  isAbsolute: boolean;
  isArray: boolean;
  isBufferedBytes: boolean;
  isConstant: boolean;
  isLinear: boolean;
  isRange: boolean;
  // WebHID exposes both `isAbsolute` and `isRelative` (redundant). Some WebHID type definitions omit
  // `isRelative`; we derive it as `!isAbsolute` when absent.
  isRelative?: boolean;
  isVolatile: boolean;
  hasNull: boolean;
  hasPreferredState: boolean;
  // Wrap flag (HID main-item bit3).
  //
  // Chromium exposes this as `isWrapped`. Some WebHID type definitions use the older `wrap` name.
  isWrapped?: boolean;
  wrap?: boolean;
}

export interface HidReportInfo {
  reportId: number;
  items: readonly HidReportItem[];
}

export interface HidCollectionInfo {
  usagePage: number;
  usage: number;
  // The WebHID spec exposes collection types as numeric codes, but some browser/type-definition
  // combos use string enums ("application", "physical", ...). Accept both and normalize to the
  // numeric HID code in the output contract.
  type: HidCollectionTypeLike;
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
export type NormalizedHidReportItem = Omit<HidReportItem, "isRelative" | "isWrapped" | "wrap"> & {
  isRelative: boolean;
  isWrapped: boolean;
};
export type NormalizedHidReportInfo = Omit<HidReportInfo, "items"> & {
  items: readonly NormalizedHidReportItem[];
};

export const MAX_RANGE_CONTIGUITY_CHECK_LEN = 4096;

function normalizeCollectionType(type: HidCollectionTypeLike): HidCollectionTypeCode {
  // Some environments surface numeric HID collection type codes directly, while others use the
  // WebHID string enum. Support both to keep the normalizer resilient to typing/library changes.
  if (typeof type === "number") {
    if (!Number.isInteger(type) || type < 0 || type > 6) {
      throw new Error(`unknown HID collection type code: ${String(type)}`);
    }
    return type;
  }

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
  const rawUsages = item.usages;

  // WebHID's isRange flag can be set even for a degenerate range where `usageMinimum === usageMaximum`.
  // In that case the normalized representation uses a single-element `usages: [usageMinimum]` list.
  //
  // Reject the ambiguous case where the caller claims a non-degenerate range but only provides a
  // single usage entry; our normalizer would otherwise "collapse" the range when deriving min/max.
  if (item.isRange && rawUsages.length === 1 && item.usageMinimum !== item.usageMaximum) {
    throw new Error(
      `Invalid HID report item: isRange=true with usageMinimum!=usageMaximum requires usages.length!=1 (got 1)`,
    );
  }

  // WebHID uses `isRange` + expanded `usages` lists. For normalized metadata we emit
  // compact ranges (`[min, max]`) to keep the JSON contract bounded and deterministic.
  //
  // Be robust to malformed input: if `isRange` is true but a *small* `usages` list is
  // not contiguous, downgrade to explicit usages.
  //
  // IMPORTANT: When the list is huge, do not clone/copy it just to check contiguity.
  const isRange =
    item.isRange &&
    (rawUsages.length > MAX_RANGE_CONTIGUITY_CHECK_LEN || isContiguousUsageRange(rawUsages));

  let usageMinimum = item.usageMinimum;
  let usageMaximum = item.usageMaximum;

  if (isRange && rawUsages.length >= 2 && rawUsages.length <= MAX_RANGE_CONTIGUITY_CHECK_LEN) {
    // Derive min/max from the explicit usage list so we don't depend on the browser's bookkeeping
    // (or on hand-authored metadata) for small ranges.
    let min = rawUsages[0]!;
    let max = rawUsages[0]!;
    for (const u of rawUsages) {
      if (u < min) min = u;
      if (u > max) max = u;
    }
    usageMinimum = min;
    usageMaximum = max;
  }

  const usages = isRange
    ? usageMinimum === usageMaximum
      ? [usageMinimum]
      : [usageMinimum, usageMaximum]
    : Array.from(rawUsages);

  const isRelative = item.isRelative ?? !item.isAbsolute;
  const isWrapped = item.isWrapped ?? item.wrap ?? false;

  return {
    usagePage: item.usagePage,
    usages,
    usageMinimum,
    usageMaximum,
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
    isRelative,
    isVolatile: item.isVolatile,
    hasNull: item.hasNull,
    hasPreferredState: item.hasPreferredState,
    isWrapped,
  };
}

function normalizeReportInfo(report: HidReportInfo): NormalizedHidReportInfo {
  const reportId = report.reportId;
  if (!Number.isInteger(reportId) || reportId < 0 || reportId > 0xff) {
    throw new Error(`Invalid HID reportId: expected integer in [0,255], got ${String(reportId)}`);
  }

  return {
    reportId,
    items: Array.from(report.items, normalizeReportItem),
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

// Overload so callsites can pass `HIDDevice.collections` without casts (the WebHID types exposed by
// `@types/w3c-web-hid` are optional/loose and do not precisely match Chromium's runtime shape).
export function normalizeCollections(collections: readonly HidCollectionInfo[]): NormalizedHidCollectionInfo[];
export function normalizeCollections(collections: readonly HIDCollectionInfo[]): NormalizedHidCollectionInfo[];
export function normalizeCollections(collections: readonly unknown[]): NormalizedHidCollectionInfo[] {
  return Array.from(collections as readonly HidCollectionInfo[], normalizeCollection);
}
