import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { XHCI_MAX_HUB_PORT_COUNT, XHCI_MAX_ROUTE_TIER_COUNT } from "./xhci_hid_topology";

function parseRustConstLiteral(source: string, name: string, ty: "u8" | "usize"): number {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^\s*(?:pub\s+)?const ${name}: ${ty} = (\d+);$`, "m");
  const match = source.match(re);
  if (!match) throw new Error(`Failed to locate \`${name}: ${ty}\` constant`);
  const value = Number(match[1]);
  if (!Number.isFinite(value) || !Number.isInteger(value)) {
    throw new Error(`Invalid numeric literal for ${name}: ${match[1]}`);
  }
  return value;
}

describe("xHCI route string limits match Rust implementation", () => {
  it("keeps max downstream hub port count and hub tier depth in sync", () => {
    const rustUrl = new URL("../../../crates/aero-usb/src/xhci/context.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    const maxDepth = parseRustConstLiteral(rust, "XHCI_ROUTE_STRING_MAX_DEPTH", "usize");
    const maxPort = parseRustConstLiteral(rust, "XHCI_ROUTE_STRING_MAX_PORT", "u8");

    expect(maxDepth).toBe(XHCI_MAX_ROUTE_TIER_COUNT);
    expect(maxPort).toBe(XHCI_MAX_HUB_PORT_COUNT);
  });
});

