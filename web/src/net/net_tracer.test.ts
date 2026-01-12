import { describe, expect, it } from "vitest";
import { NetTracer } from "./net_tracer";

function countEpbs(bytes: Uint8Array): number {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 0;
  let epbs = 0;
  while (off < bytes.byteLength) {
    expect(off + 8).toBeLessThanOrEqual(bytes.byteLength);
    const blockType = view.getUint32(off, true);
    const blockLen = view.getUint32(off + 4, true);
    expect(blockLen).toBeGreaterThanOrEqual(12);
    expect(blockLen % 4).toBe(0);
    expect(off + blockLen).toBeLessThanOrEqual(bytes.byteLength);
    const trailerLen = view.getUint32(off + blockLen - 4, true);
    expect(trailerLen).toBe(blockLen);
    if (blockType === 0x0000_0006) epbs += 1;
    off += blockLen;
  }
  return epbs;
}

describe("NetTracer", () => {
  it("does not record when disabled", () => {
    const tracer = new NetTracer();
    tracer.recordEthernet("guest_tx", new Uint8Array([1, 2, 3]), 123n);
    expect(tracer.stats().records).toBe(0);
    expect(countEpbs(tracer.exportPcapng())).toBe(0);
  });

  it("records frames when enabled and exports PCAPNG with EPBs", () => {
    const tracer = new NetTracer();
    tracer.enable();
    tracer.recordEthernet("guest_tx", new Uint8Array([1, 2, 3, 4]), 1_000n);
    tracer.recordEthernet("guest_rx", new Uint8Array([5, 6, 7]), 2_000n);
    expect(tracer.stats().records).toBe(2);
    expect(countEpbs(tracer.exportPcapng())).toBe(2);
  });

  it("takePcapng drains the capture", () => {
    const tracer = new NetTracer();
    tracer.enable();
    tracer.recordEthernet("guest_tx", new Uint8Array([1, 2, 3]), 1n);
    expect(countEpbs(tracer.takePcapng())).toBe(1);
    expect(tracer.stats().records).toBe(0);
    expect(countEpbs(tracer.takePcapng())).toBe(0);
  });

  it("enforces maxBytes by dropping new frames", () => {
    const tracer = new NetTracer({ maxBytes: 6 });
    tracer.enable();
    tracer.recordEthernet("guest_tx", new Uint8Array([1, 2, 3, 4]), 1n);
    tracer.recordEthernet("guest_tx", new Uint8Array([5, 6, 7]), 2n);

    const stats = tracer.stats();
    expect(stats.records).toBe(1);
    expect(stats.bytes).toBe(4);
    expect(stats.droppedRecords).toBe(1);
    expect(stats.droppedBytes).toBe(3);
  });
});

