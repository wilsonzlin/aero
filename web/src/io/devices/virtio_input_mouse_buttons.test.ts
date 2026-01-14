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

  it("masks the input to the low 8 button bits (Linux BTN_* 0x110..0x117)", () => {
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

    // 0x1ff should behave like 0xff.
    fn.injectMouseButtons(0x1ff);

    expect(injected).toEqual([
      { code: 0x110, pressed: true }, // BTN_LEFT
      { code: 0x111, pressed: true }, // BTN_RIGHT
      { code: 0x112, pressed: true }, // BTN_MIDDLE
      { code: 0x113, pressed: true }, // BTN_SIDE
      { code: 0x114, pressed: true }, // BTN_EXTRA
      { code: 0x115, pressed: true }, // BTN_FORWARD
      { code: 0x116, pressed: true }, // BTN_BACK
      { code: 0x117, pressed: true }, // BTN_TASK
    ]);
  });

  it("treats snapshot-restore mouseButtons cache as unknown and forces a full resync (does not drop first press)", () => {
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
      load_state: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "mouse",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    // Snapshot restore should invalidate the host-side previous-buttons cache to an "unknown" marker.
    expect(fn.loadState(new Uint8Array([1, 2, 3]))).toBe(true);

    fn.injectMouseButtons(0x01);

    expect(injected).toEqual([
      { code: 0x110, pressed: true }, // BTN_LEFT
      { code: 0x111, pressed: false }, // BTN_RIGHT
      { code: 0x112, pressed: false }, // BTN_MIDDLE
      { code: 0x113, pressed: false }, // BTN_SIDE
      { code: 0x114, pressed: false }, // BTN_EXTRA
      { code: 0x115, pressed: false }, // BTN_FORWARD
      { code: 0x116, pressed: false }, // BTN_BACK
      { code: 0x117, pressed: false }, // BTN_TASK
    ]);
  });
});
