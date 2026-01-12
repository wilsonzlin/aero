import { expect, test } from "@playwright/test";

const fixtureUrl = "/tests/e2e/fixtures/usb_hid_bridge.html";

test("InputCapture → HID usage → UsbHidBridge produces keyboard + mouse reports", async ({ page }) => {
  await page.goto(fixtureUrl);

  await page.waitForFunction(() => (globalThis as any).__usbHidReady === true);

  await page.click("#emu");

  // KeyA press should yield an 8-byte boot keyboard report with usage 0x04 in slot 0.
  await page.keyboard.down("KeyA");
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  const make = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const report = bridge.drain_next_keyboard_report();
    return report ? Array.from(report) : null;
  });
  expect(make).toEqual([0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);

  await page.keyboard.up("KeyA");
  await page.evaluate(() => (globalThis as any).__capture.flushNow());

  const brk = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const report = bridge.drain_next_keyboard_report();
    return report ? Array.from(report) : null;
  });
  expect(brk).toEqual([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

  // Mouse move: movementY=3 should become dy=+3 in HID (positive down).
  await page.evaluate(() => {
    const ev = new MouseEvent("mousemove", { bubbles: true, cancelable: true });
    Object.defineProperty(ev, "movementX", { value: 5 });
    Object.defineProperty(ev, "movementY", { value: 3 });
    document.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  const move = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const report = bridge.drain_next_mouse_report();
    return report ? Array.from(report) : null;
  });
  expect(move).toEqual([0x00, 0x05, 0x03, 0x00]);

  // Buttons: left down then up.
  await page.evaluate(() => {
    document.dispatchEvent(new MouseEvent("mousedown", { button: 0, bubbles: true, cancelable: true }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0, bubbles: true, cancelable: true }));
    (globalThis as any).__capture.flushNow();
  });

  const buttons = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const out = [];
    while (true) {
      const r = bridge.drain_next_mouse_report();
      if (!r) break;
      out.push(Array.from(r));
    }
    return out;
  });
  expect(buttons).toEqual([
    [0x01, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
  ]);

  // Wheel: DOM deltaY>0 (scroll down) maps to HID wheel=-1 (0xFF).
  await page.evaluate(() => {
    const ev = new WheelEvent("wheel", {
      deltaY: 100,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
      bubbles: true,
      cancelable: true,
    });
    document.getElementById("emu")!.dispatchEvent(ev);
    (globalThis as any).__capture.flushNow();
  });

  const wheel = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const report = bridge.drain_next_mouse_report();
    return report ? Array.from(report) : null;
  });
  expect(wheel).toEqual([0x00, 0x00, 0x00, 0xff]);

  // Gamepad: send a packed report directly through the batch wire format.
  await page.evaluate(() => {
    const buffer = new ArrayBuffer((2 + 4) * 4);
    const words = new Int32Array(buffer);
    words[0] = 1; // count
    words[1] = 0; // batchSendTimestampUs (unused in fixture)

    const off = 2;
    words[off] = 5; // InputEventType.GamepadReport
    words[off + 1] = 0; // eventTimestampUs
    words[off + 2] = 0x00080001; // buttons=1, hat=8 (neutral), x=0
    words[off + 3] = 0; // y=0, rx=0, ry=0, padding=0

    (globalThis as any).__inputTarget.postMessage({ type: "in:input-batch", buffer }, []);
  });

  const pad = await page.evaluate(() => {
    const bridge = (globalThis as any).__usbHidBridge;
    const report = bridge.drain_next_gamepad_report();
    return report ? Array.from(report) : null;
  });
  expect(pad).toEqual([0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]);
});
