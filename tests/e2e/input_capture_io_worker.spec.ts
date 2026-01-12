import { expect, test } from "@playwright/test";

const fixtureUrl = "/tests/e2e/fixtures/input_capture_io_worker.html";

test("IO worker receives batched input events", async ({ page }) => {
  await page.goto(fixtureUrl);

  await page.waitForFunction(() => {
    const status = (globalThis as any).__ioStatus as Int32Array | undefined;
    const idx = (globalThis as any).__ioReadyIndex as number | undefined;
    if (!status || idx === undefined) return false;
    return Atomics.load(status, idx) === 1;
  });

  await page.click("#emu");

  await page.keyboard.down("KeyA");
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  await page.waitForFunction(() => {
    const status = (globalThis as any).__ioStatus as Int32Array | undefined;
    const idx = (globalThis as any).__ioInputEventCounterIndex as number | undefined;
    if (!status || idx === undefined) return false;
    return Atomics.load(status, idx) > 0;
  });

  const batchCount = await page.evaluate(() => {
    const status = (globalThis as any).__ioStatus as Int32Array;
    return Atomics.load(status, (globalThis as any).__ioInputBatchCounterIndex);
  });
  const eventCount = await page.evaluate(() => {
    const status = (globalThis as any).__ioStatus as Int32Array;
    return Atomics.load(status, (globalThis as any).__ioInputEventCounterIndex);
  });

  expect(batchCount).toBeGreaterThan(0);
  expect(eventCount).toBeGreaterThan(0);
});
