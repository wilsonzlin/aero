import { describe, expect, it } from "vitest";

import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input camelCase compatibility", () => {
  it("accepts camelCase virtio-input device exports (backwards compatibility)", () => {
    const mmioReads: Array<[number, number]> = [];
    const mmioWrites: Array<[number, number, number]> = [];
    const injectedKeys: Array<[number, boolean]> = [];
    let pollCount = 0;
    let freeCount = 0;
    const pciCmds: number[] = [];

    const dev = {
      mmioRead: (offset: number, size: number) => {
        mmioReads.push([offset >>> 0, size >>> 0]);
        return 0x1234_5678;
      },
      mmioWrite: (offset: number, size: number, value: number) => {
        mmioWrites.push([offset >>> 0, size >>> 0, value >>> 0]);
      },
      poll: () => {
        pollCount += 1;
      },
      driverOk: () => false,
      irqAsserted: () => false,
      injectKey: (linuxKey: number, pressed: boolean) => {
        injectedKeys.push([linuxKey >>> 0, Boolean(pressed)]);
      },
      injectRel: () => {},
      injectButton: () => {},
      injectWheel: () => {},
      setPciCommand: (cmd: number) => pciCmds.push(cmd >>> 0),
      free: () => {
        freeCount += 1;
      },
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev as unknown as VirtioInputPciDeviceLike,
      irqSink: { raiseIrq: () => {}, lowerIrq: () => {} },
    });

    // MMIO plumbing should hit the resolved camelCase helpers.
    expect(fn.mmioRead(0, 0n, 4)).toBe(0x1234_5678);
    fn.mmioWrite(0, 0n, 2, 0xfeed_beef);
    expect(mmioReads).toEqual([[0, 4]]);
    // Masked to 16-bit for size=2.
    expect(mmioWrites).toEqual([[0, 2, 0xbeef]]);

    // Input injection should use the resolved camelCase method.
    fn.injectKey(30, true);
    expect(injectedKeys).toEqual([[30, true]]);

    // Enable bus mastering so poll runs; ensure PCI command is mirrored via setPciCommand.
    fn.onPciCommandWrite(1 << 2);
    expect(pciCmds).toEqual([0x0004]);
    fn.tick(0);
    expect(pollCount).toBe(1);

    fn.destroy();
    expect(freeCount).toBe(1);
  });
});

