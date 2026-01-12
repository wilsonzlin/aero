import { describe, expect, it } from "vitest";

import { NetTracer } from "./net_tracer";

function readU16LE(buf: Uint8Array, offset: number): number {
  return buf[offset]! | (buf[offset + 1]! << 8);
}

function readU32LE(buf: Uint8Array, offset: number): number {
  return (buf[offset]! | (buf[offset + 1]! << 8) | (buf[offset + 2]! << 16) | (buf[offset + 3]! << 24)) >>> 0;
}

function readBigU64LE(buf: Uint8Array, offset: number): bigint {
  const lo = BigInt(readU32LE(buf, offset));
  const hi = BigInt(readU32LE(buf, offset + 4));
  return lo | (hi << 32n);
}

type ParsedEpb = {
  interfaceId: number;
  packetData: Uint8Array;
  flags: number | null;
};

function parsePcapng(bytes: Uint8Array): { linkTypes: number[]; epbs: ParsedEpb[] } {
  const linkTypes: number[] = [];
  const epbs: ParsedEpb[] = [];

  let off = 0;
  while (off + 12 <= bytes.byteLength) {
    const blockType = readU32LE(bytes, off);
    const blockLen = readU32LE(bytes, off + 4);
    if (blockLen < 12 || off + blockLen > bytes.byteLength) break;

    const trailerLen = readU32LE(bytes, off + blockLen - 4);
    expect(trailerLen).toBe(blockLen);

    const bodyStart = off + 8;
    const bodyEnd = off + blockLen - 4;

    if (blockType === 0x0000_0001) {
      // Interface Description Block.
      const linkType = readU16LE(bytes, bodyStart);
      linkTypes.push(linkType);
    } else if (blockType === 0x0000_0006) {
      // Enhanced Packet Block.
      const interfaceId = readU32LE(bytes, bodyStart);
      const capLen = readU32LE(bytes, bodyStart + 12);
      const pktStart = bodyStart + 20;
      const pktEnd = pktStart + capLen;
      expect(pktEnd).toBeLessThanOrEqual(bodyEnd);

      const packetData = bytes.slice(pktStart, pktEnd);

      // Options begin after packet data (padded to 32-bit alignment).
      let optOff = (pktEnd + 3) & ~3;
      let flags: number | null = null;
      while (optOff + 4 <= bodyEnd) {
        const code = readU16LE(bytes, optOff);
        const len = readU16LE(bytes, optOff + 2);
        optOff += 4;
        if (code === 0) break;
        if (code === 2 && len === 4) {
          flags = readU32LE(bytes, optOff);
        }
        optOff += len;
        optOff = (optOff + 3) & ~3;
      }

      epbs.push({ interfaceId, packetData, flags });
    }

    off += blockLen;
  }

  return { linkTypes, epbs };
}

function ascii(bytes: Uint8Array): string {
  return new TextDecoder("ascii").decode(bytes);
}

describe("NetTracer (proxy pseudo-interfaces)", () => {
  it("exports TCP/UDP proxy pseudo packets on user0/user1 with expected headers", () => {
    const tracer = new NetTracer();
    tracer.enable();

    tracer.recordTcpProxy("guest_to_remote", 42, Uint8Array.of(1, 2, 3), 1000n);
    tracer.recordUdpProxy("remote_to_guest", "webrtc", [203, 0, 113, 9], 1234, 5678, Uint8Array.of(9, 8, 7), 2000n);

    const bytes = tracer.exportPcapng();
    expect(bytes.byteLength).toBeGreaterThan(0);

    const parsed = parsePcapng(bytes);

    // Ethernet always exists; proxy pseudo-interfaces only appear when records exist.
    expect(parsed.linkTypes).toContain(1);
    expect(parsed.linkTypes).toContain(147);
    expect(parsed.linkTypes).toContain(148);

    const tcpPkt = parsed.epbs.find((epb) => ascii(epb.packetData.slice(0, 4)) === "ATCP");
    expect(tcpPkt).toBeTruthy();
    expect(tcpPkt!.flags).toBe(2); // outbound

    const atcp = tcpPkt!.packetData;
    expect(ascii(atcp.slice(0, 4))).toBe("ATCP");
    expect(atcp[4]).toBe(0); // dir
    expect(Array.from(atcp.slice(5, 8))).toEqual([0, 0, 0]); // pad
    expect(readBigU64LE(atcp, 8)).toBe(42n);
    expect(Array.from(atcp.slice(16))).toEqual([1, 2, 3]);

    const udpPkt = parsed.epbs.find((epb) => ascii(epb.packetData.slice(0, 4)) === "AUDP");
    expect(udpPkt).toBeTruthy();
    expect(udpPkt!.flags).toBe(1); // inbound

    const audp = udpPkt!.packetData;
    expect(ascii(audp.slice(0, 4))).toBe("AUDP");
    expect(audp[4]).toBe(1); // dir
    expect(audp[5]).toBe(0); // transport=webrtc
    expect(Array.from(audp.slice(6, 8))).toEqual([0, 0]); // pad
    expect(Array.from(audp.slice(8, 12))).toEqual([203, 0, 113, 9]); // remote ip
    expect(readU16LE(audp, 12)).toBe(1234); // src port (LE)
    expect(readU16LE(audp, 14)).toBe(5678); // dst port (LE)
    expect(Array.from(audp.slice(16))).toEqual([9, 8, 7]);
  });
});
