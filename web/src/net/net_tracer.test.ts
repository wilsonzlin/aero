import { describe, expect, it } from "vitest";
import { NetTracer } from "./net_tracer";

function arraysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

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

function parsePcapngEpbs(bytes: Uint8Array): Array<{ interfaceId: number; payload: Uint8Array; flags: number | null }> {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out: Array<{ interfaceId: number; payload: Uint8Array; flags: number | null }> = [];

  function alignUp4(n: number): number {
    return (n + 3) & ~3;
  }

  let off = 0;
  while (off < bytes.byteLength) {
    expect(off + 8).toBeLessThanOrEqual(bytes.byteLength);
    const blockType = view.getUint32(off, true);
    const blockLen = view.getUint32(off + 4, true);
    expect(blockLen).toBeGreaterThanOrEqual(12);
    expect(blockLen % 4).toBe(0);
    expect(off + blockLen).toBeLessThanOrEqual(bytes.byteLength);
    const trailerLen = view.getUint32(off + blockLen - 4, true);
    expect(trailerLen).toBe(blockLen);

    if (blockType === 0x0000_0006) {
      const interfaceId = view.getUint32(off + 8, true);
      const capLen = view.getUint32(off + 20, true);
      const packetDataStart = off + 28;
      const packetDataEnd = packetDataStart + capLen;
      expect(packetDataEnd).toBeLessThanOrEqual(off + blockLen - 4);
      const payload = bytes.subarray(packetDataStart, packetDataEnd).slice();

      const optsStart = packetDataStart + alignUp4(capLen);
      const optsEnd = off + blockLen - 4;
      let optOff = optsStart;
      let flags: number | null = null;
      while (optOff + 4 <= optsEnd) {
        const code = view.getUint16(optOff, true);
        const len = view.getUint16(optOff + 2, true);
        optOff += 4;
        if (code === 0) break;
        if (code === 2 && len === 4) {
          flags = view.getUint32(optOff, true);
        }
        optOff += len;
        optOff = alignUp4(optOff);
      }
      out.push({ interfaceId, payload, flags });
    }

    off += blockLen;
  }

  return out;
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
    const txFrame = new Uint8Array([1, 2, 3, 4]);
    const rxFrame = new Uint8Array([5, 6, 7]);
    tracer.recordEthernet("guest_tx", txFrame, 1_000n);
    tracer.recordEthernet("guest_rx", rxFrame, 2_000n);
    expect(tracer.stats().records).toBe(2);
    const bytes = tracer.exportPcapng();
    expect(countEpbs(bytes)).toBe(2);

    const epbs = parsePcapngEpbs(bytes);
    const tx = epbs.find((e) => arraysEqual(e.payload, txFrame));
    const rx = epbs.find((e) => arraysEqual(e.payload, rxFrame));
    expect(tx).toBeTruthy();
    expect(rx).toBeTruthy();

    // Interface 0 is guest_rx, 1 is guest_tx.
    expect(tx!.interfaceId).toBe(1);
    expect(rx!.interfaceId).toBe(0);

    // EPB direction bits: 1=inbound, 2=outbound.
    expect((tx!.flags ?? 0) & 0x3).toBe(2);
    expect((rx!.flags ?? 0) & 0x3).toBe(1);
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
