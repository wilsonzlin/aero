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

type ParsedInterface = {
  linkType: number;
  name: string | null;
};

function parsePcapng(bytes: Uint8Array): { interfaces: ParsedInterface[]; epbs: ParsedEpb[] } {
  const interfaces: ParsedInterface[] = [];
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

      let name: string | null = null;
      let optOff = bodyStart + 8;
      while (optOff + 4 <= bodyEnd) {
        const code = readU16LE(bytes, optOff);
        const len = readU16LE(bytes, optOff + 2);
        optOff += 4;
        if (code === 0) break;
        if (code === 2) {
          name = new TextDecoder().decode(bytes.subarray(optOff, optOff + len));
        }
        optOff += len;
        optOff = (optOff + 3) & ~3;
      }

      interfaces.push({ linkType, name });
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

  return { interfaces, epbs };
}

function ascii(bytes: Uint8Array): string {
  return new TextDecoder("ascii").decode(bytes);
}

describe("NetTracer (proxy pseudo-interfaces)", () => {
  it("exports TCP/UDP proxy pseudo packets on user0/user1 with expected headers", () => {
    const tracer = new NetTracer({ captureTcpProxy: true, captureUdpProxy: true });
    tracer.enable();

    tracer.recordTcpProxy("guest_to_remote", 42, Uint8Array.of(1, 2, 3), 1000n);
    tracer.recordUdpProxy("remote_to_guest", "webrtc", [203, 0, 113, 9], 1234, 5678, Uint8Array.of(9, 8, 7), 2000n);

    const bytes = tracer.exportPcapng();
    expect(bytes.byteLength).toBeGreaterThan(0);

    const parsed = parsePcapng(bytes);

    // Ethernet always exists; proxy pseudo-interfaces only appear when records exist.
    expect(parsed.interfaces.map((i) => i.linkType)).toContain(1);
    expect(parsed.interfaces.map((i) => i.linkType)).toContain(147);
    expect(parsed.interfaces.map((i) => i.linkType)).toContain(148);

    expect(parsed.interfaces.map((i) => i.name)).toContain("guest-eth0");
    expect(parsed.interfaces.map((i) => i.name)).toContain("tcp-proxy");
    expect(parsed.interfaces.map((i) => i.name)).toContain("udp-proxy");

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

  it("only creates tcp-proxy interface when TCP proxy records exist", () => {
    const tracer = new NetTracer({ captureTcpProxy: true });
    tracer.enable();
    tracer.recordTcpProxy("guest_to_remote", 1, Uint8Array.of(1, 2, 3), 1n);

    const { interfaces } = parsePcapng(tracer.exportPcapng());
    const linkTypes = interfaces.map((i) => i.linkType);
    const names = interfaces.map((i) => i.name);

    expect(linkTypes).toContain(1);
    expect(linkTypes).toContain(147);
    expect(linkTypes).not.toContain(148);

    expect(names).toContain("guest-eth0");
    expect(names).toContain("tcp-proxy");
    expect(names).not.toContain("udp-proxy");
  });

  it("only creates udp-proxy interface when UDP proxy records exist", () => {
    const tracer = new NetTracer({ captureUdpProxy: true });
    tracer.enable();
    tracer.recordUdpProxy("guest_to_remote", "proxy", [192, 0, 2, 1], 1000, 2000, Uint8Array.of(4, 5, 6), 1n);

    const { interfaces } = parsePcapng(tracer.exportPcapng());
    const linkTypes = interfaces.map((i) => i.linkType);
    const names = interfaces.map((i) => i.name);

    expect(linkTypes).toContain(1);
    expect(linkTypes).toContain(148);
    expect(linkTypes).not.toContain(147);

    expect(names).toContain("guest-eth0");
    expect(names).toContain("udp-proxy");
    expect(names).not.toContain("tcp-proxy");
  });

  it("records empty proxy payloads as header-only pseudo packets", () => {
    const tracer = new NetTracer({ captureTcpProxy: true, captureUdpProxy: true });
    tracer.enable();

    tracer.recordTcpProxy("guest_to_remote", 123, new Uint8Array([]), 1n);
    tracer.recordUdpProxy("guest_to_remote", "proxy", [1, 2, 3, 4], 1, 2, new Uint8Array([]), 2n);

    const { epbs } = parsePcapng(tracer.exportPcapng());
    const tcpPkt = epbs.find((epb) => ascii(epb.packetData.slice(0, 4)) === "ATCP");
    const udpPkt = epbs.find((epb) => ascii(epb.packetData.slice(0, 4)) === "AUDP");
    expect(tcpPkt).toBeTruthy();
    expect(udpPkt).toBeTruthy();
    expect(tcpPkt!.packetData.byteLength).toBe(16);
    expect(udpPkt!.packetData.byteLength).toBe(16);
  });

  it("does not capture proxy packets unless explicitly enabled in NetTraceConfig", () => {
    const tracer = new NetTracer();
    tracer.enable();

    tracer.recordTcpProxy("guest_to_remote", 123, Uint8Array.of(1, 2, 3), 1n);
    tracer.recordUdpProxy("guest_to_remote", "proxy", [1, 2, 3, 4], 1, 2, Uint8Array.of(4, 5, 6), 2n);

    const { interfaces, epbs } = parsePcapng(tracer.exportPcapng());
    const linkTypes = interfaces.map((i) => i.linkType);
    expect(linkTypes).toContain(1);
    expect(linkTypes).not.toContain(147);
    expect(linkTypes).not.toContain(148);

    expect(epbs.some((epb) => ascii(epb.packetData.slice(0, 4)) === "ATCP")).toBe(false);
    expect(epbs.some((epb) => ascii(epb.packetData.slice(0, 4)) === "AUDP")).toBe(false);
  });
});
