import { expect, test, type Page } from '@playwright/test';

const fixtureUrl = '/tests/e2e/fixtures/input_capture.html';

type ParsedEvent = {
  type: number;
  timestampUs: number;
  a: number;
  b: number;
};

async function popBatchEvents(page: Page): Promise<ParsedEvent[]> {
  // Parse the input batch in the page context to avoid ArrayBuffer/TypedArray
  // serialization edge cases across Playwright versions.
  return page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const messages = (globalThis as any).__inputMessages as any[] | undefined;
    const msg = messages?.pop();
    if (!msg || !(msg.buffer instanceof ArrayBuffer)) {
      return [];
    }

    const words = new Int32Array(msg.buffer);
    const count = words[0] >>> 0;
    const base = 2;

    const events: Array<{ type: number; timestampUs: number; a: number; b: number }> = [];
    for (let i = 0; i < count; i++) {
      const off = base + i * 4;
      events.push({
        type: words[off] >>> 0,
        timestampUs: words[off + 1] >>> 0,
        a: words[off + 2] | 0,
        b: words[off + 3] | 0,
      });
    }
    return events;
  });
}

test('requests pointer lock on canvas click', async ({ page }) => {
  await page.goto(fixtureUrl);
  await page.click('#emu');

  await expect(page.locator('#emu')).toBeVisible();

  const requestCount = await page.evaluate(() => (globalThis as any).__plRequestCount ?? 0);
  expect(requestCount).toBe(1);

  const locked = await page.evaluate(() => (globalThis as any).__capture.pointerLocked);
  expect(locked).toBe(true);
});

test('translates keyboard events into PS/2 set-2 scancode sequences', async ({ page }) => {
  await page.goto(fixtureUrl);
  await page.click('#emu');
  await page.evaluate(() => ((globalThis as any).__inputMessages.length = 0));

  await page.keyboard.down('KeyA');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  let events = await popBatchEvents(page);
  const make = events.find((e) => e.type === 1);
  expect(make).toBeTruthy();
  expect(make!.a & 0xff).toBe(0x1c);
  expect(make!.b).toBe(1);

  await page.evaluate(() => ((globalThis as any).__inputMessages.length = 0));
  await page.keyboard.up('KeyA');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  events = await popBatchEvents(page);
  const brk = events.find((e) => e.type === 1);
  expect(brk).toBeTruthy();
  expect(brk!.b).toBe(2);
  expect(brk!.a & 0xff).toBe(0xf0);
  expect((brk!.a >>> 8) & 0xff).toBe(0x1c);

  await page.evaluate(() => ((globalThis as any).__inputMessages.length = 0));
  await page.keyboard.down('ArrowUp');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());
  events = await popBatchEvents(page);
  const upMake = events.find((e) => e.type === 1);
  expect(upMake).toBeTruthy();
  expect(upMake!.b).toBe(2);
  expect(upMake!.a & 0xff).toBe(0xe0);
  expect((upMake!.a >>> 8) & 0xff).toBe(0x75);

  await page.evaluate(() => ((globalThis as any).__inputMessages.length = 0));
  await page.keyboard.up("ArrowUp");
  await page.evaluate(() => (globalThis as any).__capture.flushNow());
  events = await popBatchEvents(page);
  const upBreak = events.find((e) => e.type === 1);
  expect(upBreak).toBeTruthy();
  expect(upBreak!.b).toBe(3);
  expect(upBreak!.a & 0xff).toBe(0xe0);
  expect((upBreak!.a >>> 8) & 0xff).toBe(0xf0);
  expect((upBreak!.a >>> 16) & 0xff).toBe(0x75);

  // PrintScreen uses a multi-byte Set 2 sequence. The make code is 4 bytes and the break code is 6 bytes.
  await page.evaluate(() => {
    (globalThis as any).__inputMessages.length = 0;
    window.dispatchEvent(new KeyboardEvent("keydown", { code: "PrintScreen", bubbles: true }));
    (globalThis as any).__capture.flushNow();
  });
  events = await popBatchEvents(page);
  const printMake = events.filter((e) => e.type === 1);
  expect(printMake).toHaveLength(1);
  expect(printMake[0]!.b).toBe(4);
  expect(printMake[0]!.a & 0xff).toBe(0xe0);
  expect((printMake[0]!.a >>> 8) & 0xff).toBe(0x12);
  expect((printMake[0]!.a >>> 16) & 0xff).toBe(0xe0);
  expect((printMake[0]!.a >>> 24) & 0xff).toBe(0x7c);

  await page.evaluate(() => {
    (globalThis as any).__inputMessages.length = 0;
    window.dispatchEvent(new KeyboardEvent("keyup", { code: "PrintScreen", bubbles: true }));
    (globalThis as any).__capture.flushNow();
  });
  events = await popBatchEvents(page);
  const printBreak = events.filter((e) => e.type === 1);
  expect(printBreak).toHaveLength(2);
  expect(printBreak[0]!.b).toBe(4);
  expect(printBreak[0]!.a & 0xff).toBe(0xe0);
  expect((printBreak[0]!.a >>> 8) & 0xff).toBe(0xf0);
  expect((printBreak[0]!.a >>> 16) & 0xff).toBe(0x7c);
  expect((printBreak[0]!.a >>> 24) & 0xff).toBe(0xe0);

  expect(printBreak[1]!.b).toBe(2);
  expect(printBreak[1]!.a & 0xff).toBe(0xf0);
  expect((printBreak[1]!.a >>> 8) & 0xff).toBe(0x12);
});

test('captures mouse move/buttons/wheel and batches to worker', async ({ page }) => {
  await page.goto(fixtureUrl);
  await page.click('#emu');
  await page.evaluate(() => ((globalThis as any).__inputMessages.length = 0));

  // Mouse move (pointer locked): inject movementX/Y.
  await page.evaluate(() => {
    const ev = new MouseEvent('mousemove', { bubbles: true, cancelable: true });
    Object.defineProperty(ev, 'movementX', { value: 5 });
    Object.defineProperty(ev, 'movementY', { value: 3 });
    document.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  let events = await popBatchEvents(page);
  const move = events.find((e) => e.type === 2);
  expect(move).toBeTruthy();
  expect(move!.a).toBe(5);
  expect(move!.b).toBe(-3);

  // Buttons.
  await page.evaluate(() => {
    (globalThis as any).__inputMessages.length = 0;
    document.dispatchEvent(new MouseEvent('mousedown', { button: 0, bubbles: true, cancelable: true }));
    document.dispatchEvent(new MouseEvent('mouseup', { button: 0, bubbles: true, cancelable: true }));
    (globalThis as any).__capture.flushNow();
  });

  events = await popBatchEvents(page);
  const buttons = events.filter((e) => e.type === 3).map((e) => e.a);
  expect(buttons).toEqual([1, 0]);

  // Wheel.
  await page.evaluate(() => {
    (globalThis as any).__inputMessages.length = 0;
    const ev = new WheelEvent('wheel', {
      deltaY: 100,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
      bubbles: true,
      cancelable: true,
    });
    document.getElementById('emu')!.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  events = await popBatchEvents(page);
  const wheel = events.find((e) => e.type === 4);
  expect(wheel).toBeTruthy();
  expect(wheel!.a).toBe(-1);
});
