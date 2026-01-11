import test from "node:test";
import assert from "node:assert/strict";

import { normalizeCollections, type HidCollectionInfo } from "../src/hid/webhid_normalize.ts";

const BASE: Omit<HidCollectionInfo, "type"> = {
  usagePage: 1,
  usage: 0,
  children: [],
  inputReports: [],
  outputReports: [],
  featureReports: [],
};

test("normalizeCollections: maps WebHID collection type strings to numeric HID codes", () => {
  const cases = [
    ["physical", 0],
    ["application", 1],
    ["logical", 2],
    ["report", 3],
    ["namedArray", 4],
    ["usageSwitch", 5],
    ["usageModifier", 6],
  ] as const;

  for (const [type, code] of cases) {
    const normalized = normalizeCollections([{ ...BASE, type }]);
    assert.equal(normalized.length, 1);
    assert.equal(normalized[0]!.collectionType, code);
    assert.equal("type" in normalized[0]!, false);
  }
});
