import { describe, expect, it } from "vitest";

import { explainWebUsbError } from "./webusb_troubleshooting";

describe("webusb_troubleshooting", () => {
  it("explains NotAllowedError with user gesture hints", () => {
    const explanation = explainWebUsbError({
      name: "NotAllowedError",
      message: "The requestDevice() call failed because it was not called from a user gesture.",
    });

    expect(explanation.title).toContain("permission");
    expect(explanation.hints.some((h) => h.includes("requestDevice") && h.includes("user gesture"))).toBe(true);
  });

  it("explains SecurityError with secure-context + protected-class hints", () => {
    const explanation = explainWebUsbError({
      name: "SecurityError",
      message: "This must be called from a secure context.",
    });

    expect(explanation.title).toContain("security");
    expect(explanation.hints.some((h) => h.includes("secure context"))).toBe(true);
    expect(explanation.hints.some((h) => h.includes("protected") && h.includes("WebUSB"))).toBe(true);
  });

  it("explains insecure context even when error name is unknown", () => {
    const original = (globalThis as unknown as { isSecureContext?: unknown }).isSecureContext;
    try {
      (globalThis as unknown as { isSecureContext?: boolean }).isSecureContext = false;
      const explanation = explainWebUsbError({ name: "TypeError", message: "some error" });
      expect(explanation.hints.some((h) => h.includes("secure context"))).toBe(true);
    } finally {
      // Restore or delete the property to avoid polluting other tests.
      if (typeof original === "undefined") {
        delete (globalThis as unknown as { isSecureContext?: unknown }).isSecureContext;
      } else {
        (globalThis as unknown as { isSecureContext?: unknown }).isSecureContext = original;
      }
    }
  });

  it("never throws on non-error inputs", () => {
    const explanation = explainWebUsbError(123);
    expect(typeof explanation.title).toBe("string");
    expect(Array.isArray(explanation.hints)).toBe(true);
    expect(explanation.hints.length).toBeGreaterThan(0);
  });
});

