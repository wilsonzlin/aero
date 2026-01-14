import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  normalizeCollections,
  type HidCollectionInfo,
  type HidReportItem,
} from "../src/hid/webhid_normalize";

function fixtureUrl(name: string): URL {
  return new URL(`../../tests/fixtures/hid/${name}`, import.meta.url);
}

function readFixture(name: string): unknown {
  return JSON.parse(readFileSync(fixtureUrl(name), "utf8"));
}

function u32(values: number[]): readonly number[] {
  return new Uint32Array(values) as unknown as readonly number[];
}

describe("normalizeCollections: WebHID normalized fixture contract (vitest)", () => {
  it("mouse", () => {
    const buttonsItem: HidReportItem = {
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

    const xyItem: HidReportItem = {
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

    const collections: HidCollectionInfo[] = [
      {
        usagePage: 1,
        usage: 2,
        type: "application",
        children: [],
        inputReports: [{ reportId: 0, items: [buttonsItem, xyItem] }],
        outputReports: [],
        featureReports: [],
      },
    ];

    expect(normalizeCollections(collections, { validate: true })).toEqual(
      readFixture("webhid_normalized_mouse.json"),
    );
  });

  it("keyboard", () => {
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

    expect(normalizeCollections(collections, { validate: true })).toEqual(
      readFixture("webhid_normalized_keyboard.json"),
    );
  });

  it("gamepad", () => {
    const buttonsItem: HidReportItem = {
      usagePage: 9,
      usages: u32([1, 8]),
      usageMinimum: 1,
      usageMaximum: 8,
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

    const xyItem: HidReportItem = {
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
      isAbsolute: true,
      isArray: false,
      isBufferedBytes: false,
      isConstant: false,
      isLinear: true,
      isRange: false,
      isRelative: false,
      isVolatile: false,
      hasNull: false,
      hasPreferredState: true,
      isWrapped: false,
    };

    const collections: HidCollectionInfo[] = [
      {
        usagePage: 1,
        usage: 5,
        type: "application",
        children: [],
        inputReports: [{ reportId: 0, items: [buttonsItem, xyItem] }],
        outputReports: [],
        featureReports: [],
      },
    ];

    expect(normalizeCollections(collections, { validate: true })).toEqual(
      readFixture("webhid_normalized_gamepad.json"),
    );
  });
});

