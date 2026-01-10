import { describe, expect, it } from "vitest";

import { decodeCommand, decodeEvent, encodeCommand, encodeEvent } from "./protocol.ts";

describe("ipc/protocol", () => {
  it("roundtrips commands", () => {
    const cases = [
      { kind: "nop" as const, seq: 123 },
      { kind: "shutdown" as const },
      { kind: "mmioRead" as const, id: 1, addr: 0xfee0_0000n, size: 4 },
      { kind: "mmioWrite" as const, id: 2, addr: 0xfed0_0000n, data: Uint8Array.of(1, 2, 3, 4, 5) },
      { kind: "portRead" as const, id: 3, port: 0x0060, size: 1 },
      { kind: "portWrite" as const, id: 4, port: 0x0064, size: 1, value: 0xaa },
    ];

    for (const cmd of cases) {
      const bytes = encodeCommand(cmd);
      const decoded = decodeCommand(bytes);
      expect(decoded).toEqual(cmd);
    }
  });

  it("encodes NOP with a stable byte layout", () => {
    expect(Array.from(encodeCommand({ kind: "nop", seq: 1 }))).toEqual([0x00, 0x00, 1, 0, 0, 0]);
  });

  it("roundtrips events", () => {
    const cases = [
      { kind: "ack" as const, seq: 42 },
      { kind: "mmioReadResp" as const, id: 9, data: Uint8Array.of(0xaa, 0xbb) },
      { kind: "portReadResp" as const, id: 10, value: 0x1234_5678 },
      { kind: "mmioWriteResp" as const, id: 11 },
      { kind: "portWriteResp" as const, id: 12 },
      { kind: "frameReady" as const, frameId: 999n },
      { kind: "irqRaise" as const, irq: 5 },
      { kind: "irqLower" as const, irq: 5 },
      { kind: "log" as const, level: "info" as const, message: "hello" },
      { kind: "panic" as const, message: "oh no" },
      { kind: "tripleFault" as const },
    ];

    for (const evt of cases) {
      const bytes = encodeEvent(evt);
      const decoded = decodeEvent(bytes);
      expect(decoded).toEqual(evt);
    }
  });
});
