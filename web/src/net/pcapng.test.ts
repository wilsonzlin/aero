import { describe, expect, it } from "vitest";

import {
  LinkType,
  PacketDirection,
  PcapngWriter,
  PCAPNG_LINKTYPE_ETHERNET,
  writePcapng,
  type PcapngCapture,
} from "./pcapng";

function pad4(len: number): number {
  return (4 - (len & 3)) & 3;
}

function alignUp4(n: number): number {
  return (n + 3) & ~3;
}

describe("net/pcapng.writePcapng", () => {
  it("writes SHB/IDB/EPB blocks with correct layout and option padding", () => {
    const ifaceName = "en0"; // 3 bytes => requires 1 byte of 32-bit padding.
    const snapLen = 0x1234;
    const tsResolPower10 = 9;
    const packet = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01]); // 5 bytes => requires 3 bytes padding.
    const timestamp = 0x1122_3344_5566_7788n;
    const flags = 0xaabb_ccdd;

    const capture: PcapngCapture = {
      interfaces: [
        {
          linkType: PCAPNG_LINKTYPE_ETHERNET,
          snapLen,
          name: ifaceName,
          tsResolPower10,
        },
      ],
      packets: [
        {
          interfaceId: 0,
          timestamp,
          packet,
          flags,
        },
      ],
    };

    const bytes = writePcapng(capture);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

    // ---- Section Header Block (SHB) ----
    const shbOff = 0;
    const shbType = view.getUint32(shbOff, true);
    expect(shbType).toBe(0x0a0d_0d0a);

    const shbLen = view.getUint32(shbOff + 4, true);
    const shbUserApplBytes = new TextEncoder().encode("aero");
    const shbExpectedLen =
      28 +
      // shb_userappl (code=4)
      4 +
      shbUserApplBytes.byteLength +
      pad4(shbUserApplBytes.byteLength) +
      // end of options
      4;
    expect(shbLen).toBe(shbExpectedLen);
    expect(shbLen % 4).toBe(0);

    const shbBom = view.getUint32(shbOff + 8, true);
    expect(shbBom).toBe(0x1a2b_3c4d);

    const shbMajor = view.getUint16(shbOff + 12, true);
    const shbMinor = view.getUint16(shbOff + 14, true);
    expect(shbMajor).toBe(1);
    expect(shbMinor).toBe(0);

    // 64-bit "unknown section length" sentinel.
    const shbSectionLenLo = view.getUint32(shbOff + 16, true);
    const shbSectionLenHi = view.getUint32(shbOff + 20, true);
    const shbSectionLen = (BigInt(shbSectionLenHi) << 32n) | BigInt(shbSectionLenLo);
    expect(shbSectionLen).toBe(0xffff_ffff_ffff_ffffn);

    // shb_userappl (code=4)
    {
      const optOff = shbOff + 24;
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      expect(code).toBe(4);
      expect(len).toBe(shbUserApplBytes.byteLength);
      const valueStart = optOff + 4;
      const valueEnd = valueStart + len;
      expect(bytes.subarray(valueStart, valueEnd)).toEqual(shbUserApplBytes);
      // padding bytes
      const paddedEnd = valueEnd + pad4(len);
      for (let i = valueEnd; i < paddedEnd; i += 1) {
        expect(bytes[i]).toBe(0);
      }
      // end-of-options
      expect(view.getUint16(paddedEnd, true)).toBe(0);
      expect(view.getUint16(paddedEnd + 2, true)).toBe(0);
    }

    const shbTrailerLen = view.getUint32(shbOff + shbLen - 4, true);
    expect(shbTrailerLen).toBe(shbLen);

    // ---- Interface Description Block (IDB) ----
    const idbOff = shbOff + shbLen;
    const idbType = view.getUint32(idbOff, true);
    expect(idbType).toBe(0x0000_0001);

    const ifaceNameBytes = new TextEncoder().encode(ifaceName);
    const idbOptionsLen =
      // if_name (code=2)
      4 + ifaceNameBytes.byteLength + pad4(ifaceNameBytes.byteLength) +
      // if_tsresol (code=9)
      4 + 1 + pad4(1) +
      // end of options
      4;
    const idbExpectedLen = 20 + idbOptionsLen;

    const idbLen = view.getUint32(idbOff + 4, true);
    expect(idbLen).toBe(idbExpectedLen);
    expect(idbLen % 4).toBe(0);

    const idbLinkType = view.getUint16(idbOff + 8, true);
    expect(idbLinkType).toBe(PCAPNG_LINKTYPE_ETHERNET);
    expect(view.getUint16(idbOff + 10, true)).toBe(0); // reserved
    expect(view.getUint32(idbOff + 12, true)).toBe(snapLen);

    const idbBodyEnd = idbOff + idbLen - 4;
    let optOff = idbOff + 16;
    const textDecoder = new TextDecoder();

    // if_name (code=2)
    {
      expect((optOff - idbOff) % 4).toBe(0);
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      expect(code).toBe(2);
      expect(len).toBe(ifaceNameBytes.byteLength);

      const valueStart = optOff + 4;
      const valueEnd = valueStart + len;
      expect(textDecoder.decode(bytes.subarray(valueStart, valueEnd))).toBe(ifaceName);
      expect(bytes.subarray(valueStart, valueEnd)).toEqual(ifaceNameBytes);

      const paddedEnd = valueEnd + pad4(len);
      expect((paddedEnd - idbOff) % 4).toBe(0);
      for (let i = valueEnd; i < paddedEnd; i += 1) {
        expect(bytes[i]).toBe(0);
      }

      optOff = paddedEnd;
    }

    // if_tsresol (code=9)
    {
      expect((optOff - idbOff) % 4).toBe(0);
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      expect(code).toBe(9);
      expect(len).toBe(1);

      const valueStart = optOff + 4;
      const valueEnd = valueStart + len;
      expect(bytes[valueStart]).toBe(tsResolPower10);

      const paddedEnd = valueEnd + pad4(len);
      expect((paddedEnd - idbOff) % 4).toBe(0);
      for (let i = valueEnd; i < paddedEnd; i += 1) {
        expect(bytes[i]).toBe(0);
      }

      optOff = paddedEnd;
    }

    // end-of-options
    {
      expect((optOff - idbOff) % 4).toBe(0);
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      expect(code).toBe(0);
      expect(len).toBe(0);
      optOff += 4;
    }

    // Options must end exactly at the end of the block body (before trailer).
    expect(optOff).toBe(idbBodyEnd);

    const idbTrailerLen = view.getUint32(idbOff + idbLen - 4, true);
    expect(idbTrailerLen).toBe(idbLen);

    // ---- Enhanced Packet Block (EPB) ----
    const epbOff = idbOff + idbLen;
    const epbType = view.getUint32(epbOff, true);
    expect(epbType).toBe(0x0000_0006);

    const epbLen = view.getUint32(epbOff + 4, true);
    const epbExpectedLen = 32 + (packet.byteLength + pad4(packet.byteLength)) + 12; // epb_flags + end-of-options
    expect(epbLen).toBe(epbExpectedLen);
    expect(epbLen % 4).toBe(0);

    const interfaceId = view.getUint32(epbOff + 8, true);
    expect(interfaceId).toBe(0);

    const tsHi = view.getUint32(epbOff + 12, true);
    const tsLo = view.getUint32(epbOff + 16, true);
    expect(tsHi).toBe(0x1122_3344);
    expect(tsLo).toBe(0x5566_7788);

    const capturedLen = view.getUint32(epbOff + 20, true);
    const originalLen = view.getUint32(epbOff + 24, true);
    expect(capturedLen).toBe(packet.byteLength);
    expect(originalLen).toBe(packet.byteLength);

    const packetStart = epbOff + 28;
    const packetEnd = packetStart + packet.byteLength;
    expect(bytes.subarray(packetStart, packetEnd)).toEqual(packet);

    const paddedPacketEnd = packetEnd + pad4(packet.byteLength);
    expect((paddedPacketEnd - epbOff) % 4).toBe(0);
    for (let i = packetEnd; i < paddedPacketEnd; i += 1) {
      expect(bytes[i]).toBe(0);
    }

    // EPB options.
    const epbBodyEnd = epbOff + epbLen - 4;
    let epbOptOff = paddedPacketEnd;

    // epb_flags (code=2, len=4)
    {
      expect((epbOptOff - epbOff) % 4).toBe(0);
      const code = view.getUint16(epbOptOff, true);
      const len = view.getUint16(epbOptOff + 2, true);
      expect(code).toBe(2);
      expect(len).toBe(4);
      expect(view.getUint32(epbOptOff + 4, true)).toBe(flags >>> 0);
      epbOptOff += 8;
    }

    // end-of-options.
    {
      expect((epbOptOff - epbOff) % 4).toBe(0);
      const code = view.getUint16(epbOptOff, true);
      const len = view.getUint16(epbOptOff + 2, true);
      expect(code).toBe(0);
      expect(len).toBe(0);
      epbOptOff += 4;
    }

    expect(epbOptOff).toBe(epbBodyEnd);

    const epbTrailerLen = view.getUint32(epbOff + epbLen - 4, true);
    expect(epbTrailerLen).toBe(epbLen);
  });
});

describe("net/pcapng.PcapngWriter", () => {
  it("writes SHB, IDB, and EPB blocks with expected fields and options", () => {
    const userAppl = "aero-test";
    const ifaceName = "guest-eth0";
    const textDecoder = new TextDecoder();

    const w = new PcapngWriter(userAppl);
    const iface = w.addInterface(LinkType.Ethernet, ifaceName);

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

    // Ensure shb_userappl (option 4) is present.
    {
      const optsStart = offset + 8 + 16;
      const optsEnd = offset + shbLen - 4;
      let optOff = optsStart;
      let found = false;
      while (optOff + 4 <= optsEnd) {
        const code = view.getUint16(optOff, true);
        const len = view.getUint16(optOff + 2, true);
        optOff += 4;
        if (code === 0) break;
        const valueStart = optOff;
        const valueEnd = valueStart + len;
        expect(valueEnd).toBeLessThanOrEqual(optsEnd);
        if (code === 4) {
          expect(textDecoder.decode(bytes.subarray(valueStart, valueEnd))).toBe(userAppl);
          found = true;
        }
        optOff = alignUp4(valueEnd);
      }
      expect(found).toBe(true);
    }

    offset += shbLen;

    // --- IDB ---
    const idbType = view.getUint32(offset, true);
    expect(idbType).toBe(0x0000_0001);
    const idbLen = view.getUint32(offset + 4, true);
    expect(view.getUint32(offset + idbLen - 4, true)).toBe(idbLen);

    const linkType = view.getUint16(offset + 8, true);
    expect(linkType).toBe(LinkType.Ethernet);

    // Ensure if_name (option 2) and if_tsresol (option 9) exist.
    const idbOptsStart = offset + 16;
    const idbOptsEnd = offset + idbLen - 4;
    let optOff = idbOptsStart;
    let foundTsresol = false;
    let foundName = false;
    while (optOff + 4 <= idbOptsEnd) {
      const code = view.getUint16(optOff, true);
      const len = view.getUint16(optOff + 2, true);
      optOff += 4;
      if (code === 0) break;
      if (code === 2) {
        expect(textDecoder.decode(bytes.subarray(optOff, optOff + len))).toBe(ifaceName);
        foundName = true;
      }
      if (code === 9) {
        expect(len).toBe(1);
        expect(bytes[optOff]).toBe(9);
        foundTsresol = true;
      }
      optOff += len;
      optOff = alignUp4(optOff);
    }
    expect(foundName).toBe(true);
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
