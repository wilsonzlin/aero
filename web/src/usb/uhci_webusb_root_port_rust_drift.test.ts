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

function parseRustUsizeConstExpr(source: string, name: string): string {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^(?:pub(?:\([^\)]*\))?\s+)?const ${name}: usize = ([^;]+);$`, "m");
  const match = source.match(re);
  if (!match) throw new Error(`Failed to locate \`${name}: usize\` constant`);
  return match[1]!;
}

describe("UHCI WebUSB root port reservation matches web runtime topology", () => {
  it("reserves a different root port than the external hub so WebUSB + WebHID/synthetic HID can coexist", () => {
    const portsUrl = new URL("../../../crates/aero-wasm/src/webusb_ports.rs", import.meta.url);
    const ports = readFileSync(portsUrl, "utf8");
    const sharedWebusbRootPort = parseRustU8ConstLiteral(ports, "WEBUSB_ROOT_PORT");

    const parseWebusbRootPort = (source: string, label: string): number => {
      const expr = parseRustUsizeConstExpr(source, "WEBUSB_ROOT_PORT").trim();
      if (expr === "crate::webusb_ports::WEBUSB_ROOT_PORT as usize") {
        return sharedWebusbRootPort;
      }
      if (/^\d+$/.test(expr)) {
        return Number(expr);
      }
      throw new Error(`Unexpected ${label} WEBUSB_ROOT_PORT expression: ${expr}`);
    };

    const bridgeUrl = new URL("../../../crates/aero-wasm/src/uhci_controller_bridge.rs", import.meta.url);
    const bridgeRust = readFileSync(bridgeUrl, "utf8");
    const bridgeWebusbRootPort = parseWebusbRootPort(bridgeRust, "UHCI bridge");
    expect(bridgeWebusbRootPort).toBe(sharedWebusbRootPort);

    const runtimeUrl = new URL("../../../crates/aero-wasm/src/uhci_runtime.rs", import.meta.url);
    const runtimeRust = readFileSync(runtimeUrl, "utf8");
    const runtimeWebusbRootPort = parseWebusbRootPort(runtimeRust, "UHCI runtime");
    expect(runtimeWebusbRootPort).toBe(sharedWebusbRootPort);

    expect(sharedWebusbRootPort).toBe(WEBUSB_GUEST_ROOT_PORT);
    expect(sharedWebusbRootPort).not.toBe(EXTERNAL_HUB_ROOT_PORT);
  });
});
