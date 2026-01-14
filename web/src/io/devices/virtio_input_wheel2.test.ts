import { describe, expect, it } from "vitest";

import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input VirtioInputPciFunction.injectWheel2", () => {
  it("prefers the wasm inject_wheel2 export when present", () => {
    const calls: Array<{ kind: string; args: unknown[] }> = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: (...args) => calls.push({ kind: "inject_wheel", args }),
      inject_hwheel: (...args) => calls.push({ kind: "inject_hwheel", args }),
      inject_wheel2: (...args) => calls.push({ kind: "inject_wheel2", args }),
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "mouse",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    fn.injectWheel2(2, -3);

    expect(calls).toEqual([{ kind: "inject_wheel2", args: [2, -3] }]);
  });

  it("falls back to inject_wheel + inject_hwheel when inject_wheel2 is missing", () => {
    const calls: Array<{ kind: string; args: unknown[] }> = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: (...args) => calls.push({ kind: "inject_wheel", args }),
      inject_hwheel: (...args) => calls.push({ kind: "inject_hwheel", args }),
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "mouse",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    fn.injectWheel2(2, -3);

    expect(calls).toEqual([
      { kind: "inject_wheel", args: [2] },
      { kind: "inject_hwheel", args: [-3] },
    ]);
  });
});

