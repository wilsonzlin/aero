import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("shared framebuffer: cursor overlay blends in presenter worker", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/shared-framebuffer-cursor-overlay.html", { waitUntil: "load" });
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

  expect((result as any).pass).toBe(true);
  expect((result as any).sample).toEqual([128, 0, 127, 255]);
});
