import { describe, expect, it } from "vitest";

import { defaultReadValue } from "../ipc/io_protocol.ts";
import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input VirtioInputPciFunction MMIO bounds", () => {
  it("returns defaultReadValue and does not call into the device for invalid MMIO reads", () => {
    let readCalls = 0;

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => {
        readCalls += 1;
        return 0;
      },
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    // Wrong BAR index.
    expect(fn.mmioRead(1, 0n, 4)).toBe(defaultReadValue(4));
    // Unsupported size.
    expect(fn.mmioRead(0, 0n, 8)).toBe(defaultReadValue(8));
    // Out-of-range offset.
    expect(fn.mmioRead(0, 0x4000n, 4)).toBe(defaultReadValue(4));

    expect(readCalls).toBe(0);
  });

  it("ignores invalid MMIO writes and only forwards valid ones", () => {
    const writes: Array<{ off: number; size: number; value: number }> = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: (off, size, value) => writes.push({ off, size, value }),
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    // Wrong BAR index.
    fn.mmioWrite(1, 0n, 4, 0x1234);
    // Unsupported size.
    fn.mmioWrite(0, 0n, 8, 0x1234);
    // Out-of-range offset.
    fn.mmioWrite(0, 0x4000n, 4, 0x1234);

    // Valid write (within range, supported size).
    fn.mmioWrite(0, 0x10n, 4, 0x11223344);

    expect(writes).toEqual([{ off: 0x10, size: 4, value: 0x11223344 }]);
  });
});
