import { describe, expect, it } from "vitest";
import { NetTracer } from "./net_tracer";
import { PCAPNG_LINKTYPE_ETHERNET } from "./pcapng";

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

function parsePcapngIdbs(bytes: Uint8Array): Array<{ name: string | null; linkType: number }> {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const textDecoder = new TextDecoder();
  const interfaces: Array<{ name: string | null; linkType: number }> = [];

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

    if (blockType === 0x0000_0001) {
      const bodyStart = off + 8;
      const bodyEnd = off + blockLen - 4;

      const linkType = view.getUint16(bodyStart, true);
      // IDB fixed body is 8 bytes: linktype(u16), reserved(u16), snaplen(u32).
      let optOff = bodyStart + 8;
      let name: string | null = null;
      while (optOff + 4 <= bodyEnd) {
        const code = view.getUint16(optOff, true);
        const len = view.getUint16(optOff + 2, true);
        const valueStart = optOff + 4;
        const valueEnd = valueStart + len;
        expect(valueEnd).toBeLessThanOrEqual(bodyEnd);
        if (code === 0) break;
        if (code === 2) name = textDecoder.decode(bytes.subarray(valueStart, valueEnd));
        optOff = valueStart + ((len + 3) & ~3);
      }
      interfaces.push({ name, linkType });
    }

    off += blockLen;
  }

  return interfaces;
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

    const idbs = parsePcapngIdbs(bytes);
    expect(idbs).toEqual([{ name: "guest-eth0", linkType: PCAPNG_LINKTYPE_ETHERNET }]);

    const epbs = parsePcapngEpbs(bytes);
    const tx = epbs.find((e) => arraysEqual(e.payload, txFrame));
    const rx = epbs.find((e) => arraysEqual(e.payload, rxFrame));
    expect(tx).toBeTruthy();
    expect(rx).toBeTruthy();

    // Ethernet frames use a single interface ("guest-eth0"); direction is encoded via EPB flags.
    expect(tx!.interfaceId).toBe(0);
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

  it("exportPcapng does not drain the capture", () => {
    const tracer = new NetTracer();
    tracer.enable();
    tracer.recordEthernet("guest_tx", new Uint8Array([9, 9, 9]), 1n);
    expect(countEpbs(tracer.exportPcapng())).toBe(1);
    expect(tracer.stats().records).toBe(1);
    expect(countEpbs(tracer.exportPcapng())).toBe(1);
    expect(tracer.stats().records).toBe(1);
  });

  it("records empty Ethernet frames (length 0) as EPBs", () => {
    const tracer = new NetTracer();
    tracer.enable();
    tracer.recordEthernet("guest_tx", new Uint8Array([]), 1n);

    expect(tracer.stats().records).toBe(1);
    const epbs = parsePcapngEpbs(tracer.exportPcapng());
    expect(epbs.length).toBe(1);
    expect(epbs[0]!.payload.byteLength).toBe(0);
    expect((epbs[0]!.flags ?? 0) & 0x3).toBe(2); // outbound
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
