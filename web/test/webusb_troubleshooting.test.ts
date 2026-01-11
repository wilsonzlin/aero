import assert from "node:assert/strict";
import test from "node:test";

import { explainWebUsbError } from "../src/platform/webusb_troubleshooting.ts";

function withUserAgent<T>(userAgent: string, fn: () => T): T {
  if (typeof navigator === "undefined") return fn();

  const hadOwn = Object.prototype.hasOwnProperty.call(navigator, "userAgent");
  const prev = navigator.userAgent;

  Object.defineProperty(navigator, "userAgent", { value: userAgent, configurable: true });
  try {
    return fn();
  } finally {
    if (hadOwn) {
      Object.defineProperty(navigator, "userAgent", { value: prev, configurable: true });
    } else {
      delete (navigator as unknown as { userAgent?: unknown }).userAgent;
    }
  }
}

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
  const res = withUserAgent("Node.js/25", () => explainWebUsbError("NetworkError: Unable to claim interface."));

  assert.ok(res.title.toLowerCase().includes("claim") || res.title.toLowerCase().includes("communication"));
  assert.ok(res.hints.some((hint) => hint.includes("usb-internals")));
  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("udev")));
});

test("explainWebUsbError: TypeError filter validation suggests adding vendorId/productId filters", () => {
  const res = explainWebUsbError({
    name: "TypeError",
    message: "Failed to execute 'requestDevice' on 'USB': At least one filter must be specified.",
  });

  assert.ok(res.title.toLowerCase().includes("webusb"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("vendorid")));
});

test("explainWebUsbError: prefers DOMException name from Error.cause when present", () => {
  const err = new Error("Failed to open USB device", {
    cause: { name: "NetworkError", message: "Unable to claim interface." },
  });

  const res = withUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", () => explainWebUsbError(err));
  assert.ok(res.title.toLowerCase().includes("communication") || res.title.toLowerCase().includes("claim"));
  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
});

test("explainWebUsbError: parses formatted '<-' error chains in strings", () => {
  const err = new Error("Error: Failed to open USB device <- NetworkError: Unable to claim interface.");
  const res = withUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", () => explainWebUsbError(err));
  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
});

test("explainWebUsbError: Windows driver hints omit Linux udev guidance", () => {
  const res = withUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", () =>
    explainWebUsbError("NetworkError: Unable to claim interface."),
  );

  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
  assert.ok(!res.hints.some((hint) => hint.toLowerCase().includes("udev")));
});

test("explainWebUsbError: Linux permission hints omit Windows WinUSB guidance", () => {
  const res = withUserAgent("Mozilla/5.0 (X11; Linux x86_64)", () => explainWebUsbError("NetworkError: Unable to claim interface."));

  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("udev")));
  assert.ok(!res.hints.some((hint) => hint.includes("WinUSB")));
});

test("explainWebUsbError: Android hints omit desktop WinUSB/udev guidance", () => {
  const res = withUserAgent("Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 Chrome/120.0.0.0 Mobile", () =>
    explainWebUsbError("NetworkError: Unable to claim interface."),
  );

  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("android")));
  assert.ok(!res.hints.some((hint) => hint.includes("WinUSB")));
  assert.ok(!res.hints.some((hint) => hint.toLowerCase().includes("udev")));
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

test("explainWebUsbError: AbortError indicates cancellation / retry requestDevice()", () => {
  const res = explainWebUsbError({
    name: "AbortError",
    message: "The user aborted a request.",
  });

  assert.ok(res.title.toLowerCase().includes("abort") || res.title.toLowerCase().includes("cancel"));
  assert.ok(res.hints.some((hint) => hint.includes("requestDevice()")));
});

test("explainWebUsbError: NotSupportedError mentions isochronous limitations", () => {
  const res = explainWebUsbError({
    name: "NotSupportedError",
    message: "Isochronous transfers are not supported.",
  });

  assert.ok(res.title.toLowerCase().includes("not supported") || res.title.toLowerCase().includes("isochronous"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("isochronous")));
});

test("explainWebUsbError: InvalidAccessError hints at endpoints/interfaces", () => {
  const res = explainWebUsbError({
    name: "InvalidAccessError",
    message: "The endpoint number is invalid.",
  });

  assert.ok(res.title.toLowerCase().includes("rejected") || res.title.toLowerCase().includes("endpoint"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("endpoint")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("claimed")));
});

test("explainWebUsbError: OperationError suggests reset/replug", () => {
  const res = explainWebUsbError({
    name: "OperationError",
    message: "USB transfer failed.",
  });

  assert.ok(res.title.toLowerCase().includes("operation"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("replug") || hint.toLowerCase().includes("reset")));
});

test("explainWebUsbError: DataCloneError suggests keeping WebUSB on the main thread", () => {
  const res = explainWebUsbError({
    name: "DataCloneError",
    message: "USBDevice could not be cloned.",
  });

  assert.ok(res.title.toLowerCase().includes("worker") || res.title.toLowerCase().includes("transferred"));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("structured")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("main thread")));
});

test("explainWebUsbError: NotReadableError includes driver/permissions hints", () => {
  const res = withUserAgent("Node.js/25", () =>
    explainWebUsbError({
      name: "NotReadableError",
      message: "Failed to open device.",
    }),
  );

  assert.ok(res.title.toLowerCase().includes("access") || res.title.toLowerCase().includes("open"));
  assert.ok(res.hints.some((hint) => hint.includes("WinUSB")));
  assert.ok(res.hints.some((hint) => hint.toLowerCase().includes("udev")));
});
