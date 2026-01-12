import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("runtime workers: CPU worker cursor messages are forwarded to the GPU presenter worker", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/runtime-workers-cursor-forwarding-smoke.html", { waitUntil: "load" });
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
  expect((result as any).sample).toEqual([0, 0, 255, 255]);
});
