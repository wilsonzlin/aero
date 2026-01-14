import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  normalizeCollections,
  type HidCollectionInfo,
  type HidReportItem,
} from "../src/hid/webhid_normalize.ts";

const MOUSE_FIXTURE_URL = new URL("../../tests/fixtures/hid/webhid_normalized_mouse.json", import.meta.url);
const KEYBOARD_FIXTURE_URL = new URL(
  "../../tests/fixtures/hid/webhid_normalized_keyboard.json",
  import.meta.url,
);
const GAMEPAD_FIXTURE_URL = new URL("../../tests/fixtures/hid/webhid_normalized_gamepad.json", import.meta.url);

function readFixture(url: URL): unknown {
  return JSON.parse(readFileSync(url, "utf8"));
}

function u32(values: number[]): readonly number[] {
  return new Uint32Array(values) as unknown as readonly number[];
}

const MOUSE_BUTTONS_ITEM: HidReportItem = {
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

const MOUSE_XY_ITEM: HidReportItem = {
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

const MOCK_MOUSE_COLLECTIONS: HidCollectionInfo[] = [
  {
    usagePage: 1,
    usage: 2,
    type: "application",
    children: [],
    inputReports: [{ reportId: 0, items: [MOUSE_BUTTONS_ITEM, MOUSE_XY_ITEM] }],
    outputReports: [],
    featureReports: [],
  },
];

test("normalizeCollections: WebHID normalized metadata JSON contract", () => {
  const expected = readFixture(MOUSE_FIXTURE_URL);
  assert.deepStrictEqual(normalizeCollections(MOCK_MOUSE_COLLECTIONS, { validate: true }), expected);
});

test("normalizeCollections: WebHID normalized keyboard metadata JSON contract", () => {
  const modifiersItem: HidReportItem = {
    usagePage: 7,
    usages: u32([224, 231]),
    usageMinimum: 224,
    usageMaximum: 231,
    reportSize: 1,
    reportCount: 8,
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

  const reservedByteItem: HidReportItem = {
    ...modifiersItem,
    usages: u32([]),
    usageMinimum: 0,
    usageMaximum: 0,
    reportSize: 8,
    reportCount: 1,
    isConstant: true,
    isRange: false,
  };

  const keysItem: HidReportItem = {
    ...modifiersItem,
    usages: u32([0, 101]),
    usageMinimum: 0,
    usageMaximum: 101,
    reportSize: 8,
    reportCount: 6,
    logicalMaximum: 101,
    physicalMaximum: 101,
    isArray: true,
  };

  const ledsItem: HidReportItem = {
    ...modifiersItem,
    usagePage: 8,
    usages: u32([1, 5]),
    usageMinimum: 1,
    usageMaximum: 5,
    reportSize: 1,
    reportCount: 5,
    isConstant: false,
  };

  const ledsPaddingItem: HidReportItem = {
    ...ledsItem,
    usages: u32([]),
    usageMinimum: 0,
    usageMaximum: 0,
    reportSize: 3,
    reportCount: 1,
    isConstant: true,
    isRange: false,
  };

  const collections: HidCollectionInfo[] = [
    {
      usagePage: 1,
      usage: 6,
      type: "application",
      children: [],
      inputReports: [{ reportId: 0, items: [modifiersItem, reservedByteItem, keysItem] }],
      outputReports: [{ reportId: 0, items: [ledsItem, ledsPaddingItem] }],
      featureReports: [],
    },
  ];

  const expected = readFixture(KEYBOARD_FIXTURE_URL);
  assert.deepStrictEqual(normalizeCollections(collections, { validate: true }), expected);
});

test("normalizeCollections: WebHID normalized gamepad metadata JSON contract", () => {
  const buttonsItem: HidReportItem = {
    ...MOUSE_BUTTONS_ITEM,
    usages: u32([1, 8]),
    usageMinimum: 1,
    usageMaximum: 8,
    reportCount: 8,
  };

  const axesItem: HidReportItem = {
    ...MOUSE_XY_ITEM,
    isAbsolute: true,
    isRelative: false,
  };

  const collections: HidCollectionInfo[] = [
    {
      usagePage: 1,
      usage: 5,
      type: "application",
      children: [],
      inputReports: [{ reportId: 0, items: [buttonsItem, axesItem] }],
      outputReports: [],
      featureReports: [],
    },
  ];

  const expected = readFixture(GAMEPAD_FIXTURE_URL);
  assert.deepStrictEqual(normalizeCollections(collections, { validate: true }), expected);
});

test("normalizeCollections: derives usageMinimum/Maximum from usages for small ranges", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...MOCK_MOUSE_COLLECTIONS[0]!,
      inputReports: [
        {
          reportId: 0,
          items: [
            {
              ...MOUSE_BUTTONS_ITEM,
              usageMinimum: 0,
              usageMaximum: 0,
            },
          ],
        },
      ],
    },
  ];

  const normalized = normalizeCollections(collections);
  const item = normalized[0]!.inputReports[0]!.items[0]!;
  assert.equal(item.usageMinimum, 1);
  assert.equal(item.usageMaximum, 3);
  assert.deepStrictEqual(item.usages, [1, 3]);
});

test("normalizeCollections: expanded usages lists are canonicalized to compact [min, max]", () => {
  const expandedButtonsItem: HidReportItem = {
    ...MOUSE_BUTTONS_ITEM,
    usages: u32([1, 2, 3]),
  };

  const normalized = normalizeCollections([
    {
      ...MOCK_MOUSE_COLLECTIONS[0]!,
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
    ...MOUSE_BUTTONS_ITEM,
    usages: hugeUsages,
    usageMinimum: 1,
    usageMaximum: 10_000,
    reportCount: 10_000,
  };

  const normalized = normalizeCollections([
    {
      ...MOCK_MOUSE_COLLECTIONS[0]!,
      inputReports: [{ reportId: 0, items: [hugeRangeItem] }],
    },
  ]);

  const item = normalized[0]!.inputReports[0]!.items[0]!;
  assert.deepStrictEqual(item.usages, [hugeRangeItem.usageMinimum, hugeRangeItem.usageMaximum]);
  assert.ok(item.usages.length <= 2);
});
