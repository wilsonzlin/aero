import { describe, expect, it } from "vitest";

import { queueKind } from "./layout";
import { IpcLayoutError, createIpcBuffer, openRingByKind, parseIpcBuffer } from "./ipc";

describe("ipc/ipc layout", () => {
  it("creates and parses a buffer with queue descriptors", () => {
    const { buffer, queues } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 64 },
      { kind: queueKind.EVT, capacityBytes: 128 },
    ]);

    const parsed = parseIpcBuffer(buffer);
    expect(parsed.queues).toEqual(queues);
  });

  it("openRingByKind returns a functional ring", () => {
    const { buffer } = createIpcBuffer([
      { kind: queueKind.CMD, capacityBytes: 64 },
      { kind: queueKind.EVT, capacityBytes: 64 },
    ]);

    const cmd = openRingByKind(buffer, queueKind.CMD);
    expect(cmd.tryPush(Uint8Array.of(1, 2, 3))).toBe(true);
    expect(Array.from(cmd.tryPop() ?? [])).toEqual([1, 2, 3]);
  });

  it("rejects buffers with the wrong magic", () => {
    const { buffer } = createIpcBuffer([{ kind: queueKind.CMD, capacityBytes: 64 }]);
    new DataView(buffer).setUint32(0, 0xdead_beef, true);
    expect(() => parseIpcBuffer(buffer)).toThrow(IpcLayoutError);
  });
});

