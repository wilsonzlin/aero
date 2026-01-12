import { describe, expect, it } from "vitest";

import { LinkType, PacketDirection, PcapngWriter } from "./pcapng";

function alignUp4(n: number): number {
  return (n + 3) & ~3;
}

describe("net/pcapng", () => {
  it("writes SHB, IDB, and EPB blocks with expected fields and options", () => {
    const w = new PcapngWriter("aero-test");
    const iface = w.addInterface(LinkType.Ethernet, "guest-eth0");

    const payload = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01]);
    w.writePacket(iface, 123_456_789n, payload, PacketDirection.Inbound);

    const bytes = w.intoBytes();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

    let offset = 0;

    // --- SHB ---
    const shbType = view.getUint32(offset, true);
    expect(shbType).toBe(0x0a0d0d0a);
    const shbLen = view.getUint32(offset + 4, true);
    expect(view.getUint32(offset + shbLen - 4, true)).toBe(shbLen);

    const bom = view.getUint32(offset + 8, true);
    expect(bom).toBe(0x1a2b3c4d);

    offset += shbLen;

    // --- IDB ---
    const idbType = view.getUint32(offset, true);
    expect(idbType).toBe(0x0000_0001);
    const idbLen = view.getUint32(offset + 4, true);
    expect(view.getUint32(offset + idbLen - 4, true)).toBe(idbLen);

    const linkType = view.getUint16(offset + 8, true);
    expect(linkType).toBe(LinkType.Ethernet);

    // Ensure if_tsresol (option 9) exists and is 10^-9 (9).
    const idbOptsStart = offset + 16;
    const idbOptsEnd = offset + idbLen - 4;
    let optOff = idbOptsStart;
    let foundTsresol = false;
    while (optOff + 4 <= idbOptsEnd) {
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      optOff += 4;
      if (code === 0) break;
      if (code === 9) {
        expect(len).toBe(1);
        expect(bytes[optOff]).toBe(9);
        foundTsresol = true;
      }
      optOff += len;
      optOff = alignUp4(optOff);
    }
    expect(foundTsresol).toBe(true);

    offset += idbLen;

    // --- EPB ---
    const epbType = view.getUint32(offset, true);
    expect(epbType).toBe(0x0000_0006);
    const epbLen = view.getUint32(offset + 4, true);
    expect(view.getUint32(offset + epbLen - 4, true)).toBe(epbLen);

    const capLen = view.getUint32(offset + 20, true);
    const origLen = view.getUint32(offset + 24, true);
    expect(capLen).toBe(payload.byteLength);
    expect(origLen).toBe(payload.byteLength);

    const packetDataStart = offset + 28;
    expect(bytes.subarray(packetDataStart, packetDataStart + payload.byteLength)).toEqual(payload);

    const epbOptsStart = packetDataStart + alignUp4(payload.byteLength);
    const epbOptsEnd = offset + epbLen - 4;
    optOff = epbOptsStart;
    let foundFlags = false;
    while (optOff + 4 <= epbOptsEnd) {
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      optOff += 4;
      if (code === 0) break;
      if (code === 2) {
        expect(len).toBe(4);
        expect(view.getUint32(optOff, true)).toBe(1); // inbound
        foundFlags = true;
      }
      optOff += len;
      optOff = alignUp4(optOff);
    }
    expect(foundFlags).toBe(true);
  });
});

