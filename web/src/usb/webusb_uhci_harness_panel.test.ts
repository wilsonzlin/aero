import { describe, expect, it, vi } from "vitest";

import type { UsbHostAction, UsbHostCompletion } from "./webusb_backend";
import { bridgeHarnessDrainActions } from "./webusb_uhci_harness_panel";

describe("bridgeHarnessDrainActions", () => {
  it("drains actions, executes them, then pushes completions in order", async () => {
    const callOrder: string[] = [];

    const actions: UsbHostAction[] = [
      { kind: "bulkIn", id: 1, endpoint: 0x81, length: 8 },
      { kind: "bulkOut", id: 2, endpoint: 0x01, data: new Uint8Array([1, 2, 3]) },
    ];

    const harnessCompletions: UsbHostCompletion[] = [];

    const harness = {
      drain_actions: () => {
        callOrder.push("drain");
        return actions;
      },
      push_completion: (completion: UsbHostCompletion) => {
        callOrder.push(`push:${completion.id}`);
        harnessCompletions.push(completion);
      },
    };

    const backend = {
      execute: async (action: UsbHostAction): Promise<UsbHostCompletion> => {
        callOrder.push(`execute:${action.id}`);
        switch (action.kind) {
          case "controlIn":
            return { kind: "controlIn", id: action.id, status: "stall" };
          case "controlOut":
            return { kind: "controlOut", id: action.id, status: "stall" };
          case "bulkIn":
            return { kind: "bulkIn", id: action.id, status: "stall" };
          case "bulkOut":
            return { kind: "bulkOut", id: action.id, status: "success", bytesWritten: action.data.byteLength };
        }
      },
    };

    const { completions } = await bridgeHarnessDrainActions(harness, backend);

    expect(callOrder).toEqual(["drain", "execute:1", "push:1", "execute:2", "push:2"]);
    expect(harnessCompletions).toEqual(completions);
  });

  it("rejects actions with invalid ids before executing anything", async () => {
    const setup = { bmRequestType: 0x80, bRequest: 0x06, wValue: 0x0100, wIndex: 0, wLength: 18 };
    const harness = {
      drain_actions: () => [{ kind: "controlIn", id: -1, setup }],
      push_completion: (_completion: UsbHostCompletion) => {},
    };
    const backend = {
      execute: vi.fn(async (_action: UsbHostAction): Promise<UsbHostCompletion> => {
        return { kind: "controlIn", id: 0, status: "stall" } satisfies UsbHostCompletion;
      }),
    };

    await expect(bridgeHarnessDrainActions(harness, backend)).rejects.toThrow(/uint32/i);
    expect(backend.execute).not.toHaveBeenCalled();
  });

  it("rejects actions with invalid byte arrays before executing anything", async () => {
    const setup = { bmRequestType: 0x00, bRequest: 0x09, wValue: 1, wIndex: 0, wLength: 2 };
    const harness = {
      drain_actions: () => [{ kind: "controlOut", id: 1, setup, data: [1, 256] }],
      push_completion: (_completion: UsbHostCompletion) => {},
    };
    const backend = {
      execute: vi.fn(async (_action: UsbHostAction): Promise<UsbHostCompletion> => {
        return { kind: "controlOut", id: 0, status: "stall" } satisfies UsbHostCompletion;
      }),
    };

    await expect(bridgeHarnessDrainActions(harness, backend)).rejects.toThrow(/uint8/i);
    expect(backend.execute).not.toHaveBeenCalled();
  });

  it("rejects bulk endpoint numbers that are not proper endpoint addresses", async () => {
    const harness = {
      drain_actions: () => [{ kind: "bulkIn", id: 1, endpoint: 1, length: 8 }],
      push_completion: (_completion: UsbHostCompletion) => {},
    };
    const backend = {
      execute: vi.fn(async (_action: UsbHostAction): Promise<UsbHostCompletion> => {
        return { kind: "bulkIn", id: 0, status: "stall" } satisfies UsbHostCompletion;
      }),
    };

    await expect(bridgeHarnessDrainActions(harness, backend)).rejects.toThrow(/endpoint address/i);
    expect(backend.execute).not.toHaveBeenCalled();
  });
});
