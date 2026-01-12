// Minimal PCAPNG writer for the web runtime.
//
// This module contains two encoders:
//
// - `PcapngWriter`: incremental PCAPNG writer (port of
//   `crates/emulator/src/io/net/trace/pcapng.rs`).
// - `writePcapng`: convenience helper used by `NetTracer`.
//
// Only a small subset of the PCAPNG spec is implemented:
// - Section Header Block (SHB)
// - Interface Description Block (IDB)
// - Enhanced Packet Block (EPB)
//
// The incremental writer supports writing an `epb_flags` option for packet
// direction.

// Avoid TypeScript `enum` declarations so this module can be executed directly
// under Node with `--experimental-strip-types` (strip-only mode does not
// transform enums). This is required by worker_threads unit tests that load the
// net worker directly in Node.
export const LinkType = {
  Ethernet: 1,
  User0: 147,
  User1: 148,
} as const;
export type LinkType = (typeof LinkType)[keyof typeof LinkType];
export const PacketDirection = {
  Inbound: 0,
  Outbound: 1,
} as const;
export type PacketDirection = (typeof PacketDirection)[keyof typeof PacketDirection];

const textEncoder = new TextEncoder();

function alignUp(n: number, align: number): number {
  // Only used with 4 in this module; still keep it generic.
  if (!Number.isInteger(align) || align <= 0) throw new RangeError(`invalid align: ${align}`);
  return Math.ceil(n / align) * align;
}

function padLen32(n: number): number {
  return (4 - (n % 4)) % 4;
}

function assertU16Len(n: number, what: string): void {
  if (!Number.isInteger(n) || n < 0 || n > 0xffff) {
    throw new RangeError(`${what} must fit in u16: ${n}`);
  }
}

function assertU32(n: number, what: string): void {
  if (!Number.isInteger(n) || n < 0 || n > 0xffff_ffff) {
    throw new RangeError(`${what} must fit in u32: ${n}`);
  }
}

class ByteBuilder {
  private buf: Uint8Array<ArrayBuffer>;
  private view: DataView;
  private len = 0;

  constructor(initialCapacity = 1024) {
    const cap = Math.max(0, Math.floor(initialCapacity));
    this.buf = new Uint8Array(cap);
    this.view = new DataView(this.buf.buffer);
  }

  get length(): number {
    return this.len;
  }

  private ensureCapacity(additionalBytes: number): void {
    const add = Math.max(0, Math.floor(additionalBytes));
    const required = this.len + add;
    if (required <= this.buf.byteLength) return;

    // Doubling growth keeps this amortized O(n) without frequent reallocations.
    const prev = this.buf.byteLength;
    let next = prev > 0 ? prev : 1024;
    while (next < required) {
      next = Math.max(next * 2, required);
    }

    const newBuf = new Uint8Array(next);
    newBuf.set(this.buf.subarray(0, this.len));
    this.buf = newBuf as Uint8Array<ArrayBuffer>;
    this.view = new DataView(this.buf.buffer);
  }

  writeU8(v: number): void {
    this.ensureCapacity(1);
    this.buf[this.len] = v & 0xff;
    this.len += 1;
  }

  writeU16LE(v: number): void {
    this.ensureCapacity(2);
    this.view.setUint16(this.len, v & 0xffff, true);
    this.len += 2;
  }

  writeU32LE(v: number): void {
    this.ensureCapacity(4);
    this.view.setUint32(this.len, v >>> 0, true);
    this.len += 4;
  }

  writeU64LE(v: bigint): void {
    this.ensureCapacity(8);
    const lo = Number(v & 0xffff_ffffn);
    const hi = Number((v >> 32n) & 0xffff_ffffn);
    this.view.setUint32(this.len, lo, true);
    this.view.setUint32(this.len + 4, hi, true);
    this.len += 8;
  }

  writeBytes(bytes: Uint8Array): void {
    this.ensureCapacity(bytes.byteLength);
    this.buf.set(bytes, this.len);
    this.len += bytes.byteLength;
  }

  writeZeros(n: number): void {
    const count = Math.max(0, Math.floor(n));
    this.ensureCapacity(count);
    this.buf.fill(0, this.len, this.len + count);
    this.len += count;
  }

  padTo32(): void {
    this.writeZeros(padLen32(this.len));
  }

  intoBytes(): Uint8Array<ArrayBuffer> {
    // Avoid copying: callers can rely on byteLength; the underlying ArrayBuffer
    // may be larger than the written region due to growth strategy.
    return this.buf.subarray(0, this.len) as Uint8Array<ArrayBuffer>;
  }
}

function optTotalLen(valLen: number): number {
  return 4 + alignUp(valLen, 4);
}

function writeOptEnd(out: ByteBuilder): void {
  out.writeU16LE(0);
  out.writeU16LE(0);
}

function writeOptU8(out: ByteBuilder, code: number, val: number): void {
  out.writeU16LE(code);
  out.writeU16LE(1);
  out.writeU8(val);
  out.padTo32();
}

function writeOptU32(out: ByteBuilder, code: number, val: number): void {
  out.writeU16LE(code);
  out.writeU16LE(4);
  out.writeU32LE(val);
  out.padTo32();
}

export class PcapngWriter {
  private readonly buf: ByteBuilder;
  private nextInterfaceId = 0;

  constructor(userAppl: string) {
    this.buf = new ByteBuilder();
    this.writeSectionHeaderBlock(userAppl);
  }

  addInterface(linkType: LinkType, name: string): number {
    const id = this.nextInterfaceId;
    this.nextInterfaceId += 1;
    this.writeInterfaceDescriptionBlock(linkType, name);
    return id;
  }

  writePacket(
    interfaceId: number,
    timestampNs: bigint,
    payload: Uint8Array,
    direction?: PacketDirection,
  ): void {
    this.writeEnhancedPacketBlock(interfaceId, timestampNs, payload, direction);
  }

  intoBytes(): Uint8Array<ArrayBuffer> {
    return this.buf.intoBytes();
  }

  private writeSectionHeaderBlock(userAppl: string): void {
    const BLOCK_TYPE = 0x0a0d0d0a;

    const userApplBytes = textEncoder.encode(userAppl);
    assertU16Len(userApplBytes.byteLength, "shb_userappl option length");

    const bodyLen = 16;
    const optsLen = optTotalLen(userApplBytes.byteLength) + 4; // + opt_end
    const totalLen = 12 + bodyLen + optsLen;
    assertU32(totalLen, "pcapng SHB total length");

    this.buf.writeU32LE(BLOCK_TYPE);
    this.buf.writeU32LE(totalLen);

    // Body.
    this.buf.writeU32LE(0x1a2b3c4d); // byte-order magic
    this.buf.writeU16LE(1); // major
    this.buf.writeU16LE(0); // minor
    this.buf.writeU64LE(0xffff_ffff_ffff_ffffn); // section length: unspecified

    // Options.
    this.buf.writeU16LE(4); // shb_userappl
    this.buf.writeU16LE(userApplBytes.byteLength);
    this.buf.writeBytes(userApplBytes);
    this.buf.padTo32();
    writeOptEnd(this.buf);

    // Footer.
    this.buf.writeU32LE(totalLen);
  }

  private writeInterfaceDescriptionBlock(linkType: LinkType, name: string): void {
    const BLOCK_TYPE = 0x0000_0001;

    const nameBytes = textEncoder.encode(name);
    assertU16Len(nameBytes.byteLength, "if_name option length");

    const bodyLen = 8;
    const optsLen = optTotalLen(nameBytes.byteLength) + optTotalLen(1) + 4; // if_name + if_tsresol + opt_end
    const totalLen = 12 + bodyLen + optsLen;
    assertU32(totalLen, "pcapng IDB total length");

    this.buf.writeU32LE(BLOCK_TYPE);
    this.buf.writeU32LE(totalLen);

    // Body.
    this.buf.writeU16LE(linkType);
    this.buf.writeU16LE(0); // reserved
    this.buf.writeU32LE(65535); // snaplen

    // Options.
    // if_name
    this.buf.writeU16LE(2);
    this.buf.writeU16LE(nameBytes.byteLength);
    this.buf.writeBytes(nameBytes);
    this.buf.padTo32();

    // if_tsresol (10^-9)
    writeOptU8(this.buf, 9, 9);

    writeOptEnd(this.buf);

    // Footer.
    this.buf.writeU32LE(totalLen);
  }

  private writeEnhancedPacketBlock(
    interfaceId: number,
    timestampNs: bigint,
    payload: Uint8Array,
    direction: PacketDirection | undefined,
  ): void {
    const BLOCK_TYPE = 0x0000_0006;

    const payloadLen = payload.byteLength;
    const payloadPad = padLen32(payloadLen);
    const bodyLen = 20 + payloadLen + payloadPad;

    const optsLen = (direction === undefined ? 0 : optTotalLen(4)) + 4; // + opt_end
    const totalLen = 12 + bodyLen + optsLen;
    assertU32(totalLen, "pcapng EPB total length");

    this.buf.writeU32LE(BLOCK_TYPE);
    this.buf.writeU32LE(totalLen);

    // Body.
    assertU32(interfaceId, "pcapng interface id");
    this.buf.writeU32LE(interfaceId);

    if (timestampNs < 0n || timestampNs > 0xffff_ffff_ffff_ffffn) {
      throw new RangeError(`pcapng timestamp_ns must fit in u64: ${timestampNs.toString()}`);
    }
    const tsHigh = Number((timestampNs >> 32n) & 0xffff_ffffn);
    const tsLow = Number(timestampNs & 0xffff_ffffn);
    this.buf.writeU32LE(tsHigh);
    this.buf.writeU32LE(tsLow);

    const capLen = payloadLen > 0xffff_ffff ? 0xffff_ffff : payloadLen;
    this.buf.writeU32LE(capLen);
    this.buf.writeU32LE(capLen);

    this.buf.writeBytes(payload);
    this.buf.writeZeros(payloadPad);

    // Options.
    if (direction !== undefined) {
      const dirBits = direction === PacketDirection.Inbound ? 1 : 2;
      writeOptU32(this.buf, 2, dirBits); // epb_flags
    }
    writeOptEnd(this.buf);

    // Footer.
    this.buf.writeU32LE(totalLen);
  }
}

// ---------------------------------------------------------------------------
// Convenience encoder used by `NetTracer`.
// ---------------------------------------------------------------------------

const PCAPNG_BLOCK_TYPE_SECTION_HEADER = 0x0a0d0d0a;
const PCAPNG_BLOCK_TYPE_INTERFACE_DESCRIPTION = 0x0000_0001;
const PCAPNG_BLOCK_TYPE_ENHANCED_PACKET = 0x0000_0006;

const PCAPNG_BYTE_ORDER_MAGIC = 0x1a2b3c4d;

// https://www.ietf.org/archive/id/draft-tuexen-opsawg-pcapng-02.html
// (PCAPNG registry): LINKTYPE_ETHERNET
export const PCAPNG_LINKTYPE_ETHERNET = LinkType.Ethernet;
// LINKTYPE_USER0 / LINKTYPE_USER1
// Used by the Rust net tracer for proxy pseudo-packets ("ATCP"/"AUDP").
export const PCAPNG_LINKTYPE_USER0 = LinkType.User0;
export const PCAPNG_LINKTYPE_USER1 = LinkType.User1;

const PCAPNG_IF_OPTION_NAME = 2;
const PCAPNG_IF_OPTION_TSRESOL = 9;

// Enhanced Packet Block options.
export const PCAPNG_EPB_OPTION_FLAGS = 2;
export const PCAPNG_EPB_DIR_MASK = 0b11;
export const PCAPNG_EPB_DIR_INBOUND = 1;
export const PCAPNG_EPB_DIR_OUTBOUND = 2;

function pad4(len: number): number {
  return (4 - (len & 3)) & 3;
}

class ByteWriter {
  readonly out: Uint8Array<ArrayBuffer>;
  private readonly view: DataView;
  private off = 0;

  constructor(size: number) {
    this.out = new Uint8Array(size);
    this.view = new DataView(this.out.buffer);
  }

  writeU8(v: number): void {
    this.out[this.off++] = v & 0xff;
  }

  writeU16(v: number): void {
    this.view.setUint16(this.off, v & 0xffff, true);
    this.off += 2;
  }

  writeU32(v: number): void {
    this.view.setUint32(this.off, v >>> 0, true);
    this.off += 4;
  }

  writeBytes(bytes: Uint8Array): void {
    this.out.set(bytes, this.off);
    this.off += bytes.byteLength;
  }

  writeZeros(len: number): void {
    this.out.fill(0, this.off, this.off + len);
    this.off += len;
  }

  finish(): Uint8Array<ArrayBuffer> {
    if (this.off !== this.out.byteLength) {
      throw new Error(`pcapng writer size mismatch: wrote ${this.off} bytes, expected ${this.out.byteLength}`);
    }
    return this.out;
  }
}

export interface PcapngInterfaceDescription {
  // 16-bit LINKTYPE_* value (e.g. `PCAPNG_LINKTYPE_ETHERNET`).
  linkType: number;
  // Maximum captured packet length for this interface.
  snapLen: number;
  // Optional interface name (Wireshark will show this in the interface list).
  name?: string;
  // Timestamp resolution exponent `N` in units of `10^-N` seconds.
  // For nanoseconds, use 9 (this is what `NetTracer` uses).
  tsResolPower10?: number;
}

export interface PcapngEnhancedPacket {
  interfaceId: number;
  // Timestamp units must match the interface's `if_tsresol` (typically ns).
  timestamp: bigint;
  packet: Uint8Array;
  // Optional Enhanced Packet Block flags (pcapng `epb_flags` option code 2).
  //
  // Bits 0-1 encode packet direction:
  // - 0: unknown
  // - 1: inbound
  // - 2: outbound
  flags?: number;
}

export interface PcapngCapture {
  // Optional SHB `shb_userappl` string (pcapng option code 4).
  // If omitted, defaults to `"aero"` for parity with the Rust tracer.
  userAppl?: string;
  interfaces: readonly PcapngInterfaceDescription[];
  packets: readonly PcapngEnhancedPacket[];
}

function computeInterfaceOptionsLength(desc: PcapngInterfaceDescription): number {
  let len = 0;
  if (desc.name !== undefined) {
    const bytes = textEncoder.encode(desc.name);
    assertU16Len(bytes.byteLength, "if_name option length");
    len += 4 + bytes.byteLength + pad4(bytes.byteLength);
  }
  if (desc.tsResolPower10 !== undefined) {
    // 1 byte value + padding.
    len += 4 + 1 + pad4(1);
  }
  if (len === 0) return 0;
  // End of options.
  return len + 4;
}

function writeInterfaceOptions(w: ByteWriter, desc: PcapngInterfaceDescription): void {
  if (desc.name !== undefined) {
    const bytes = textEncoder.encode(desc.name);
    assertU16Len(bytes.byteLength, "if_name option length");
    w.writeU16(PCAPNG_IF_OPTION_NAME);
    w.writeU16(bytes.byteLength);
    w.writeBytes(bytes);
    w.writeZeros(pad4(bytes.byteLength));
  }
  if (desc.tsResolPower10 !== undefined) {
    w.writeU16(PCAPNG_IF_OPTION_TSRESOL);
    w.writeU16(1);
    w.writeU8(desc.tsResolPower10 & 0xff);
    w.writeZeros(pad4(1));
  }

  // End of options (even if empty, only written when caller decided options exist).
  w.writeU16(0);
  w.writeU16(0);
}

export function writePcapng(capture: PcapngCapture): Uint8Array<ArrayBuffer> {
  const userAppl = capture.userAppl ?? "aero";
  const userApplBytes = textEncoder.encode(userAppl);
  assertU16Len(userApplBytes.byteLength, "shb_userappl option length");
  const shbOptLen = optTotalLen(userApplBytes.byteLength);
  const shbOptsLen = shbOptLen + 4; // + opt_end

  // Section Header Block: fixed body + (optional) userappl option.
  const shbLen = 28 + shbOptsLen;

  let totalLen = shbLen;

  const idbOptionLens = capture.interfaces.map(computeInterfaceOptionsLength);
  for (const [idx, iface] of capture.interfaces.entries()) {
    const optLen = idbOptionLens[idx];
    totalLen += 20 + optLen;
  }

  for (const pkt of capture.packets) {
    const dataLen = pkt.packet.byteLength;
    const optsLen = pkt.flags === undefined ? 0 : 12; // flags + end-of-options
    totalLen += 32 + dataLen + pad4(dataLen) + optsLen;
  }

  assertU32(totalLen, "pcapng capture total length");
  const w = new ByteWriter(totalLen);

  // SHB
  w.writeU32(PCAPNG_BLOCK_TYPE_SECTION_HEADER);
  w.writeU32(shbLen);
  w.writeU32(PCAPNG_BYTE_ORDER_MAGIC);
  w.writeU16(1); // major
  w.writeU16(0); // minor
  w.writeU32(0xffff_ffff); // section length low (unknown)
  w.writeU32(0xffff_ffff); // section length high (unknown)
  // shb_userappl option
  w.writeU16(4); // shb_userappl
  w.writeU16(userApplBytes.byteLength);
  w.writeBytes(userApplBytes);
  w.writeZeros(pad4(userApplBytes.byteLength));
  // End of options.
  w.writeU16(0);
  w.writeU16(0);
  w.writeU32(shbLen);

  // IDBs
  for (const [idx, iface] of capture.interfaces.entries()) {
    const optLen = idbOptionLens[idx];
    const idbLen = 20 + optLen;

    w.writeU32(PCAPNG_BLOCK_TYPE_INTERFACE_DESCRIPTION);
    w.writeU32(idbLen);
    w.writeU16(iface.linkType);
    w.writeU16(0); // reserved
    w.writeU32(iface.snapLen >>> 0);
    if (optLen !== 0) {
      writeInterfaceOptions(w, iface);
    }
    w.writeU32(idbLen);
  }

  // EPBs
  for (const pkt of capture.packets) {
    const dataLen = pkt.packet.byteLength;
    const paddedDataLen = dataLen + pad4(dataLen);
    const optsLen = pkt.flags === undefined ? 0 : 12; // flags + end-of-options
    const epbLen = 32 + paddedDataLen + optsLen;

    const ts = pkt.timestamp;
    const tsLo = Number(ts & 0xffff_ffffn);
    const tsHi = Number((ts >> 32n) & 0xffff_ffffn);

    w.writeU32(PCAPNG_BLOCK_TYPE_ENHANCED_PACKET);
    w.writeU32(epbLen);
    w.writeU32(pkt.interfaceId >>> 0);
    w.writeU32(tsHi);
    w.writeU32(tsLo);
    w.writeU32(dataLen >>> 0);
    w.writeU32(dataLen >>> 0);
    w.writeBytes(pkt.packet);
    w.writeZeros(pad4(dataLen));
    if (pkt.flags !== undefined) {
      w.writeU16(PCAPNG_EPB_OPTION_FLAGS);
      w.writeU16(4);
      w.writeU32(pkt.flags >>> 0);
      // End of options.
      w.writeU16(0);
      w.writeU16(0);
    }
    w.writeU32(epbLen);
  }

  return w.finish();
}
