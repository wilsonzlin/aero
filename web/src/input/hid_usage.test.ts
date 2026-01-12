import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { keyboardCodeToHidUsage } from "./hid_usage";

type FixtureEntry = {
  code: string;
  usage: string;
};

function parseHexU8(s: string): number {
  const trimmed = s.startsWith("0x") || s.startsWith("0X") ? s.slice(2) : s;
  const n = Number.parseInt(trimmed, 16);
  if (!Number.isFinite(n) || Number.isNaN(n) || n < 0 || n > 0xff) {
    throw new Error(`invalid u8 hex literal ${JSON.stringify(s)}`);
  }
  return n;
}

describe("keyboardCodeToHidUsage", () => {
  it("matches the shared fixture (docs/fixtures/hid_usage_keyboard.json)", () => {
    const fixtureUrl = new URL("../../../docs/fixtures/hid_usage_keyboard.json", import.meta.url);
    const entries = JSON.parse(readFileSync(fixtureUrl, "utf8")) as FixtureEntry[];
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);

    for (const entry of entries) {
      expect(keyboardCodeToHidUsage(entry.code)).toBe(parseHexU8(entry.usage));
    }

    expect(keyboardCodeToHidUsage("NoSuchKey")).toBeNull();
  });
});

