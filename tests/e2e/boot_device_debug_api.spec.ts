import { expect, test } from "@playwright/test";

test("Boot device debug API is installed on window.aero.debug", async ({ page }) => {
  test.setTimeout(20_000);
  await page.goto("/", { waitUntil: "load" });

  await page.waitForFunction(() => {
    const dbg = (window as any).aero?.debug;
    return typeof dbg?.getBootDisks === "function" && typeof dbg?.getMachineCpuActiveBootDevice === "function";
  });

  const snapshot = await page.evaluate(() => {
    const dbg = (window as any).aero?.debug;
    return {
      bootDisks: dbg?.getBootDisks?.() ?? "missing",
      active: dbg?.getMachineCpuActiveBootDevice?.() ?? "missing",
    };
  });

  expect(snapshot.bootDisks).toBe(null);
  expect(snapshot.active).toBe(null);
});

