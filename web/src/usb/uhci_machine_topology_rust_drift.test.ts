import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
  UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
  UHCI_SYNTHETIC_HID_HUB_PORT_COUNT,
  UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
  UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
  WEBUSB_GUEST_ROOT_PORT,
} from "./uhci_external_hub";

function parseRustU8ConstLiteral(source: string, name: string): number {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^\s*(?:pub(?:\([^\)]*\))?\s+)?const ${name}: u8 = (\d+);$`, "m");
  const match = source.match(re);
  if (!match) throw new Error(`Failed to locate \`${name}: u8\` constant`);
  const value = Number(match[1]);
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 0 || value > 0xff) {
    throw new Error(`Invalid uint8 value for ${name}: ${match[1]}`);
  }
  return value;
}

describe("aero-machine UHCI topology constants match the web runtime", () => {
  it("keeps the reserved root ports + synthetic HID hub ports consistent", () => {
    const machineUrl = new URL("../../../crates/aero-machine/src/lib.rs", import.meta.url);
    const rust = readFileSync(machineUrl, "utf8");

    const externalHubRootPort = parseRustU8ConstLiteral(rust, "UHCI_EXTERNAL_HUB_ROOT_PORT");
    const webusbRootPort = parseRustU8ConstLiteral(rust, "UHCI_WEBUSB_ROOT_PORT");
    const externalHubPortCount = parseRustU8ConstLiteral(rust, "UHCI_EXTERNAL_HUB_PORT_COUNT");

    const keyboardHubPort = parseRustU8ConstLiteral(rust, "UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT");
    const mouseHubPort = parseRustU8ConstLiteral(rust, "UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT");
    const gamepadHubPort = parseRustU8ConstLiteral(rust, "UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT");
    const consumerHubPort = parseRustU8ConstLiteral(rust, "UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT");
    const syntheticHubPortCount = parseRustU8ConstLiteral(rust, "UHCI_SYNTHETIC_HID_HUB_PORT_COUNT");

    expect(externalHubRootPort).toBe(EXTERNAL_HUB_ROOT_PORT);
    expect(webusbRootPort).toBe(WEBUSB_GUEST_ROOT_PORT);
    expect(webusbRootPort).not.toBe(externalHubRootPort);

    expect(externalHubPortCount).toBe(DEFAULT_EXTERNAL_HUB_PORT_COUNT);

    expect(keyboardHubPort).toBe(UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT);
    expect(mouseHubPort).toBe(UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT);
    expect(gamepadHubPort).toBe(UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT);
    expect(consumerHubPort).toBe(UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT);
    expect(syntheticHubPortCount).toBe(UHCI_SYNTHETIC_HID_HUB_PORT_COUNT);
  });
});

