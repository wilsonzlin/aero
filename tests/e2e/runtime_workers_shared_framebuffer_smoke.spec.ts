import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("runtime workers smoke: CPU publishes shared framebuffer frames and GPU worker presents them", async ({
  page,
  browserName,
}) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/runtime-workers-shared-framebuffer-smoke.html", { waitUntil: "load" });
  await waitForReady(page);

  const result = await page.evaluate(() => (window as any).__aeroTest);
  expect(result).toBeTruthy();
  if (!result || typeof result !== "object") {
    throw new Error("Missing __aeroTest result");
  }
  if ((result as any).error) {
    throw new Error(String((result as any).error));
  }

  expect((result as any).pass).toBe(true);
  expect(Array.isArray((result as any).hashes)).toBe(true);

  const metrics = (result as any).metrics;
  expect(metrics).toBeTruthy();
  expect(metrics.framesPresented).toBeGreaterThan(0);
  expect(metrics.framesReceived).toBeGreaterThan(0);
});
