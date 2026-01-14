import { describe, expect, it } from "vitest";

import {
  normalizeCollections,
  type HidCollectionInfo,
  type HidReportInfo,
  type HidReportItem,
} from "./webhid_normalize";

function u32(values: number[]): readonly number[] {
  return new Uint32Array(values) as unknown as readonly number[];
}

function mockItem(overrides: Partial<HidReportItem> = {}): HidReportItem {
  return {
    usagePage: 0,
    usages: u32([]),
    usageMinimum: 0,
    usageMaximum: 0,
    reportSize: 0,
    reportCount: 0,
    unitExponent: 0,
    unit: 0,
    logicalMinimum: 0,
    logicalMaximum: 0,
    physicalMinimum: 0,
    physicalMaximum: 0,
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
    ...overrides,
  } as unknown as HidReportItem;
}

function mockReport(overrides: Partial<HidReportInfo> = {}): HidReportInfo {
  return {
    reportId: 0,
    items: [],
    ...overrides,
  } as unknown as HidReportInfo;
}

function mockCollection(overrides: Partial<HidCollectionInfo> = {}): HidCollectionInfo {
  return {
    usagePage: 0,
    usage: 0,
    type: "application",
    children: [],
    inputReports: [],
    outputReports: [],
    featureReports: [],
    ...overrides,
  } as unknown as HidCollectionInfo;
}

describe("normalizeCollections(WebHID)", () => {
  it("accepts partial WebHID dictionary shapes (optional fields)", () => {
    const collections: HIDCollectionInfo[] = [
      {
        usagePage: 1,
        usage: 2,
        type: 1,
        inputReports: [
          {
            items: [{} as unknown as HIDReportItem],
          } as unknown as HIDReportInfo,
        ],
      },
    ];

    const normalized = normalizeCollections(collections);
    expect(normalized).toHaveLength(1);
    expect(normalized[0]).toMatchObject({
      usagePage: 1,
      usage: 2,
      collectionType: 1,
      children: [],
      inputReports: [{ reportId: 0 }],
      outputReports: [],
      featureReports: [],
    });
    expect(normalized[0]!.inputReports[0]!.items).toHaveLength(1);

    const item = normalized[0]!.inputReports[0]!.items[0]!;
    expect(item).toMatchObject({
      usagePage: 0,
      usages: [],
      usageMinimum: 0,
      usageMaximum: 0,
      reportSize: 0,
      reportCount: 0,
      unitExponent: 0,
      unit: 0,
      logicalMinimum: 0,
      logicalMaximum: 0,
      physicalMinimum: 0,
      physicalMaximum: 0,
      strings: [],
      designators: [],
      isAbsolute: true,
      isRelative: false,
      isArray: false,
      isConstant: false,
      isBufferedBytes: false,
      isLinear: true,
      isRange: false,
      isVolatile: false,
      hasNull: false,
      hasPreferredState: true,
      isWrapped: false,
    });
  });

  it("deep-copies the full tree and produces mutable JS arrays", () => {
    const itemUsages = Object.freeze([0xe0, 0xe7]) as unknown as readonly number[];
    const itemStrings = Object.freeze([]) as unknown as readonly number[];
    const itemDesignators = Object.freeze([]) as unknown as readonly number[];
    const item = mockItem({
      isRange: true,
      usages: itemUsages,
      usageMinimum: 0xe0,
      usageMaximum: 0xe7,
      strings: itemStrings,
      designators: itemDesignators,
    });
    const reportItems = Object.freeze([item]) as unknown as readonly HidReportItem[];
    const report = mockReport({ reportId: 1, items: reportItems });
    const inputReports = Object.freeze([report]) as unknown as readonly HidReportInfo[];

    const child = mockCollection({
      usagePage: 0x07,
      usage: 0xe0,
      type: "logical",
      children: Object.freeze([]) as unknown as readonly HidCollectionInfo[],
    });

    const rootChildren = Object.freeze([child]) as unknown as readonly HidCollectionInfo[];
    const root = mockCollection({
      usagePage: 0x01,
      usage: 0x06,
      type: "application",
      inputReports,
      children: rootChildren,
    });

    const normalized = normalizeCollections(Object.freeze([root]) as unknown as readonly HidCollectionInfo[]);

    // Not the same references (deep copy).
    expect(normalized[0]).not.toBe(root);
    expect(normalized[0].children).not.toBe(root.children);
    expect(normalized[0].inputReports).not.toBe(root.inputReports);
    expect(normalized[0].inputReports[0]).not.toBe(report);
    expect(normalized[0].inputReports[0].items).not.toBe(report.items);
    expect(normalized[0].inputReports[0].items[0].usages).not.toBe(item.usages);

    // Arrays should not remain frozen.
    expect(Object.isFrozen(normalized[0].children)).toBe(false);
    expect(Object.isFrozen(normalized[0].inputReports)).toBe(false);
    expect(Object.isFrozen(normalized[0].inputReports[0].items)).toBe(false);
    expect(Object.isFrozen(normalized[0].inputReports[0].items[0].usages)).toBe(false);
    expect(Object.isFrozen(normalized[0].inputReports[0].items[0].strings)).toBe(false);
    expect(Object.isFrozen(normalized[0].inputReports[0].items[0].designators)).toBe(false);

    // Mutating output should not mutate input.
    normalized[0]!.usagePage = 0xff;
    (normalized[0].children as unknown as HidCollectionInfo[]).push(
      mockCollection({ usagePage: 1, usage: 1, type: "logical" }) as unknown as HidCollectionInfo,
    );
    (normalized[0].inputReports[0].items[0].usages as unknown as number[]).push(0xaa);

    expect(root.usagePage).toBe(0x01);
    expect(root.children.length).toBe(1);
    expect(Array.from(item.usages)).toEqual([0xe0, 0xe7]);
  });

  it("validates reportId is an integer in [0,255]", () => {
    const root = mockCollection({
      inputReports: [
        mockReport({
          reportId: 256,
          items: [mockItem()],
        }),
      ] as unknown as HidReportInfo[],
    });

    expect(() => normalizeCollections([root])).toThrow(/reportId/i);
  });

  it("rejects isRange items with a single usage when usageMinimum != usageMaximum", () => {
    const root = mockCollection({
      inputReports: [
        mockReport({
          reportId: 1,
          items: [mockItem({ isRange: true, usages: u32([1]), usageMinimum: 1, usageMaximum: 2 })],
        }),
      ] as unknown as HidReportInfo[],
    });

    expect(() => normalizeCollections([root])).toThrow(/isRange/i);
  });

  it("accepts degenerate isRange items (usageMinimum == usageMaximum)", () => {
    const root = mockCollection({
      inputReports: [
        mockReport({
          reportId: 1,
          items: [mockItem({ isRange: true, usages: u32([5]), usageMinimum: 5, usageMaximum: 5 })],
        }),
      ] as unknown as HidReportInfo[],
    });

    const normalized = normalizeCollections([root]);
    expect(normalized[0]?.inputReports[0]?.items[0]?.isRange).toBe(true);
    expect(normalized[0]?.inputReports[0]?.items[0]?.usages).toEqual([5, 5]);
  });

  it("derives isRelative when omitted and accepts wrap alias for isWrapped", () => {
    const root = mockCollection({
      inputReports: [
        mockReport({
          reportId: 1,
          items: [
            mockItem({
              isAbsolute: false,
              isRelative: undefined,
              isWrapped: undefined,
              wrap: true,
            }),
          ],
        }),
      ] as unknown as HidReportInfo[],
    });

    const normalized = normalizeCollections([root]);
    const item = normalized[0]?.inputReports[0]?.items[0];
    if (!item) throw new Error("expected normalized report item");
    expect(item.isRelative).toBe(true);
    expect(item.isWrapped).toBe(true);
    expect("wrap" in item).toBe(false);
  });

  it("normalizes a small keyboard-like collection tree", () => {
    const modifierBits = mockItem({
      isArray: false,
      isAbsolute: true,
      isBufferedBytes: false,
      isConstant: false,
      isLinear: true,
      isRange: true,
      logicalMinimum: 0,
      logicalMaximum: 1,
      physicalMinimum: 0,
      physicalMaximum: 1,
      unitExponent: 0,
      unit: 0,
      reportSize: 1,
      reportCount: 8,
      usagePage: 0x07,
      usages: u32([0xe0, 0xe7]),
      usageMinimum: 0xe0,
      usageMaximum: 0xe7,
    });

    const keys = mockItem({
      isArray: true,
      isAbsolute: true,
      isBufferedBytes: false,
      isConstant: false,
      isLinear: true,
      isRange: true,
      logicalMinimum: 0,
      logicalMaximum: 0x65,
      physicalMinimum: 0,
      physicalMaximum: 0x65,
      unitExponent: 0,
      unit: 0,
      reportSize: 8,
      reportCount: 6,
      usagePage: 0x07,
      usages: u32([0x00, 0x65]),
      usageMinimum: 0x00,
      usageMaximum: 0x65,
    });

    const leds = mockItem({
      isArray: false,
      isAbsolute: true,
      isBufferedBytes: false,
      isConstant: false,
      isLinear: true,
      isRange: true,
      logicalMinimum: 0,
      logicalMaximum: 1,
      physicalMinimum: 0,
      physicalMaximum: 1,
      unitExponent: 0,
      unit: 0,
      reportSize: 1,
      reportCount: 5,
      usagePage: 0x08,
      usages: u32([1, 5]),
      usageMinimum: 1,
      usageMaximum: 5,
    });

    const root = mockCollection({
      usagePage: 0x01,
      usage: 0x06,
      type: "application",
      inputReports: [
        mockReport({
          reportId: 1,
          items: [modifierBits, keys],
        }),
      ] as unknown as HidReportInfo[],
      outputReports: [
        mockReport({
          reportId: 1,
          items: [leds],
        }),
      ] as unknown as HidReportInfo[],
      children: [
        mockCollection({
          usagePage: 0x07,
          usage: 0xe0,
          type: "logical",
        }),
      ] as unknown as HidCollectionInfo[],
    });

    const normalized = normalizeCollections([root]);

    expect(normalized).toHaveLength(1);
    expect(normalized[0].usagePage).toBe(0x01);
    expect(normalized[0].usage).toBe(0x06);
    expect(normalized[0].collectionType).toBe(1);

    expect(normalized[0].inputReports[0].reportId).toBe(1);
    expect(normalized[0].inputReports[0].items).toHaveLength(2);
    expect(normalized[0].inputReports[0].items[0].usagePage).toBe(0x07);
    expect(Array.isArray(normalized[0].inputReports[0].items[0].usages)).toBe(true);
    expect(normalized[0].inputReports[0].items[0].usages).toEqual([0xe0, 0xe7]);

    expect(normalized[0].outputReports[0].items[0].usagePage).toBe(0x08);
    expect(normalized[0].outputReports[0].items[0].usages).toEqual([1, 5]);

    expect(normalized[0].children[0].usagePage).toBe(0x07);
    expect(normalized[0].children[0].usage).toBe(0xe0);
    expect(normalized[0].children[0].collectionType).toBe(2);
  });

  it("rejects output reports larger than the max USB HID control-transfer size (65535 bytes)", () => {
    // Use a non-zero reportId so the report ID prefix pushes the on-wire length over 65535 bytes.
    const bigOutputItem = mockItem({ reportSize: 8, reportCount: 65_535 });
    const root = mockCollection({
      outputReports: [mockReport({ reportId: 1, items: [bigOutputItem] })] as unknown as HidReportInfo[],
    });

    expect(() => normalizeCollections([root], { validate: true })).toThrow(
      /output report length 65536 bytes exceeds max USB (?:HID )?control transfer length 65535(?: bytes)?/i,
    );
  });

  it("rejects feature reports larger than the max USB HID control-transfer size (65535 bytes)", () => {
    // Use a non-zero reportId so the report ID prefix pushes the on-wire length over 65535 bytes.
    const bigFeatureItem = mockItem({ reportSize: 8, reportCount: 65_535 });
    const root = mockCollection({
      featureReports: [mockReport({ reportId: 1, items: [bigFeatureItem] })] as unknown as HidReportInfo[],
    });

    expect(() => normalizeCollections([root], { validate: true })).toThrow(
      /feature report length 65536 bytes exceeds max USB (?:HID )?control transfer length 65535(?: bytes)?/i,
    );
  });
});
