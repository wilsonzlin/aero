import { expect, test, type Page } from "@playwright/test";

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

test("shared framebuffer smoke: CPU publishes frames via SharedArrayBuffer and GPU worker presents them", async ({
  page,
  browserName,
}) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/shared-framebuffer-smoke.html", { waitUntil: "load" });
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
  expect((result as any).hashes.first).not.toBe((result as any).hashes.second);

  const first = (result as any).samples?.first ?? null;
  const second = (result as any).samples?.second ?? null;
  expect(first).not.toBeNull();
  expect(second).not.toBeNull();

  const asKey = (rgba: number[]) => rgba.join(",");
  const ok = new Set(["0,255,0,255", "255,0,0,255"]);
  expect(ok.has(asKey(first))).toBe(true);
  expect(ok.has(asKey(second))).toBe(true);
});
