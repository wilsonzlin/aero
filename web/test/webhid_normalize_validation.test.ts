import test from "node:test";
import assert from "node:assert/strict";

import {
  normalizeCollections,
  type HidCollectionInfo,
  type HidReportItem,
} from "../src/hid/webhid_normalize.ts";

const BASE_ITEM: HidReportItem = {
  usagePage: 1,
  usages: [],
  usageMinimum: 0,
  usageMaximum: 0,
  reportSize: 8,
  reportCount: 1,
  unitExponent: 0,
  unit: 0,
  logicalMinimum: 0,
  logicalMaximum: 0,
  physicalMinimum: 0,
  physicalMaximum: 0,
  strings: [],
  stringMinimum: 0,
  stringMaximum: 0,
  designators: [],
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

function baseCollection(): HidCollectionInfo {
  return {
    usagePage: 1,
    usage: 2,
    type: "application",
    children: [],
    inputReports: [],
    outputReports: [],
    featureReports: [],
  };
}

test("normalizeCollections(validate): rejects mixed reportIds", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 1, items: [BASE_ITEM] }],
      outputReports: [{ reportId: 0, items: [BASE_ITEM] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /collections\[0\]\.outputReports\[0\]/,
  });
});

test("normalizeCollections(validate): rejects collection usagePage outside u16 range with a path", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      usagePage: 0x1_0000,
      inputReports: [{ reportId: 0, items: [BASE_ITEM] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /usagePage.*collections\[0\]/,
  });
});

test("normalizeCollections(validate): rejects isRange items with usages out of order", () => {
  // Use a huge `usages` list so the normalizer does not attempt to derive
  // usageMinimum/Maximum from it (bounded by MAX_RANGE_CONTIGUITY_CHECK_LEN),
  // and therefore preserves the explicit out-of-order bounds.
  const hugeUsages = Array.from({ length: 4097 }, (_v, i) => i);
  const rangeItem: HidReportItem = { ...BASE_ITEM, isRange: true, usageMinimum: 10, usageMaximum: 2, usages: hugeUsages };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [rangeItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects usages values outside u16 range with a path", () => {
  const badItem: HidReportItem = { ...BASE_ITEM, usages: [-1] };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [badItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /usages\[0\].*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects item usagePage outside u16 range with a path", () => {
  const badItem: HidReportItem = { ...BASE_ITEM, usagePage: 0x1_0000 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [badItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /usagePage.*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects non-boolean isAbsolute with a path", () => {
  const badItem = { ...BASE_ITEM, isAbsolute: 1 } as unknown as HidReportItem;
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [badItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /isAbsolute.*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): coerces non-numeric strings/designators locals to empty arrays", () => {
  const badItem = {
    ...BASE_ITEM,
    strings: ["foo"] as unknown as never,
    designators: ["bar"] as unknown as never,
  } as unknown as HidReportItem;
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [badItem] }],
    },
  ];

  const normalized = normalizeCollections(collections, { validate: true });
  const item = normalized[0]!.inputReports[0]!.items[0]!;
  assert.deepStrictEqual(item.strings, []);
  assert.deepStrictEqual(item.designators, []);
});

test("normalizeCollections(validate): rejects logicalMinimum outside i32 with a path", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [
        {
          reportId: 0,
          items: [{ ...BASE_ITEM, logicalMinimum: 2147483648, logicalMaximum: 2147483648 }],
        },
      ],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /logicalMinimum.*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects reportSize 0 with an item path", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [{ ...BASE_ITEM, reportSize: 0 }] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /reportSize.*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects unitExponent out of range with an item path", () => {
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [{ ...BASE_ITEM, unitExponent: 8 }] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /unitExponent.*collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects reportSize*reportCount u32 overflow with an item path", () => {
  // Use a huge reportCount that is still a safe integer (so it passes type checks) but causes
  // reportSize*reportCount to exceed the u32 bit-length cap in the validator.
  const hugeItem: HidReportItem = { ...BASE_ITEM, reportSize: 255, reportCount: 20_000_000 };

  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      featureReports: [{ reportId: 0, items: [hugeItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /reportSize\*reportCount overflows u32.*collections\[0\]\.featureReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects input reports longer than a full-speed interrupt packet", () => {
  const bigItem: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 65 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 0, items: [bigItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): rejects reportId prefixes that push input reports over 64 bytes", () => {
  const bigItem: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 64 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      inputReports: [{ reportId: 1, items: [bigItem] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /collections\[0\]\.inputReports\[0\]\.items\[0\]/,
  });
});

test("normalizeCollections(validate): accepts output reports longer than a full-speed interrupt packet", () => {
  const bigItem: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 65 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      outputReports: [{ reportId: 0, items: [bigItem] }],
    },
  ];

  assert.doesNotThrow(() => normalizeCollections(collections, { validate: true }));
});

test("normalizeCollections(validate): rejects output reports longer than u16::MAX bytes", () => {
  const maxItem: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 65_535 };
  const plusOne: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 1 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      outputReports: [{ reportId: 0, items: [maxItem, plusOne] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /output report length 65536 bytes exceeds max USB control transfer length 65535.*collections\[0\]\.outputReports\[0\]\.items\[1\]/,
  });
});

test("normalizeCollections(validate): rejects feature reports longer than u16::MAX bytes", () => {
  const maxItem: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 65_535 };
  const plusOne: HidReportItem = { ...BASE_ITEM, reportSize: 8, reportCount: 1 };
  const collections: HidCollectionInfo[] = [
    {
      ...baseCollection(),
      featureReports: [{ reportId: 0, items: [maxItem, plusOne] }],
    },
  ];

  assert.throws(() => normalizeCollections(collections, { validate: true }), {
    message: /feature report length 65536 bytes exceeds max USB control transfer length 65535.*collections\[0\]\.featureReports\[0\]\.items\[1\]/,
  });
});

test("normalizeCollections: rejects excessive collection depth with a path", () => {
  const root = baseCollection() as unknown as { children: any[] };
  let current: { children: any[] } = root;
  for (let i = 0; i < 32; i++) {
    const child = baseCollection() as unknown as { children: any[] };
    current.children = [child];
    current = child;
  }

  let expectedPath = "collections[0]";
  for (let i = 0; i < 32; i++) expectedPath += ".children[0]";

  try {
    normalizeCollections([root as unknown as HidCollectionInfo]);
    assert.fail("expected normalizeCollections() to throw");
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    assert.match(message, /max depth/i);
    assert.ok(message.endsWith(`(at ${expectedPath})`), message);
  }
});

test("normalizeCollections: rejects cyclic collection graphs with a path", () => {
  const root = baseCollection() as unknown as { children: any[] };
  root.children = [root];

  try {
    normalizeCollections([root as unknown as HidCollectionInfo]);
    assert.fail("expected normalizeCollections() to throw");
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    assert.match(message, /cycle/i);
    assert.ok(message.endsWith("(at collections[0].children[0])"), message);
  }
});
