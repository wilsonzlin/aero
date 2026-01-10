import { expect, test } from '@playwright/test';

const fixtureUrl = 'http://127.0.0.1:5173/tests/e2e/fixtures/input_capture.html';

type ParsedEvent = {
  type: number;
  timestampUs: number;
  a: number;
  b: number;
};

function parseBatch(buffer: ArrayBuffer): ParsedEvent[] {
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;
  const events: ParsedEvent[] = [];
  const base = 2;
  for (let i = 0; i < count; i++) {
    const off = base + i * 4;
    events.push({
      type: words[off] >>> 0,
      timestampUs: words[off + 1] >>> 0,
      a: words[off + 2] | 0,
      b: words[off + 3] | 0
    });
  }
  return events;
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

  await page.keyboard.down('KeyA');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  let msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  let events = parseBatch(msg.buffer);
  const make = events.find((e) => e.type === 1);
  expect(make).toBeTruthy();
  expect(make!.a & 0xff).toBe(0x1c);
  expect(make!.b).toBe(1);

  await page.keyboard.up('KeyA');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  events = parseBatch(msg.buffer);
  const brk = events.find((e) => e.type === 1);
  expect(brk).toBeTruthy();
  expect(brk!.b).toBe(2);
  expect(brk!.a & 0xff).toBe(0xf0);
  expect((brk!.a >>> 8) & 0xff).toBe(0x1c);

  await page.keyboard.down('ArrowUp');
  await page.evaluate(() => (globalThis as any).__capture.flushNow());
  msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  events = parseBatch(msg.buffer);
  const upMake = events.find((e) => e.type === 1);
  expect(upMake).toBeTruthy();
  expect(upMake!.b).toBe(2);
  expect(upMake!.a & 0xff).toBe(0xe0);
  expect((upMake!.a >>> 8) & 0xff).toBe(0x75);
});

test('captures mouse move/buttons/wheel and batches to worker', async ({ page }) => {
  await page.goto(fixtureUrl);
  await page.click('#emu');

  // Mouse move (pointer locked): inject movementX/Y.
  await page.evaluate(() => {
    const ev = new MouseEvent('mousemove', { bubbles: true, cancelable: true });
    Object.defineProperty(ev, 'movementX', { value: 5 });
    Object.defineProperty(ev, 'movementY', { value: 3 });
    document.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  let msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  let events = parseBatch(msg.buffer);
  const move = events.find((e) => e.type === 2);
  expect(move).toBeTruthy();
  expect(move!.a).toBe(5);
  expect(move!.b).toBe(-3);

  // Buttons.
  await page.evaluate(() => {
    document.dispatchEvent(new MouseEvent('mousedown', { button: 0, bubbles: true, cancelable: true }));
    document.dispatchEvent(new MouseEvent('mouseup', { button: 0, bubbles: true, cancelable: true }));
    (globalThis as any).__capture.flushNow();
  });

  msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  events = parseBatch(msg.buffer);
  const buttons = events.filter((e) => e.type === 3).map((e) => e.a);
  expect(buttons).toEqual([1, 0]);

  // Wheel.
  await page.evaluate(() => {
    const ev = new WheelEvent('wheel', { deltaY: 100, deltaMode: WheelEvent.DOM_DELTA_PIXEL, bubbles: true, cancelable: true });
    document.getElementById('emu')!.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  msg = await page.evaluate(() => (globalThis as any).__inputMessages.shift());
  events = parseBatch(msg.buffer);
  const wheel = events.find((e) => e.type === 4);
  expect(wheel).toBeTruthy();
  expect(wheel!.a).toBe(-1);
});
