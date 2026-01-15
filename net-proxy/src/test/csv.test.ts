import test from "node:test";
import assert from "node:assert/strict";
import { splitCommaSeparatedList } from "../csv";

test("splitCommaSeparatedList trims and skips empty entries", () => {
  assert.deepEqual(splitCommaSeparatedList(" a, b ,, c "), ["a", "b", "c"]);
  assert.deepEqual(splitCommaSeparatedList(""), []);
  assert.deepEqual(splitCommaSeparatedList(" , , "), []);
});

test("splitCommaSeparatedList enforces maxItems", () => {
  assert.throws(() => splitCommaSeparatedList("a,b,c", { maxLen: 32, maxItems: 2 }), /Too many entries/);
});

test("splitCommaSeparatedList enforces maxLen", () => {
  assert.throws(() => splitCommaSeparatedList("x".repeat(10), { maxLen: 9 }), /Value too long/);
});
