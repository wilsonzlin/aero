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

test("IO worker receives Consumer Control (media key) events", async ({ page }) => {
  await page.goto(fixtureUrl);

  await page.waitForFunction(() => {
    const status = (globalThis as any).__ioStatus as Int32Array | undefined;
    const idx = (globalThis as any).__ioReadyIndex as number | undefined;
    if (!status || idx === undefined) return false;
    return Atomics.load(status, idx) === 1;
  });

  await page.click("#emu");

  const before = await page.evaluate(() => {
    const status = (globalThis as any).__ioStatus as Int32Array;
    return Atomics.load(status, (globalThis as any).__ioInputEventCounterIndex);
  });

  await page.evaluate(() => {
    // Playwright's keyboard helper doesn't support media keys reliably; dispatch a synthetic event.
    const down = new KeyboardEvent("keydown", { bubbles: true, cancelable: true });
    Object.defineProperty(down, "code", { value: "AudioVolumeUp" });
    window.dispatchEvent(down);
    const up = new KeyboardEvent("keyup", { bubbles: true, cancelable: true });
    Object.defineProperty(up, "code", { value: "AudioVolumeUp" });
    window.dispatchEvent(up);
    (globalThis as any).__capture.flushNow();
  });

  await page.waitForFunction(
    (prev) => {
      const status = (globalThis as any).__ioStatus as Int32Array | undefined;
      const idx = (globalThis as any).__ioInputEventCounterIndex as number | undefined;
      if (!status || idx === undefined) return false;
      return Atomics.load(status, idx) > (prev as number);
    },
    before,
  );

  const after = await page.evaluate(() => {
    const status = (globalThis as any).__ioStatus as Int32Array;
    return Atomics.load(status, (globalThis as any).__ioInputEventCounterIndex);
  });
  expect(after).toBeGreaterThanOrEqual(before + 2);

  const lastBatch = await page.evaluate(() => (globalThis as any).__lastInputBatchEvents);
  expect(Array.isArray(lastBatch)).toBe(true);
  // Each entry is [type, eventTimestampUs, a, b].
  const hidUsage16 = (lastBatch as Array<[number, number, number, number]>).filter((e) => e[0] === 7);
  // Expect keydown + keyup for AudioVolumeUp (Consumer Page 0x0C, usage 0x00E9).
  expect(
    hidUsage16.map(([, , a, b]) => ({
      usagePage: a & 0xffff,
      pressed: ((a >>> 16) & 1) !== 0,
      usageId: b & 0xffff,
    })),
  ).toEqual(
    expect.arrayContaining([
      { usagePage: 0x0c, pressed: true, usageId: 0x00e9 },
      { usagePage: 0x0c, pressed: false, usageId: 0x00e9 },
    ]),
  );
});
