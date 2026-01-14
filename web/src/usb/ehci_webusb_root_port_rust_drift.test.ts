import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { EXTERNAL_HUB_ROOT_PORT, WEBUSB_GUEST_ROOT_PORT } from "./uhci_external_hub";

function parseRustU8Const(source: string, name: string): number {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^const ${name}: u8 = (\d+);$`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`const ${name}: u8\` in crates/aero-wasm/src/ehci_controller_bridge.rs`);
  }
  const value = Number(match[1]);
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 0 || value > 0xff) {
    throw new Error(`Invalid uint8 value for ${name}: ${match[1]}`);
  }
  return value;
}

describe("EHCI WebUSB root port reservation matches web runtime topology", () => {
  it("reserves a different root port than the external hub so WebUSB + WebHID/synthetic HID can coexist", () => {
    const rustUrl = new URL("../../../crates/aero-wasm/src/ehci_controller_bridge.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    const ehciWebusbRootPort = parseRustU8Const(rust, "WEBUSB_ROOT_PORT");
    expect(ehciWebusbRootPort).toBe(WEBUSB_GUEST_ROOT_PORT);
    expect(ehciWebusbRootPort).not.toBe(EXTERNAL_HUB_ROOT_PORT);
  });
});

