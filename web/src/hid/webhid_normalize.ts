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
//   under `crates/emulator`).

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
const MAX_COLLECTION_DEPTH = 32;
const MAX_REPORT_SIZE_BITS = 255;
const MAX_REPORT_COUNT = 65_535;
const MAX_U32 = 0xffff_ffffn;
const MAX_U32_NUM = 0xffff_ffff;
const MAX_U16_NUM = 0xffff;
const MAX_INTERRUPT_REPORT_BYTES = 64n;
// USB control transfers use a 16-bit `wLength`, so a single transaction cannot carry more than
// u16::MAX bytes of payload.
const MAX_CONTROL_REPORT_BYTES = 0xffffn;
const MIN_I32 = -0x8000_0000;
const MAX_I32 = 0x7fff_ffff;

type Path = string[];

function pathToString(path: Path): string {
  return path.join(".");
}

function err(path: Path, message: string): Error {
  return new Error(`${message} (at ${pathToString(path)})`);
}

function validateU32(value: unknown, name: string, path: Path): void {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > MAX_U32_NUM) {
    throw err(path, `${name} must be an integer in [0, ${MAX_U32_NUM}] (got ${String(value)})`);
  }
}

function validateU16(value: unknown, name: string, path: Path): void {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > MAX_U16_NUM) {
    throw err(path, `${name} must be an integer in [0, ${MAX_U16_NUM}] (got ${String(value)})`);
  }
}

function validateI32(value: unknown, name: string, path: Path): void {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < MIN_I32 ||
    value > MAX_I32
  ) {
    throw err(path, `${name} must be an integer in [${MIN_I32}, ${MAX_I32}] (got ${String(value)})`);
  }
}

function validateU32Array(values: unknown, name: string, path: Path): void {
  if (!Array.isArray(values)) {
    throw err(path, `${name} must be an array (got ${String(values)})`);
  }
  for (let i = 0; i < values.length; i++) {
    validateU32(values[i], `${name}[${i}]`, path);
  }
}

function validateU16Array(values: unknown, name: string, path: Path): void {
  if (!Array.isArray(values)) {
    throw err(path, `${name} must be an array (got ${String(values)})`);
  }
  for (let i = 0; i < values.length; i++) {
    validateU16(values[i], `${name}[${i}]`, path);
  }
}

function validateBool(value: unknown, name: string, path: Path): void {
  if (typeof value !== "boolean") {
    throw err(path, `${name} must be a boolean (got ${String(value)})`);
  }
}

function coerceU32Array(value: unknown): number[] {
  if (value === null || value === undefined) return [];
  let values: unknown[];
  try {
    values = Array.from(value as unknown as Iterable<unknown>);
  } catch {
    return [];
  }
  const out: number[] = [];
  for (const v of values) {
    if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0 || v > MAX_U32_NUM) continue;
    out.push(v);
  }
  return out;
}

function normalizeCollectionType(type: HidCollectionTypeLike, path: Path): HidCollectionTypeCode {
  // Some environments surface numeric HID collection type codes directly, while others use the
  // WebHID string enum. Support both to keep the normalizer resilient to typing/library changes.
  if (typeof type === "number") {
    if (!Number.isInteger(type) || type < 0 || type > 6) {
      throw err(path, `unknown HID collection type code: ${String(type)}`);
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
      throw err(path, `unknown HID collection type: ${_exhaustive}`);
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
export type NormalizeCollectionsOptions = {
  /**
   * When enabled, validate key invariants before returning the normalized
   * metadata. This makes failures deterministic and actionable instead of
   * synthesizing an invalid report descriptor.
   */
  validate?: boolean;
};

function normalizeReportItem(item: HidReportItem, path: Path): NormalizedHidReportItem {
  const usagePage = item.usagePage ?? 0;
  const rawUsages = item.usages ?? [];

  // Reject the ambiguous case where the caller claims a non-degenerate range but only provides a
  // single usage entry; our normalizer would otherwise "collapse" the range when deriving min/max.
  if (item.isRange && rawUsages.length === 1 && item.usageMinimum !== item.usageMaximum) {
    throw err(
      path,
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
    (item.isRange ?? false) &&
    (rawUsages.length > MAX_RANGE_CONTIGUITY_CHECK_LEN || isContiguousUsageRange(rawUsages));

  let usageMinimum = item.usageMinimum ?? 0;
  let usageMaximum = item.usageMaximum ?? 0;

  if (isRange && rawUsages.length >= 1 && rawUsages.length <= MAX_RANGE_CONTIGUITY_CHECK_LEN) {
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

  // When `isRange` is true in the normalized contract, `usages` is always the compact `[min, max]`
  // representation (even for a degenerate range where `min === max`).
  const usages = isRange ? [usageMinimum, usageMaximum] : Array.from(rawUsages);

  const isAbsolute = item.isAbsolute ?? (item.isRelative !== undefined ? !item.isRelative : true);
  // `isRelative` is redundant with `isAbsolute`. Prefer `isAbsolute` as the source of truth when
  // both are present so the normalized contract is always internally consistent.
  const isRelative = !isAbsolute;
  const isWrapped = item.isWrapped ?? item.wrap ?? false;

  return {
    usagePage,
    usages,
    usageMinimum,
    usageMaximum,
    reportSize: item.reportSize ?? 0,
    reportCount: item.reportCount ?? 0,
    unitExponent: item.unitExponent ?? 0,
    unit: item.unit ?? 0,
    logicalMinimum: item.logicalMinimum ?? 0,
    logicalMaximum: item.logicalMaximum ?? 0,
    physicalMinimum: item.physicalMinimum ?? 0,
    physicalMaximum: item.physicalMaximum ?? 0,
    // WebHID type definitions are inconsistent about the `strings`/`designators` locals: some
    // model them as string arrays, while others use numeric indices. We don't currently
    // synthesize these tags, but we still want the normalized JSON contract to match the Rust
    // schema shape (u32 arrays). Filter non-u32 entries rather than throwing.
    strings: coerceU32Array((item as unknown as { strings?: unknown }).strings),
    stringMinimum: item.stringMinimum ?? 0,
    stringMaximum: item.stringMaximum ?? 0,
    designators: coerceU32Array((item as unknown as { designators?: unknown }).designators),
    designatorMinimum: item.designatorMinimum ?? 0,
    designatorMaximum: item.designatorMaximum ?? 0,

    isAbsolute,
    isArray: item.isArray ?? false,
    isBufferedBytes: item.isBufferedBytes ?? false,
    isConstant: item.isConstant ?? false,
    isLinear: item.isLinear ?? true,
    isRange,
    isRelative,
    isVolatile: item.isVolatile ?? false,
    hasNull: item.hasNull ?? false,
    hasPreferredState: item.hasPreferredState ?? true,
    isWrapped,
  };
}

function normalizeReportInfo(report: HidReportInfo, path: Path): NormalizedHidReportInfo {
  const reportId = report.reportId ?? 0;
  if (!Number.isInteger(reportId) || reportId < 0 || reportId > 0xff) {
    throw err(path, `Invalid HID reportId: expected integer in [0,255], got ${String(reportId)}`);
  }

  const rawItems = report.items ?? [];
  const items: NormalizedHidReportItem[] = [];
  for (let itemIdx = 0; itemIdx < rawItems.length; itemIdx++) {
    items.push(normalizeReportItem(rawItems[itemIdx]!, [...path, `items[${itemIdx}]`]));
  }

  return {
    reportId,
    items,
  };
}

type ReportListName = "inputReports" | "outputReports" | "featureReports";

function normalizeReportList(
  reports: readonly HidReportInfo[],
  listName: ReportListName,
  collectionPath: Path,
): NormalizedHidReportInfo[] {
  const rawReports = reports ?? [];
  const out: NormalizedHidReportInfo[] = [];
  for (let reportIdx = 0; reportIdx < rawReports.length; reportIdx++) {
    out.push(
      normalizeReportInfo(rawReports[reportIdx]!, [
        ...collectionPath,
        `${listName}[${reportIdx}]`,
      ]),
    );
  }
  return out;
}

function normalizeCollection(
  collection: HidCollectionInfo,
  path: Path,
  depth: number,
  stack: WeakSet<object>,
): NormalizedHidCollectionInfo {
  if (depth > MAX_COLLECTION_DEPTH) {
    throw err(path, `HID collection nesting exceeds max depth ${MAX_COLLECTION_DEPTH}`);
  }

  const obj = typeof collection === "object" && collection !== null ? (collection as unknown as object) : null;
  if (obj) {
    if (stack.has(obj)) {
      throw err(path, "HID collection tree contains a cycle");
    }
    stack.add(obj);
  }

  try {
    const children: NormalizedHidCollectionInfo[] = [];
    const rawChildren = collection.children ?? [];
    for (let childIdx = 0; childIdx < rawChildren.length; childIdx++) {
      children.push(
        normalizeCollection(
          rawChildren[childIdx]!,
          [...path, `children[${childIdx}]`],
          depth + 1,
          stack,
        ),
      );
    }

    return {
      usagePage: collection.usagePage ?? 0,
      usage: collection.usage ?? 0,
      collectionType: normalizeCollectionType(collection.type, [...path, "type"]),
      children,
      inputReports: normalizeReportList(collection.inputReports ?? [], "inputReports", path),
      outputReports: normalizeReportList(collection.outputReports ?? [], "outputReports", path),
      featureReports: normalizeReportList(collection.featureReports ?? [], "featureReports", path),
    };
  } finally {
    if (obj) stack.delete(obj);
  }
}

// Overload so callsites can pass `HIDDevice.collections` without casts (the WebHID types exposed by
// `@types/w3c-web-hid` are optional/loose and do not precisely match Chromium's runtime shape).
export function normalizeCollections(
  collections: readonly HidCollectionInfo[],
  options?: NormalizeCollectionsOptions,
): NormalizedHidCollectionInfo[];
export function normalizeCollections(
  collections: readonly HIDCollectionInfo[],
  options?: NormalizeCollectionsOptions,
): NormalizedHidCollectionInfo[];
export function normalizeCollections(
  collections: readonly unknown[],
  options: NormalizeCollectionsOptions = {},
): NormalizedHidCollectionInfo[] {
  const rawCollections = collections as readonly HidCollectionInfo[];
  const normalized: NormalizedHidCollectionInfo[] = [];
  for (let i = 0; i < rawCollections.length; i++) {
    normalized.push(normalizeCollection(rawCollections[i]!, [`collections[${i}]`], 1, new WeakSet()));
  }
  if (options.validate) {
    validateCollections(normalized);
  }
  return normalized;
}

function validateCollections(collections: readonly NormalizedHidCollectionInfo[]): void {
  let hasNonZeroReportId = false;
  let firstZeroReportPath: Path | null = null;
  const reportBits = new Map<string, bigint>();

  const visitCollection = (collection: NormalizedHidCollectionInfo, path: Path): void => {
    validateU16(collection.usagePage, "usagePage", path);
    validateU16(collection.usage, "usage", path);

    const visitReportList = (
      reports: readonly NormalizedHidReportInfo[],
      listName: "inputReports" | "outputReports" | "featureReports",
    ): void => {
      for (let reportIdx = 0; reportIdx < reports.length; reportIdx++) {
        const report = reports[reportIdx];
        const reportPath = [...path, `${listName}[${reportIdx}]`];
        const reportKey = `${listName}:${report.reportId}`;
        let totalBits = reportBits.get(reportKey) ?? 0n;

        if (report.reportId === 0) {
          if (firstZeroReportPath === null) firstZeroReportPath = reportPath;
        } else {
          hasNonZeroReportId = true;
        }

        for (let itemIdx = 0; itemIdx < report.items.length; itemIdx++) {
          const item = report.items[itemIdx];
          const itemPath = [...reportPath, `items[${itemIdx}]`];

          validateU16(item.usagePage, "usagePage", itemPath);
          validateU16Array(item.usages, "usages", itemPath);
          validateU16(item.usageMinimum, "usageMinimum", itemPath);
          validateU16(item.usageMaximum, "usageMaximum", itemPath);
          validateU32Array(item.strings, "strings", itemPath);
          validateU32(item.stringMinimum, "stringMinimum", itemPath);
          validateU32(item.stringMaximum, "stringMaximum", itemPath);
          validateU32Array(item.designators, "designators", itemPath);
          validateU32(item.designatorMinimum, "designatorMinimum", itemPath);
          validateU32(item.designatorMaximum, "designatorMaximum", itemPath);
          validateU32(item.unit, "unit", itemPath);
          validateI32(item.logicalMinimum, "logicalMinimum", itemPath);
          validateI32(item.logicalMaximum, "logicalMaximum", itemPath);
          validateI32(item.physicalMinimum, "physicalMinimum", itemPath);
          validateI32(item.physicalMaximum, "physicalMaximum", itemPath);
          validateBool(item.isAbsolute, "isAbsolute", itemPath);
          validateBool(item.isArray, "isArray", itemPath);
          validateBool(item.isBufferedBytes, "isBufferedBytes", itemPath);
          validateBool(item.isConstant, "isConstant", itemPath);
          validateBool(item.isLinear, "isLinear", itemPath);
          validateBool(item.isRange, "isRange", itemPath);
          validateBool(item.isRelative, "isRelative", itemPath);
          validateBool(item.isVolatile, "isVolatile", itemPath);
          validateBool(item.hasNull, "hasNull", itemPath);
          validateBool(item.hasPreferredState, "hasPreferredState", itemPath);
          validateBool(item.isWrapped, "isWrapped", itemPath);

          if (
            !Number.isSafeInteger(item.reportSize) ||
            item.reportSize < 1 ||
            item.reportSize > MAX_REPORT_SIZE_BITS
          ) {
            throw err(
              itemPath,
              `reportSize must be in 1..=${MAX_REPORT_SIZE_BITS} (got ${String(item.reportSize)})`,
            );
          }

          if (!Number.isSafeInteger(item.reportCount) || item.reportCount < 0) {
            throw err(
              itemPath,
              `reportCount must be in 0..=${MAX_REPORT_COUNT} (got ${String(item.reportCount)})`,
            );
          }

          const bits = BigInt(item.reportSize) * BigInt(item.reportCount);
          if (bits > MAX_U32) {
            throw err(
              itemPath,
              `reportSize*reportCount overflows u32 (${item.reportSize}*${item.reportCount})`,
            );
          }

          totalBits += bits;
          if (totalBits > MAX_U32) {
            throw err(itemPath, "total report bit length overflows u32");
          }

          const dataBytes = (totalBits + 7n) / 8n;
          const reportBytes = dataBytes + (report.reportId !== 0 ? 1n : 0n);

          if (listName === "inputReports") {
            // Full-speed USB interrupt endpoints have a 64-byte max packet size, and HID input
            // reports must fit within a single interrupt IN transaction.
            if (reportBytes > MAX_INTERRUPT_REPORT_BYTES) {
              throw err(
                itemPath,
                `input report length ${reportBytes} bytes exceeds max USB full-speed interrupt packet size ${MAX_INTERRUPT_REPORT_BYTES}`,
              );
            }
          } else if (listName === "outputReports" || listName === "featureReports") {
            // Output and feature reports can be transferred over the control endpoint (SET_REPORT /
            // GET_REPORT). Bound the descriptor-defined size so we can't allocate absurdly large
            // buffers based on untrusted metadata.
            if (reportBytes > MAX_CONTROL_REPORT_BYTES) {
              const kind = listName === "outputReports" ? "output" : "feature";
              throw err(
                itemPath,
                `${kind} report length ${reportBytes} bytes exceeds max USB control transfer length ${MAX_CONTROL_REPORT_BYTES}`,
              );
            }
          }

          if (item.reportCount > MAX_REPORT_COUNT) {
            throw err(
              itemPath,
              `reportCount must be in 0..=${MAX_REPORT_COUNT} (got ${String(item.reportCount)})`,
            );
          }

          if (
            !Number.isSafeInteger(item.unitExponent) ||
            item.unitExponent < -8 ||
            item.unitExponent > 7
          ) {
            throw err(
              itemPath,
              `unitExponent must be in -8..=7 (got ${String(item.unitExponent)})`,
            );
          }

          if (item.logicalMinimum > item.logicalMaximum) {
            throw err(
              itemPath,
              `logicalMinimum must be <= logicalMaximum (got ${item.logicalMinimum} > ${item.logicalMaximum})`,
            );
          }

          if (item.physicalMinimum > item.physicalMaximum) {
            throw err(
              itemPath,
              `physicalMinimum must be <= physicalMaximum (got ${item.physicalMinimum} > ${item.physicalMaximum})`,
            );
          }

          if (!item.isRange) continue;
          const usagesLen = Array.isArray(item.usages) ? item.usages.length : 0;
          if (usagesLen !== 2) {
            throw err(itemPath, `isRange=true requires usages.length == 2 (min/max) (got ${usagesLen})`);
          }
          if (item.usages[0] > item.usages[1]) {
            throw err(
              itemPath,
              `isRange=true requires usages[0] <= usages[1] (got ${item.usages[0]} > ${item.usages[1]})`,
            );
          }
        }

        reportBits.set(reportKey, totalBits);
      }
    };

    visitReportList(collection.inputReports, "inputReports");
    visitReportList(collection.outputReports, "outputReports");
    visitReportList(collection.featureReports, "featureReports");

    for (let childIdx = 0; childIdx < collection.children.length; childIdx++) {
      visitCollection(collection.children[childIdx], [...path, `children[${childIdx}]`]);
    }
  };

  for (let i = 0; i < collections.length; i++) {
    visitCollection(collections[i], [`collections[${i}]`]);
  }

  if (hasNonZeroReportId && firstZeroReportPath !== null) {
    throw err(
      firstZeroReportPath,
      "Found reportId 0 but other reports use non-zero reportId; when any report uses a reportId, all reports must use a non-zero reportId",
    );
  }
}
