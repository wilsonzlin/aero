import { basename } from "node:path";
import { describe, expect, it } from "vitest";

import viteConfig from "../vite.config.ts";

describe("vite.config", () => {
  it("includes the WebUSB diagnostics page in the production build inputs", () => {
    const input = (viteConfig as { build?: { rollupOptions?: { input?: unknown } } }).build?.rollupOptions?.input;

    expect(input).toBeTruthy();
    expect(typeof input).toBe("object");
    expect(Array.isArray(input)).toBe(false);

    const pages = input as Record<string, unknown>;
    expect(pages).toHaveProperty("webusb_diagnostics");
    expect(basename(String(pages.webusb_diagnostics))).toBe("webusb_diagnostics.html");
  });
});
