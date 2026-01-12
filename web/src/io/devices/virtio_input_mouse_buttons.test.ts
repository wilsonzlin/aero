import { describe, expect, it } from "vitest";

import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input VirtioInputPciFunction.injectMouseButtons", () => {
  it("emits BTN_* transitions only for changed button bits", () => {
    const injected: Array<{ code: number; pressed: boolean }> = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: (btn, pressed) => injected.push({ code: btn, pressed }),
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "mouse",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    fn.injectMouseButtons(0x01); // left down
    fn.injectMouseButtons(0x01); // no-op (still down)
    fn.injectMouseButtons(0x03); // right down
    fn.injectMouseButtons(0x00); // left+right up

    expect(injected).toEqual([
      { code: 0x110, pressed: true }, // BTN_LEFT
      { code: 0x111, pressed: true }, // BTN_RIGHT
      { code: 0x110, pressed: false }, // BTN_LEFT
      { code: 0x111, pressed: false }, // BTN_RIGHT
    ]);
  });

  it("masks the input to the low 3 button bits (left/right/middle)", () => {
    const injected: Array<{ code: number; pressed: boolean }> = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: (btn, pressed) => injected.push({ code: btn, pressed }),
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "mouse",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    // 0xff should behave like 0x07.
    fn.injectMouseButtons(0xff);

    expect(injected).toEqual([
      { code: 0x110, pressed: true }, // BTN_LEFT
      { code: 0x111, pressed: true }, // BTN_RIGHT
      { code: 0x112, pressed: true }, // BTN_MIDDLE
    ]);
  });
});

