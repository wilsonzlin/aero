import assert from "node:assert/strict";
import test from "node:test";

import { explainWebUsbError } from "../src/platform/webusb_troubleshooting.ts";

test("explainWebUsbError: NotAllowedError includes a user-gesture hint", () => {
  const res = explainWebUsbError({
    name: "NotAllowedError",
    message: "Must be handling a user gesture to show a permission request.",
  });

  assert.ok(res.title.toLowerCase().includes("permission") || res.title.toLowerCase().includes("gesture"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("user gesture")));
});

test("explainWebUsbError: InvalidStateError suggests device.open() and selectConfiguration()", () => {
  const res = explainWebUsbError({
    name: "InvalidStateError",
    message: "The device must be opened first.",
  });

  assert.ok(res.hints.some((hint) => hint.includes("device.open()")));
  assert.ok(res.hints.some((hint) => hint.includes("selectConfiguration")));
});

test("explainWebUsbError: claimInterface failures include WinUSB + udev hints", () => {
  const res = explainWebUsbError("NetworkError: Unable to claim interface.");

  assert.ok(res.title.toLowerCase().includes("claim") || res.title.toLowerCase().includes("communication"));
  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("udev")));
});

test("explainWebUsbError: SecurityError mentions protected interface classes", () => {
  const res = explainWebUsbError({
    name: "SecurityError",
    message: "Access denied. Protected interface class.",
  });

  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("protected")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("hid")));
});

test("explainWebUsbError: includes secure-context hint when isSecureContext is false", () => {
  const prev = (globalThis as typeof globalThis & { isSecureContext?: unknown }).isSecureContext;
  (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext = false;

  try {
    const res = explainWebUsbError({ name: "SecurityError", message: "Access denied." });
    assert.ok(res.hints.some((hint) => hint.includes("https://") || hint.includes("localhost")));
  } finally {
    if (prev === undefined) {
      delete (globalThis as typeof globalThis & { isSecureContext?: unknown }).isSecureContext;
    } else {
      (globalThis as typeof globalThis & { isSecureContext?: unknown }).isSecureContext = prev;
    }
  }
});

