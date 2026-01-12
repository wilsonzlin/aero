import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("shared framebuffer dirty tiles smoke: partial uploads keep untouched regions", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/shared-framebuffer-dirty-tiles-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(() => {
    return (window as any).__aeroTest;
  });

  expect(result).toBeTruthy();
  if (!result || typeof result !== "object") {
    throw new Error("Missing __aeroTest result");
  }
  if ((result as any).error) {
    throw new Error(String((result as any).error));
  }

  expect((result as any).backend).toBe("webgl2_raw");
  expect((result as any).uploadBytesFullEstimate).toBe(64 * 64 * 4);
  expect((result as any).uploadBytesMax).toBeGreaterThan(0);
  expect((result as any).uploadBytesMax).toBeLessThan((result as any).uploadBytesFullEstimate);
  expect((result as any).uploadBytesMax).toBeLessThanOrEqual(10_000);

  expect((result as any).pass).toBe(true);

  // Sanity: green frame and mixed frame should differ.
  expect((result as any).hashes.gotGreen).not.toBe((result as any).hashes.gotMixed);

  // Spot-check colors.
  expect((result as any).samples.greenTopLeft).toEqual([0, 255, 0, 255]);
  expect((result as any).samples.mixedTopLeft).toEqual([255, 0, 0, 255]);
  expect((result as any).samples.mixedTopRight).toEqual([0, 255, 0, 255]);
});
