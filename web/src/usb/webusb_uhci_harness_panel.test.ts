import { describe, expect, it } from "vitest";

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
          case "bulkIn":
            return { kind: "bulkIn", id: action.id, status: "stall" };
          case "bulkOut":
            return { kind: "bulkOut", id: action.id, status: "success", bytesWritten: action.data.byteLength };
          default:
            throw new Error(`unreachable action kind: ${action.kind}`);
        }
      },
    };

    const { completions } = await bridgeHarnessDrainActions(harness, backend);

    expect(callOrder).toEqual(["drain", "execute:1", "push:1", "execute:2", "push:2"]);
    expect(harnessCompletions).toEqual(completions);
  });
});
