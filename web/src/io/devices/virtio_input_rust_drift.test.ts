import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { hidConsumerUsageToLinuxKeyCode, hidUsageToLinuxKeyCode } from "./virtio_input";

function parseIntLiteral(value: string): number {
  const trimmed = value.trim();
  const parsed = trimmed.startsWith("0x") || trimmed.startsWith("0X") ? Number.parseInt(trimmed.slice(2), 16) : Number.parseInt(trimmed, 10);
  if (!Number.isFinite(parsed) || !Number.isInteger(parsed)) {
    throw new Error(`Invalid numeric literal: ${value}`);
  }
  return parsed;
}

function parseRustU16Consts(source: string): Map<string, number> {
  // Keep the matcher intentionally strict so drift is caught early if we reformat the source.
  const re = /^\s*pub const ([A-Z0-9_]+): u16 = (0x[0-9A-Fa-f]+|\d+);$/gm;
  const out = new Map<string, number>();
  for (;;) {
    const match = re.exec(source);
    if (!match) break;
    const name = match[1]!;
    const value = parseIntLiteral(match[2]!);
    out.set(name, value);
  }
  return out;
}

function extractRustFnBody(source: string, fnName: string): string {
  const start = source.indexOf(`fn ${fnName}`);
  if (start < 0) throw new Error(`Failed to locate Rust function \`${fnName}\``);

  const open = source.indexOf("{", start);
  if (open < 0) throw new Error(`Failed to locate opening brace for \`${fnName}\``);

  let depth = 0;
  for (let i = open; i < source.length; i++) {
    const ch = source[i];
    if (ch === "{") depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) return source.slice(open + 1, i);
    }
  }
  throw new Error(`Failed to locate closing brace for \`${fnName}\``);
}

function parseRustHexMatchMap(body: string): Map<number, string> {
  const re = /^\s*(0x[0-9A-Fa-f]+)\s*=>\s*([A-Z0-9_]+)\s*,(?:\s*\/\/.*)?$/gm;
  const out = new Map<number, string>();
  for (;;) {
    const match = re.exec(body);
    if (!match) break;
    const key = parseIntLiteral(match[1]!);
    const value = match[2]!;
    if (out.has(key)) throw new Error(`Duplicate match arm for 0x${key.toString(16)}`);
    out.set(key, value);
  }
  return out;
}

describe("virtio-input HID usage → Linux key mapping matches Rust machine runtime", () => {
  it("keeps the keyboard HID usage mapping aligned with aero-machine::Machine::inject_input_batch", () => {
    const virtioInputRustUrl = new URL("../../../../crates/aero-virtio/src/devices/input.rs", import.meta.url);
    const virtioInputRust = readFileSync(virtioInputRustUrl, "utf8");
    const linuxKeys = parseRustU16Consts(virtioInputRust);

    const machineRustUrl = new URL("../../../../crates/aero-machine/src/lib.rs", import.meta.url);
    const machineRust = readFileSync(machineRustUrl, "utf8");
    const rustMap = parseRustHexMatchMap(extractRustFnBody(machineRust, "hid_usage_to_linux_key"));

    const fixtureUrl = new URL("../../../../docs/fixtures/hid_usage_keyboard.json", import.meta.url);
    const fixture: Array<{ code: string; usage: string }> = JSON.parse(readFileSync(fixtureUrl, "utf8"));

    for (const { code, usage } of fixture) {
      const hidUsage = parseIntLiteral(usage);
      const tsLinuxKey = hidUsageToLinuxKeyCode(hidUsage);
      expect(tsLinuxKey).not.toBeNull();

      const rustKeyName = rustMap.get(hidUsage);
      expect(rustKeyName).toBeDefined();
      const rustLinuxKey = linuxKeys.get(rustKeyName!);
      expect(rustLinuxKey).toBeDefined();

      expect(rustLinuxKey).toBe(tsLinuxKey);
      void code;
    }
  });

  it("keeps the Consumer Control media-key mapping aligned with aero-machine::Machine::inject_input_batch", () => {
    const virtioInputRustUrl = new URL("../../../../crates/aero-virtio/src/devices/input.rs", import.meta.url);
    const virtioInputRust = readFileSync(virtioInputRustUrl, "utf8");
    const linuxKeys = parseRustU16Consts(virtioInputRust);

    const machineRustUrl = new URL("../../../../crates/aero-machine/src/lib.rs", import.meta.url);
    const machineRust = readFileSync(machineRustUrl, "utf8");
    const rustMap = parseRustHexMatchMap(extractRustFnBody(machineRust, "hid_consumer_usage_to_linux_key"));

    const fixtureUrl = new URL("../../../../docs/fixtures/hid_usage_consumer.json", import.meta.url);
    const fixture: Array<{ code: string; usage: string }> = JSON.parse(readFileSync(fixtureUrl, "utf8"));

    const tsMap = new Map<number, number>();
    for (const { usage } of fixture) {
      const usageId = parseIntLiteral(usage);
      const linuxKey = hidConsumerUsageToLinuxKeyCode(usageId);
      if (linuxKey !== null) tsMap.set(usageId, linuxKey);
    }

    // 1) Every TS-supported Consumer usage must exist in Rust and map to the same KEY_* value.
    for (const [usageId, tsLinuxKey] of tsMap) {
      const rustKeyName = rustMap.get(usageId);
      expect(rustKeyName).toBeDefined();
      const rustLinuxKey = linuxKeys.get(rustKeyName!);
      expect(rustLinuxKey).toBeDefined();
      expect(rustLinuxKey).toBe(tsLinuxKey);
    }

    // 2) Every Rust-supported Consumer usage must exist in TS (to avoid missing media keys in the web runtime).
    for (const [usageId] of rustMap) {
      expect(tsMap.has(usageId)).toBe(true);
    }
  });
});
