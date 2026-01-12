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

    const expectedByCode = new Map<string, number>();
    for (const entry of entries) {
      if (expectedByCode.has(entry.code)) {
        throw new Error(
          `duplicate fixture entry for KeyboardEvent.code=${JSON.stringify(entry.code)}`,
        );
      }
      const expected = parseHexU8(entry.usage);
      expectedByCode.set(entry.code, expected);
      expect(keyboardCodeToHidUsage(entry.code)).toBe(expected);
    }

    // Ensure the TS-side mapping does not "accidentally" support additional codes without the
    // shared fixture being updated. Use the PS/2 scancode list as a stable, project-wide superset
    // of common `KeyboardEvent.code` values.
    const scancodesUrl = new URL("../../../tools/gen_scancodes/scancodes.json", import.meta.url);
    const scancodes = JSON.parse(readFileSync(scancodesUrl, "utf8")) as {
      ps2_set2: Record<string, unknown>;
    };
    for (const code of Object.keys(scancodes.ps2_set2)) {
      expect(keyboardCodeToHidUsage(code)).toBe(expectedByCode.get(code) ?? null);
    }

    expect(keyboardCodeToHidUsage("NoSuchKey")).toBeNull();
  });
});
