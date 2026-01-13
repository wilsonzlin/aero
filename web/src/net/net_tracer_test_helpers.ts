export function readU16LE(buf: Uint8Array, offset: number): number {
  return buf[offset]! | (buf[offset + 1]! << 8);
}

export function readU32LE(buf: Uint8Array, offset: number): number {
  return (buf[offset]! | (buf[offset + 1]! << 8) | (buf[offset + 2]! << 16) | (buf[offset + 3]! << 24)) >>> 0;
}

export function readBigU64LE(buf: Uint8Array, offset: number): bigint {
  const lo = BigInt(readU32LE(buf, offset));
  const hi = BigInt(readU32LE(buf, offset + 4));
  return lo | (hi << 32n);
}

export type ParsedEpb = {
  interfaceId: number;
  packetData: Uint8Array;
  flags: number | null;
};

export type ParsedInterface = {
  linkType: number;
  name: string | null;
};

export function parsePcapng(bytes: Uint8Array): { interfaces: ParsedInterface[]; epbs: ParsedEpb[] } {
  const interfaces: ParsedInterface[] = [];
  const epbs: ParsedEpb[] = [];

  let off = 0;
  while (off + 12 <= bytes.byteLength) {
    const blockType = readU32LE(bytes, off);
    const blockLen = readU32LE(bytes, off + 4);
    if (blockLen < 12 || off + blockLen > bytes.byteLength) break;

    // Trailer block length must match. If it doesn't, stop parsing to avoid
    // OOB reads on malformed output.
    const trailerLen = readU32LE(bytes, off + blockLen - 4);
    if (trailerLen !== blockLen) break;

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
      if (pktEnd > bodyEnd) break;

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

export function ascii(bytes: Uint8Array): string {
  return new TextDecoder("ascii").decode(bytes);
}

