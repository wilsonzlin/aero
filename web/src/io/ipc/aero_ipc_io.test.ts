import { describe, expect, it } from "vitest";

import { createIpcBuffer, openRingByKind } from "../../ipc/ipc.ts";
import { queueKind } from "../../ipc/layout.ts";
import { encodeEvent } from "../../ipc/protocol.ts";

import { AeroIpcIoClient } from "./aero_ipc_io.ts";

describe("io/ipc/aero_ipc_io", () => {
  it("poll() drains async events (irqRaise)", () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
    const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

    const irqs: Array<{ irq: number; level: boolean }> = [];
    const io = new AeroIpcIoClient(cmdQ, evtQ, {
      onIrq: (irq, level) => irqs.push({ irq, level }),
    });

    expect(evtQ.tryPush(encodeEvent({ kind: "irqRaise", irq: 1 }))).toBe(true);
    expect(io.poll()).toBe(1);
    expect(irqs).toEqual([{ irq: 1, level: true }]);
  });

  it("poll() buffers responses so later synchronous calls can retrieve them by id", () => {
    const { buffer: ipcBuffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 1 << 16 },
      { kind: queueKind.EVT, capacityBytes: 1 << 16 },
    ]);

    const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
    const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);
    const io = new AeroIpcIoClient(cmdQ, evtQ);

    // The first request id allocated by `AeroIpcIoClient` is 1.
    expect(evtQ.tryPush(encodeEvent({ kind: "diskReadResp", id: 1, ok: true, bytes: 512 }))).toBe(true);
    expect(io.poll()).toBe(1);

    // No I/O server is needed here: the response was already buffered by poll().
    const resp = io.diskRead(0n, 512, 0n, 10);
    expect(resp).toEqual({ kind: "diskReadResp", id: 1, ok: true, bytes: 512 });
  });
});

