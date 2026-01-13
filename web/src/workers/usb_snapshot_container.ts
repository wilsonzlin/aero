export type UsbSnapshotContainerEntry = { tag: string; bytes: Uint8Array };

// "AUSB" in a u32 little-endian encoding.
const MAGIC = 0x42_53_55_41;
const HEADER_BYTES = 8; // magic:u32 + version:u16 + flags:u16
const ENTRY_HEADER_BYTES = 8; // tag:4 bytes + len:u32

export const USB_SNAPSHOT_CONTAINER_VERSION = 1;
export const USB_SNAPSHOT_TAG_UHCI = "UHCI";
export const USB_SNAPSHOT_TAG_EHCI = "EHCI";

function readU32LE(bytes: Uint8Array, offset: number): number {
  return (
    ((bytes[offset] ?? 0) |
      ((bytes[offset + 1] ?? 0) << 8) |
      ((bytes[offset + 2] ?? 0) << 16) |
      ((bytes[offset + 3] ?? 0) << 24)) >>>
    0
  );
}

function writeU32LE(out: Uint8Array, offset: number, value: number): void {
  out[offset] = value & 0xff;
  out[offset + 1] = (value >>> 8) & 0xff;
  out[offset + 2] = (value >>> 16) & 0xff;
  out[offset + 3] = (value >>> 24) & 0xff;
}

function writeU16LE(out: Uint8Array, offset: number, value: number): void {
  out[offset] = value & 0xff;
  out[offset + 1] = (value >>> 8) & 0xff;
}

function tagToBytes(tag: string): Uint8Array | null {
  if (tag.length !== 4) return null;
  const out = new Uint8Array(4);
  for (let i = 0; i < 4; i++) {
    const code = tag.charCodeAt(i);
    // Enforce printable ASCII so the encoding is stable/debuggable.
    if (code < 0x20 || code > 0x7e) return null;
    out[i] = code & 0xff;
  }
  return out;
}

function bytesToTag(bytes: Uint8Array, offset: number): string {
  return String.fromCharCode(bytes[offset] ?? 0, bytes[offset + 1] ?? 0, bytes[offset + 2] ?? 0, bytes[offset + 3] ?? 0);
}

export function isUsbSnapshotContainer(bytes: Uint8Array): boolean {
  if (bytes.byteLength < HEADER_BYTES) return false;
  return readU32LE(bytes, 0) === MAGIC;
}

/**
 * Encode a deterministic USB snapshot container that can hold multiple controller snapshots.
 *
 * Format (little-endian):
 * - magic: u32 = "AUSB"
 * - version: u16
 * - flags: u16 (reserved; must be 0 for v1)
 * - entries...:
 *   - tag: [u8;4] (FourCC, e.g. "UHCI", "EHCI")
 *   - len: u32
 *   - payload: [u8;len]
 */
export function encodeUsbSnapshotContainer(entries: UsbSnapshotContainerEntry[], opts?: { version?: number; flags?: number }): Uint8Array {
  const version = opts?.version ?? USB_SNAPSHOT_CONTAINER_VERSION;
  const flags = opts?.flags ?? 0;

  const normalized: Array<{ tag: string; tagBytes: Uint8Array; bytes: Uint8Array }> = [];
  for (const entry of entries) {
    const tagBytes = tagToBytes(entry.tag);
    if (!tagBytes) {
      throw new Error(`Invalid USB snapshot container tag: ${entry.tag} (expected 4 printable ASCII characters)`);
    }
    normalized.push({ tag: entry.tag, tagBytes, bytes: entry.bytes });
  }

  // Deterministic ordering.
  normalized.sort((a, b) => (a.tag < b.tag ? -1 : a.tag > b.tag ? 1 : 0));

  let total = HEADER_BYTES;
  for (const e of normalized) {
    total += ENTRY_HEADER_BYTES + e.bytes.byteLength;
  }

  const out = new Uint8Array(total);
  writeU32LE(out, 0, MAGIC);
  writeU16LE(out, 4, version & 0xffff);
  writeU16LE(out, 6, flags & 0xffff);

  let off = HEADER_BYTES;
  for (const e of normalized) {
    out.set(e.tagBytes, off);
    writeU32LE(out, off + 4, e.bytes.byteLength >>> 0);
    out.set(e.bytes, off + ENTRY_HEADER_BYTES);
    off += ENTRY_HEADER_BYTES + e.bytes.byteLength;
  }

  return out;
}

export function decodeUsbSnapshotContainer(bytes: Uint8Array): { version: number; flags: number; entries: UsbSnapshotContainerEntry[] } | null {
  if (!isUsbSnapshotContainer(bytes)) return null;
  if (bytes.byteLength < HEADER_BYTES) return null;

  const version = ((bytes[4] ?? 0) | ((bytes[5] ?? 0) << 8)) >>> 0;
  const flags = ((bytes[6] ?? 0) | ((bytes[7] ?? 0) << 8)) >>> 0;

  const entries: UsbSnapshotContainerEntry[] = [];
  let off = HEADER_BYTES;
  while (off < bytes.byteLength) {
    if (bytes.byteLength - off < ENTRY_HEADER_BYTES) return null;
    const tag = bytesToTag(bytes, off);
    const len = readU32LE(bytes, off + 4);
    off += ENTRY_HEADER_BYTES;
    if (len > bytes.byteLength - off) return null;
    entries.push({ tag, bytes: bytes.subarray(off, off + len) });
    off += len;
  }

  return { version, flags, entries };
}
