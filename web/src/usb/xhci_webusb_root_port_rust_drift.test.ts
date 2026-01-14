import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { EXTERNAL_HUB_ROOT_PORT, WEBUSB_GUEST_ROOT_PORT } from "./uhci_external_hub";

function parseRustU8ConstExpr(source: string, name: string): string {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^(?:pub(?:\([^\)]*\))?\s+)?const ${name}: u8 = ([^;]+);$`, "m");
  const match = source.match(re);
  if (!match) throw new Error(`Failed to locate \`${name}: u8\` constant`);
  return match[1]!;
}

function parseRustU8ConstLiteral(source: string, name: string): number {
  const expr = parseRustU8ConstExpr(source, name);
  const match = expr.match(/^(\d+)$/);
  if (!match) throw new Error(`Expected ${name} to be a numeric literal, got: ${expr}`);
  const value = Number(match[1]);
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 0 || value > 0xff) {
    throw new Error(`Invalid uint8 value for ${name}: ${match[1]}`);
  }
  return value;
}

describe("xHCI WebUSB root port reservation matches web runtime topology", () => {
  it("reserves a different root port than the external hub so WebUSB + WebHID/synthetic HID can coexist", () => {
    const rustUrl = new URL("../../../crates/aero-wasm/src/xhci_controller_bridge.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    const portsUrl = new URL("../../../crates/aero-wasm/src/webusb_ports.rs", import.meta.url);
    const ports = readFileSync(portsUrl, "utf8");
    const sharedWebusbRootPort = parseRustU8ConstLiteral(ports, "WEBUSB_ROOT_PORT");

    const xhciWebusbRootPortExpr = parseRustU8ConstExpr(rust, "WEBUSB_ROOT_PORT");
    let xhciWebusbRootPort: number;
    if (xhciWebusbRootPortExpr === "crate::webusb_ports::WEBUSB_ROOT_PORT") {
      xhciWebusbRootPort = sharedWebusbRootPort;
    } else if (/^\d+$/.test(xhciWebusbRootPortExpr)) {
      xhciWebusbRootPort = Number(xhciWebusbRootPortExpr);
    } else {
      throw new Error(
        `Unexpected xHCI WEBUSB_ROOT_PORT expression: ${xhciWebusbRootPortExpr}`,
      );
    }
    expect(xhciWebusbRootPort).toBe(sharedWebusbRootPort);

    expect(sharedWebusbRootPort).toBe(WEBUSB_GUEST_ROOT_PORT);
    expect(sharedWebusbRootPort).not.toBe(EXTERNAL_HUB_ROOT_PORT);
  });
});
