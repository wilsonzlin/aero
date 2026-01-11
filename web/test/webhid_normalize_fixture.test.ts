import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  normalizeCollections,
  type HidCollectionInfo,
  type HidReportItem,
} from "../src/hid/webhid_normalize.ts";

const FIXTURE_URL = new URL("../../tests/fixtures/hid/webhid_normalized_mouse.json", import.meta.url);

function readFixture(): unknown {
  return JSON.parse(readFileSync(FIXTURE_URL, "utf8"));
}

function u32(values: number[]): readonly number[] {
  return new Uint32Array(values) as unknown as readonly number[];
}

const BUTTONS_ITEM: HidReportItem = {
  usagePage: 9,
  usages: u32([1, 3]),
  usageMinimum: 1,
  usageMaximum: 3,
  reportSize: 1,
  reportCount: 3,
  unitExponent: 0,
  unit: 0,
  logicalMinimum: 0,
  logicalMaximum: 1,
  physicalMinimum: 0,
  physicalMaximum: 1,
  strings: u32([]),
  stringMinimum: 0,
  stringMaximum: 0,
  designators: u32([]),
  designatorMinimum: 0,
  designatorMaximum: 0,
  isAbsolute: true,
  isArray: false,
  isBufferedBytes: false,
  isConstant: false,
  isLinear: true,
  isRange: true,
  isRelative: false,
  isVolatile: false,
  hasNull: false,
  hasPreferredState: true,
  isWrapped: false,
};

const XY_ITEM: HidReportItem = {
  usagePage: 1,
  usages: u32([48, 49]),
  usageMinimum: 0,
  usageMaximum: 0,
  reportSize: 8,
  reportCount: 2,
  unitExponent: 0,
  unit: 0,
  logicalMinimum: -127,
  logicalMaximum: 127,
  physicalMinimum: -127,
  physicalMaximum: 127,
  strings: u32([]),
  stringMinimum: 0,
  stringMaximum: 0,
  designators: u32([]),
  designatorMinimum: 0,
  designatorMaximum: 0,
  isAbsolute: false,
  isArray: false,
  isBufferedBytes: false,
  isConstant: false,
  isLinear: true,
  isRange: false,
  isRelative: true,
  isVolatile: false,
  hasNull: false,
  hasPreferredState: true,
  isWrapped: false,
};

const MOCK_COLLECTIONS: HidCollectionInfo[] = [
  {
    usagePage: 1,
    usage: 2,
    type: "application",
    children: [],
    inputReports: [{ reportId: 0, items: [BUTTONS_ITEM, XY_ITEM] }],
    outputReports: [],
    featureReports: [],
  },
];

test("normalizeCollections: WebHID normalized metadata JSON contract", () => {
  const expected = readFixture();
  assert.deepStrictEqual(normalizeCollections(MOCK_COLLECTIONS), expected);
});

test("normalizeCollections: expanded usages lists are canonicalized to compact [min, max]", () => {
  const expandedButtonsItem: HidReportItem = {
    ...BUTTONS_ITEM,
    usages: u32([1, 2, 3]),
  };

  const normalized = normalizeCollections([
    {
      ...MOCK_COLLECTIONS[0]!,
      inputReports: [{ reportId: 0, items: [expandedButtonsItem] }],
    },
  ]);

  const item = normalized[0]!.inputReports[0]!.items[0]!;
  assert.deepStrictEqual(item.usages, [expandedButtonsItem.usageMinimum, expandedButtonsItem.usageMaximum]);
});

test("normalizeCollections: huge isRange usages lists are normalized to compact [min, max]", () => {
  // Proxy that reports a huge `.length` but throws if anything tries to iterate/copy it. This
  // ensures we don't accidentally clone a massive array just to check contiguity.
  const hugeUsages = new Proxy(
    { length: 10_000 },
    {
      get(target, prop) {
        if (prop === "length") return target.length;
        throw new Error(`unexpected access to huge usages list: ${String(prop)}`);
      },
    },
  ) as unknown as readonly number[];

  const hugeRangeItem: HidReportItem = {
    ...BUTTONS_ITEM,
    usages: hugeUsages,
    usageMinimum: 1,
    usageMaximum: 10_000,
    reportCount: 10_000,
  };

  const normalized = normalizeCollections([
    {
      ...MOCK_COLLECTIONS[0]!,
      inputReports: [{ reportId: 0, items: [hugeRangeItem] }],
    },
  ]);

  const item = normalized[0]!.inputReports[0]!.items[0]!;
  assert.deepStrictEqual(item.usages, [hugeRangeItem.usageMinimum, hugeRangeItem.usageMaximum]);
  assert.ok(item.usages.length <= 2);
});
