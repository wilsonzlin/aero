import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("vga presenter smoke: Canvas2D blit shows correct quadrants", async ({ page }) => {
  await page.goto("/web/vga-presenter-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api) throw new Error("__aeroTest missing");
    if (api.error) throw new Error(api.error);
    if (typeof api.samplePixels !== "function") throw new Error("samplePixels missing");
    const samples = await api.samplePixels();
    return { transport: api.transport ?? "unknown", samples };
  });

  expect(result.transport === "shared" || result.transport === "copy").toBe(true);
  expect(result.samples.width).toBe(64);
  expect(result.samples.height).toBe(64);

  expect(result.samples.topLeft).toEqual([255, 0, 0, 255]);
  expect(result.samples.topRight).toEqual([0, 255, 0, 255]);
  expect(result.samples.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(result.samples.bottomRight).toEqual([255, 255, 255, 255]);
});
