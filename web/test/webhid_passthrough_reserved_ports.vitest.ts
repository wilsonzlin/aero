import { describe, expect, it, vi } from "vitest";

import type { HidPassthroughMessage } from "../src/platform/hid_passthrough_protocol";
import { WebHidPassthroughManager } from "../src/platform/webhid_passthrough";

class TestTarget {
  readonly messages: HidPassthroughMessage[] = [];

  postMessage(message: HidPassthroughMessage): void {
    this.messages.push(message);
  }
}

function makeDevice(vendorId: number, productId: number, productName: string): HIDDevice {
  return {
    vendorId,
    productId,
    productName,
    collections: [] as unknown as HIDCollectionInfo[],
    open: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  } as unknown as HIDDevice;
}

describe("webhid passthrough reserved external hub ports", () => {
  it("allocates WebHID devices after reserved ports and reuses freed ports", async () => {
    const target = new TestTarget();
    const manager = new WebHidPassthroughManager({
      hid: null,
      target,
      externalHubPortCount: 8,
      reservedExternalHubPorts: 3,
    });

    const devA = makeDevice(1, 1, "A");
    const devB = makeDevice(2, 2, "B");
    const devC = makeDevice(3, 3, "C");

    await manager.attachKnownDevice(devA);
    await manager.attachKnownDevice(devB);

    expect(manager.getState().attachedDevices.map((d) => d.guestPath)).toEqual([
      [0, 4],
      [0, 5],
    ]);

    await manager.detachDevice(devA);
    await manager.attachKnownDevice(devC);

    // Port 0.4 should have been freed and then reused.
    expect(target.messages.some((m) => m.type === "hid:detach" && (m as any).guestPath?.[1] === 4)).toBe(true);
    expect(target.messages.some((m) => m.type === "hid:attach" && (m as any).guestPath?.[1] === 4)).toBe(true);
    expect(manager.getState().attachedDevices.map((d) => d.guestPath)).toEqual([
      [0, 4],
      [0, 5],
    ]);
  });
});

