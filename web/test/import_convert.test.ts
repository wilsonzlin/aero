import test from "node:test";
import assert from "node:assert/strict";

import { convertToAeroSparse, detectFormat } from "../src/storage/import_convert.ts";
import { crc32Final, crc32Init, crc32ToHex, crc32Update } from "../src/storage/crc32.ts";

class MemSource {
  readonly size: number;
  private readonly data: Uint8Array;

  constructor(data: Uint8Array) {
    this.data = data;
    this.size = data.byteLength;
  }

  async readAt(offset: number, length: number): Promise<Uint8Array> {
    const end = offset + length;
    if (offset < 0 || length < 0 || end > this.data.byteLength) {
      throw new RangeError(`readAt out of range: ${offset}+${length} (size=${this.data.byteLength})`);
    }
    return this.data.slice(offset, end);
  }
}

class MemSyncAccessHandle {
  private buf = new Uint8Array(0);

  read(buffer: ArrayBufferView, options?: { at: number }): number {
    const at = options?.at ?? 0;
    const view = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    const avail = Math.max(0, Math.min(view.byteLength, this.buf.byteLength - at));
    view.set(this.buf.subarray(at, at + avail));
    if (avail < view.byteLength) view.fill(0, avail);
    return avail;
  }

  write(buffer: ArrayBufferView, options?: { at: number }): number {
    const at = options?.at ?? 0;
    const view = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    const end = at + view.byteLength;
    if (end > this.buf.byteLength) {
      const next = new Uint8Array(end);
      next.set(this.buf);
      this.buf = next;
    }
    this.buf.set(view, at);
    return view.byteLength;
  }

  flush(): void {}
  close(): void {}

  getSize(): number {
    return this.buf.byteLength;
  }

  truncate(size: number): void {
    if (!Number.isSafeInteger(size) || size < 0) throw new Error(`truncate: invalid size=${size}`);
    if (size === this.buf.byteLength) return;
    if (size < this.buf.byteLength) {
      this.buf = this.buf.slice(0, size);
      return;
    }
    const next = new Uint8Array(size);
    next.set(this.buf);
    this.buf = next;
  }

  toBytes(): Uint8Array {
    return this.buf.slice();
  }
}

function writeU32BE(buf: Uint8Array, offset: number, value: number): void {
  buf[offset] = (value >>> 24) & 0xff;
  buf[offset + 1] = (value >>> 16) & 0xff;
  buf[offset + 2] = (value >>> 8) & 0xff;
  buf[offset + 3] = value & 0xff;
}

function writeU64BE(buf: Uint8Array, offset: number, value: bigint): void {
  writeU32BE(buf, offset, Number((value >> 32n) & 0xffff_ffffn));
  writeU32BE(buf, offset + 4, Number(value & 0xffff_ffffn));
}

function u64le(v: bigint): Uint8Array {
  const out = new Uint8Array(8);
  let x = v;
  for (let i = 0; i < 8; i++) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  return out;
}

function vhdChecksum(bytes: Uint8Array, checksumOffset: number): number {
  const copy = bytes.slice();
  copy.fill(0, checksumOffset, checksumOffset + 4);
  let sum = 0;
  for (const b of copy) sum = (sum + b) >>> 0;
  return (~sum) >>> 0;
}

function buildQcow2Fixture(): { file: Uint8Array; logical: Uint8Array } {
  const clusterSize = 512;
  const logicalSize = 1024;

  const refcountTableOffset = 512;
  const l1Offset = 1024;
  const l2Offset = 1536;
  const data0Offset = 2048;
  const fileSize = data0Offset + clusterSize;

  const file = new Uint8Array(fileSize);
  // magic "QFI\xfb"
  file.set([0x51, 0x46, 0x49, 0xfb], 0);
  writeU32BE(file, 4, 2); // version
  writeU64BE(file, 8, 0n); // backing file offset
  writeU32BE(file, 16, 0); // backing file size
  writeU32BE(file, 20, 9); // cluster bits (512B clusters)
  writeU64BE(file, 24, BigInt(logicalSize));
  writeU32BE(file, 32, 0); // crypt method
  writeU32BE(file, 36, 1); // l1 size
  writeU64BE(file, 40, BigInt(l1Offset));
  writeU64BE(file, 48, BigInt(refcountTableOffset));
  writeU32BE(file, 56, 1); // refcount_table_clusters
  // nb_snapshots at 60 is 0 by default.

  // L1 table (1 entry)
  writeU64BE(file, l1Offset + 0, BigInt(l2Offset));

  // L2 table (64 entries, but we only need 2)
  writeU64BE(file, l2Offset + 0, BigInt(data0Offset)); // cluster 0 allocated
  writeU64BE(file, l2Offset + 8, 0n); // cluster 1 unallocated

  const cluster0 = new Uint8Array(clusterSize);
  for (let i = 0; i < cluster0.length; i++) cluster0[i] = i & 0xff;
  file.set(cluster0, data0Offset);

  const logical = new Uint8Array(logicalSize);
  logical.set(cluster0, 0);
  return { file, logical };
}

function buildDynamicVhdFixture(maxTableEntries = 1): { file: Uint8Array; logical: Uint8Array } {
  const footerSize = 512;
  const dynHeaderOffset = 512;
  const dynHeaderSize = 1024;
  const batOffset = 1536;
  const blockOff = 2048;

  const blockSize = 1024; // 2 sectors
  const bitmapSize = 512;
  const logicalSize = 1024;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  writeU64BE(footer, 16, BigInt(dynHeaderOffset)); // data offset
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 3); // disk type dynamic
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const dyn = new Uint8Array(dynHeaderSize);
  dyn.set(new TextEncoder().encode("cxsparse"), 0);
  writeU64BE(dyn, 8, 0xffff_ffff_ffff_ffffn);
  writeU64BE(dyn, 16, BigInt(batOffset));
  writeU32BE(dyn, 24, 0x0001_0000);
  writeU32BE(dyn, 28, maxTableEntries); // max table entries
  writeU32BE(dyn, 32, blockSize);
  writeU32BE(dyn, 36, vhdChecksum(dyn, 36));

  const fileSize = blockOff + bitmapSize + blockSize + footerSize;
  const file = new Uint8Array(fileSize);

  // Optional first footer copy (helps magic detection at offset 0).
  file.set(footer, 0);
  file.set(dyn, dynHeaderOffset);

  // BAT entries: sector offset of blocks (big-endian u32)
  writeU32BE(file, batOffset, blockOff / 512);
  for (let i = 1; i < maxTableEntries; i++) writeU32BE(file, batOffset + i * 4, 0xffff_ffff);

  // Block bitmap + data.
  file[blockOff] = 0x80; // sector 0 allocated, sector 1 unallocated
  const dataBase = blockOff + bitmapSize;
  const sector0 = new Uint8Array(512);
  for (let i = 0; i < sector0.length; i++) sector0[i] = (0xa0 + i) & 0xff;
  file.set(sector0, dataBase);
  file.fill(0x55, dataBase + 512, dataBase + 1024); // should be ignored (bitmap bit clear)

  // Footer at end.
  file.set(footer, fileSize - footerSize);

  const logical = new Uint8Array(logicalSize);
  logical.set(sector0, 0);
  return { file, logical };
}

function buildDynamicVhdFixtureTwoBlocks(options: { overlap: boolean }): { file: Uint8Array } {
  const footerSize = 512;
  const dynHeaderOffset = 512;
  const dynHeaderSize = 1024;
  const batOffset = 1536;
  const blockSize = 1024;
  const bitmapSize = 512;
  const logicalSize = 2048;

  const block0Off = 2048;
  const blockTotalSize = bitmapSize + blockSize;
  const block1Off = options.overlap ? block0Off : block0Off + blockTotalSize;
  const footerOff = Math.max(block0Off, block1Off) + blockTotalSize;
  const fileSize = footerOff + footerSize;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  writeU64BE(footer, 16, BigInt(dynHeaderOffset)); // data offset
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 3); // disk type dynamic
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const dyn = new Uint8Array(dynHeaderSize);
  dyn.set(new TextEncoder().encode("cxsparse"), 0);
  writeU64BE(dyn, 8, 0xffff_ffff_ffff_ffffn);
  writeU64BE(dyn, 16, BigInt(batOffset));
  writeU32BE(dyn, 24, 0x0001_0000);
  writeU32BE(dyn, 28, 2); // max table entries
  writeU32BE(dyn, 32, blockSize);
  writeU32BE(dyn, 36, vhdChecksum(dyn, 36));

  const file = new Uint8Array(fileSize);
  // Footer copy at offset 0.
  file.set(footer, 0);
  file.set(dyn, dynHeaderOffset);

  // BAT entries.
  writeU32BE(file, batOffset, block0Off / 512);
  writeU32BE(file, batOffset + 4, block1Off / 512);

  // Footer at end.
  file.set(footer, footerOff);
  return { file };
}

function buildFixedVhdFixtureWithFooterCopy(): { file: Uint8Array; logical: Uint8Array } {
  const footerSize = 512;
  const logicalSize = 512;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  // Fixed disks use dataOffset = u64::MAX.
  writeU64BE(footer, 16, 0xffff_ffff_ffff_ffffn);
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 2); // disk type fixed
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  // Some tools may write a footer copy at offset 0 that differs from the EOF footer but still
  // describes the same disk size.
  const footer0 = footer.slice();
  writeU64BE(footer0, 40, BigInt(logicalSize + 512)); // original size (ignored by conversion)
  writeU32BE(footer0, 64, vhdChecksum(footer0, 64));

  const fileSize = footerSize + logicalSize + footerSize;
  const file = new Uint8Array(fileSize);
  // Optional footer copy at offset 0.
  file.set(footer0, 0);

  const sector0 = new Uint8Array(512);
  for (let i = 0; i < sector0.length; i++) sector0[i] = (0x10 + i) & 0xff;
  file.set(sector0, footerSize);

  // Footer at end.
  file.set(footer, fileSize - footerSize);

  const logical = new Uint8Array(logicalSize);
  logical.set(sector0, 0);
  return { file, logical };
}

function buildFixedVhdFixtureWithFooterCopyNonIdentical(): { file: Uint8Array; logical: Uint8Array } {
  const footerSize = 512;
  const logicalSize = 512;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  // Fixed disks use dataOffset = u64::MAX.
  writeU64BE(footer, 16, 0xffff_ffff_ffff_ffffn);
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 2); // disk type fixed
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  // Make a valid footer copy that differs from the EOF footer (e.g. timestamp).
  const footerCopy = footer.slice();
  writeU32BE(footerCopy, 24, 1234); // timestamp
  writeU32BE(footerCopy, 64, vhdChecksum(footerCopy, 64));

  const fileSize = footerSize + logicalSize + footerSize;
  const file = new Uint8Array(fileSize);
  // Footer copy at offset 0 (non-identical).
  file.set(footerCopy, 0);

  const sector0 = new Uint8Array(512);
  for (let i = 0; i < sector0.length; i++) sector0[i] = (0x11 + i) & 0xff;
  file.set(sector0, footerSize);

  // Footer at end.
  file.set(footer, fileSize - footerSize);

  const logical = new Uint8Array(logicalSize);
  logical.set(sector0, 0);
  return { file, logical };
}

function buildFixedVhdFixtureWithoutFooterCopyButSector0LooksLikeFooter(): { file: Uint8Array; logical: Uint8Array } {
  const footerSize = 512;
  const logicalSize = 512;

  const eofFooter = new Uint8Array(footerSize);
  eofFooter.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(eofFooter, 8, 2); // features
  writeU32BE(eofFooter, 12, 0x0001_0000); // file_format_version
  // Fixed disks use dataOffset = u64::MAX.
  writeU64BE(eofFooter, 16, 0xffff_ffff_ffff_ffffn);
  writeU64BE(eofFooter, 48, BigInt(logicalSize)); // current size
  writeU32BE(eofFooter, 60, 2); // disk type fixed
  writeU32BE(eofFooter, 24, 5678); // timestamp (arbitrary)
  writeU32BE(eofFooter, 64, vhdChecksum(eofFooter, 64));

  // Make sector 0 look like a valid footer too (same size/type), but not identical to the EOF footer.
  const sector0 = eofFooter.slice();
  writeU32BE(sector0, 24, 1234); // timestamp differs
  writeU32BE(sector0, 64, vhdChecksum(sector0, 64));

  const fileSize = logicalSize + footerSize;
  const file = new Uint8Array(fileSize);
  // Disk payload begins at offset 0 (no footer copy).
  file.set(sector0, 0);
  // Required footer at EOF.
  file.set(eofFooter, fileSize - footerSize);

  const logical = new Uint8Array(logicalSize);
  logical.set(sector0, 0);
  return { file, logical };
}

function buildFixedVhdFixtureInvalidFixedDataOffset(): { file: Uint8Array } {
  const footerSize = 512;
  const logicalSize = 512;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  // Fixed disks must use dataOffset = u64::MAX; use u64::MAX-1 (which rounds to the same JS number)
  // to ensure validation is performed at bigint precision.
  writeU64BE(footer, 16, 0xffff_ffff_ffff_fffen);
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 2); // disk type fixed
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const fileSize = logicalSize + footerSize;
  const file = new Uint8Array(fileSize);
  file.fill(0x5a, 0, logicalSize);
  file.set(footer, fileSize - footerSize);
  return { file };
}

function buildFixedVhdFixture(): { file: Uint8Array; logical: Uint8Array } {
  const footerSize = 512;
  const logicalSize = 512;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  // Fixed disks use dataOffset = u64::MAX.
  writeU64BE(footer, 16, 0xffff_ffff_ffff_ffffn);
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 2); // disk type fixed
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const fileSize = logicalSize + footerSize;
  const file = new Uint8Array(fileSize);

  const sector0 = new Uint8Array(512);
  for (let i = 0; i < sector0.length; i++) sector0[i] = (0x20 + i) & 0xff;
  file.set(sector0, 0);

  // Footer at end.
  file.set(footer, fileSize - footerSize);

  const logical = new Uint8Array(logicalSize);
  logical.set(sector0, 0);
  return { file, logical };
}

function buildIsoFixture(): Uint8Array {
  const size = 0x8001 + 5;
  const file = new Uint8Array(size);
  file.set(new TextEncoder().encode("CD001"), 0x8001);
  return file;
}

type ParsedSparse = {
  blockSizeBytes: number;
  diskSizeBytes: number;
  table: number[];
  file: Uint8Array;
};

function parseAeroSparse(file: Uint8Array): ParsedSparse {
  assert.ok(file.byteLength >= 64, "file too small");
  const magic = new TextDecoder().decode(file.slice(0, 8));
  assert.equal(magic, "AEROSPAR");
  const view = new DataView(file.buffer, file.byteOffset, file.byteLength);
  const version = view.getUint32(8, true);
  assert.equal(version, 1);
  const headerSize = view.getUint32(12, true);
  assert.equal(headerSize, 64);
  const blockSizeBytes = view.getUint32(16, true);
  const diskSizeBytes = Number(view.getBigUint64(24, true));
  const tableOffset = Number(view.getBigUint64(32, true));
  assert.equal(tableOffset, 64);
  const tableEntries = Number(view.getBigUint64(40, true));
  const tableBytes = tableEntries * 8;
  assert.ok(tableOffset + tableBytes <= file.byteLength, "table out of range");
  const tableView = new DataView(file.buffer, file.byteOffset + tableOffset, tableBytes);
  const table = new Array<number>(tableEntries);
  for (let i = 0; i < tableEntries; i++) {
    table[i] = Number(tableView.getBigUint64(i * 8, true));
  }
  return { blockSizeBytes, diskSizeBytes, table, file };
}

function readLogical(parsed: ParsedSparse, offset: number, length: number): Uint8Array {
  assert.ok(offset >= 0 && length >= 0);
  const end = offset + length;
  assert.ok(end <= parsed.diskSizeBytes, "read past end");
  const out = new Uint8Array(length);

  let pos = 0;
  while (pos < length) {
    const abs = offset + pos;
    const blockIndex = Math.floor(abs / parsed.blockSizeBytes);
    const within = abs % parsed.blockSizeBytes;
    const chunkLen = Math.min(parsed.blockSizeBytes - within, length - pos);
    const phys = parsed.table[blockIndex] ?? 0;
    if (phys !== 0) {
      out.set(parsed.file.subarray(phys + within, phys + within + chunkLen), pos);
    }
    pos += chunkLen;
  }
  return out;
}

function sparseChecksumCrc32(parsed: ParsedSparse): string {
  let crc = crc32Init();
  for (let blockIndex = 0; blockIndex < parsed.table.length; blockIndex++) {
    const phys = parsed.table[blockIndex]!;
    if (phys === 0) continue;
    const block = parsed.file.subarray(phys, phys + parsed.blockSizeBytes);
    crc = crc32Update(crc, u64le(BigInt(blockIndex)));
    crc = crc32Update(crc, block);
  }
  return crc32ToHex(crc32Final(crc));
}

test("detectFormat: qcow2/vhd/iso signatures", async () => {
  {
    const { file } = buildQcow2Fixture();
    const fmt = await detectFormat(new MemSource(file), "disk.unknown");
    assert.equal(fmt, "qcow2");
  }
  {
    const { file } = buildDynamicVhdFixture();
    const fmt = await detectFormat(new MemSource(file), "disk.unknown");
    assert.equal(fmt, "vhd");
  }
  {
    const { file } = buildFixedVhdFixtureWithFooterCopy();
    const fmt = await detectFormat(new MemSource(file), "disk.unknown");
    assert.equal(fmt, "vhd");
  }
  {
    const file = buildIsoFixture();
    const fmt = await detectFormat(new MemSource(file), "disk.unknown");
    assert.equal(fmt, "iso");
  }
});

test("detectFormat: does not misclassify qcow2 magic with invalid version", async () => {
  const file = new Uint8Array(8);
  file.set([0x51, 0x46, 0x49, 0xfb], 0);
  writeU32BE(file, 4, 99);
  const fmt = await detectFormat(new MemSource(file), "disk.unknown");
  assert.equal(fmt, "raw");
});

test("detectFormat: does not misclassify VHD cookie without valid footer", async () => {
  const file = new Uint8Array(512);
  file.set(new TextEncoder().encode("conectix"), 0);
  // Missing file_format_version, checksum, etc.
  const fmt = await detectFormat(new MemSource(file), "disk.unknown");
  assert.equal(fmt, "raw");
});

test("detectFormat: VHD checksum mismatch is still detected as vhd", async () => {
  const { file } = buildFixedVhdFixture();
  const footerOff = file.byteLength - 512;
  file[footerOff + 64] ^= 0xff; // corrupt stored checksum

  const fmt = await detectFormat(new MemSource(file), "disk.unknown");
  assert.equal(fmt, "vhd");
});

test("convertToAeroSparse: raw roundtrip preserves logical bytes and sparseness", async () => {
  const blockSize = 512;
  const logical = new Uint8Array(blockSize * 3);
  // block 0: all zero (should not allocate)
  for (let i = 0; i < blockSize; i++) logical[blockSize + i] = (0x10 + i) & 0xff;
  logical[blockSize * 2] = 0xaa;
  logical[blockSize * 2 + 1] = 0xbb;

  const src = new MemSource(logical);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "raw", sync, { blockSizeBytes: blockSize });
  assert.equal(manifest.originalFormat, "raw");
  assert.equal(manifest.convertedFormat, "aerospar");
  assert.equal(manifest.logicalSize, logical.byteLength);
  assert.equal(manifest.blockSizeBytes, blockSize);

  const parsed = parseAeroSparse(sync.toBytes());
  assert.equal(parsed.blockSizeBytes, blockSize);
  assert.equal(parsed.diskSizeBytes, logical.byteLength);
  assert.equal(parsed.table.length, 3);
  assert.equal(parsed.table[0], 0);
  assert.notEqual(parsed.table[1], 0);
  assert.notEqual(parsed.table[2], 0);

  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: rejects huge output blockSizeBytes", async () => {
  const src = new MemSource(new Uint8Array(512));
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "raw", sync, { blockSizeBytes: 128 * 1024 * 1024 }),
    (err: any) => err instanceof Error && /blockSizeBytes too large/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects aerosparse allocation table larger than 128MiB", async () => {
  class HugeZeroSource {
    readonly size: number;
    constructor(size: number) {
      this.size = size;
    }
    async readAt(_offset: number, _length: number): Promise<Uint8Array> {
      throw new Error("unexpected read");
    }
  }

  const maxTableEntries = (128 * 1024 * 1024) / 8;
  const diskSizeBytes = (maxTableEntries + 1) * 512;
  const src = new HugeZeroSource(diskSizeBytes);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src as any, "raw", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /aerosparse allocation table too large/i.test(err.message),
  );
});

test("convertToAeroSparse: qcow2 sparse copy preserves logical bytes", async () => {
  const { file, logical } = buildQcow2Fixture();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "qcow2", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "qcow2");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(parsed.table.length, 2);
  assert.notEqual(parsed.table[0], 0);
  assert.equal(parsed.table[1], 0, "unallocated qcow2 cluster should remain sparse");
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: rejects qcow2 with too many clusters", async () => {
  // Construct a qcow2 header claiming a huge logical size with tiny clusters (512B).
  // This would require an enormous cluster offset map and should be rejected up front.
  const file = new Uint8Array(72);
  file.set([0x51, 0x46, 0x49, 0xfb], 0); // magic
  writeU32BE(file, 4, 2); // version
  writeU32BE(file, 20, 9); // cluster_bits = 9 => 512B clusters
  writeU64BE(file, 24, 100n * 1024n * 1024n * 1024n); // virtual size = 100 GiB
  writeU32BE(file, 36, 3_276_800); // l1_size (derived from size/clusterSize and l2 entries)
  writeU64BE(file, 40, 512n); // l1_table_offset (not actually present in file)
  writeU64BE(file, 48, 512n); // refcount_table_offset (not actually present in file)

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "qcow2", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /qcow2 too many clusters/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects qcow2 v3 where header_length overlaps tables", async () => {
  const headerLength = 1024;
  const file = new Uint8Array(headerLength);
  file.set([0x51, 0x46, 0x49, 0xfb], 0); // magic
  writeU32BE(file, 4, 3); // version
  writeU32BE(file, 20, 9); // cluster_bits = 9 => 512B clusters
  writeU64BE(file, 24, 512n); // virtual size
  writeU32BE(file, 36, 1); // l1_size
  writeU64BE(file, 40, 512n); // l1_table_offset (cluster-aligned, but within header_length)
  writeU64BE(file, 48, 1024n); // refcount_table_offset
  writeU32BE(file, 56, 1); // refcount_table_clusters
  writeU32BE(file, 96, 4); // refcount_order
  writeU32BE(file, 100, headerLength); // header_length

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "qcow2", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /qcow2 table overlaps header/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects qcow2 v3 incompatible features", async () => {
  const file = new Uint8Array(104);
  file.set([0x51, 0x46, 0x49, 0xfb], 0); // magic
  writeU32BE(file, 4, 3); // version
  writeU32BE(file, 20, 9); // cluster_bits = 9 => 512B clusters
  writeU64BE(file, 24, 512n); // virtual size
  writeU32BE(file, 36, 1); // l1_size
  writeU64BE(file, 40, 512n); // l1_table_offset
  writeU64BE(file, 48, 1024n); // refcount_table_offset
  writeU32BE(file, 56, 1); // refcount_table_clusters
  writeU64BE(file, 72, 1n); // incompatible_features
  writeU32BE(file, 96, 4); // refcount_order
  writeU32BE(file, 100, 104); // header_length

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "qcow2", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /qcow2 incompatible features unsupported/i.test(err.message),
  );
});

test("convertToAeroSparse: dynamic VHD respects BAT + sector bitmap", async () => {
  const { file, logical } = buildDynamicVhdFixture();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "vhd");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: dynamic VHD rejects BAT entries overlapping metadata", async () => {
  const { file } = buildDynamicVhdFixture();
  // Point the first BAT entry at the dynamic header (offset 512).
  writeU32BE(file, 1536, 1);
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD block overlaps metadata/i.test(err.message),
  );
});

test("convertToAeroSparse: dynamic VHD rejects overlapping blocks", async () => {
  const { file } = buildDynamicVhdFixtureTwoBlocks({ overlap: true });
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD blocks overlap/i.test(err.message),
  );
});

test("convertToAeroSparse: dynamic VHD allows max_table_entries > required", async () => {
  const { file, logical } = buildDynamicVhdFixture(2);
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "vhd");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: dynamic VHD rejects footer copy mismatch", async () => {
  const { file } = buildDynamicVhdFixture();

  // Mutate a non-essential field (timestamp) in the offset-0 footer copy while keeping checksum valid.
  const footerCopy = file.slice(0, 512);
  writeU32BE(footerCopy, 24, 1234);
  writeU32BE(footerCopy, 64, vhdChecksum(footerCopy, 64));
  file.set(footerCopy, 0);

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD footer copy mismatch/i.test(err.message),
  );
});

test("convertToAeroSparse: dynamic VHD rejects BAT overlapping dynamic header", async () => {
  const { file } = buildDynamicVhdFixture();
  const dynHeaderOffset = 512;

  const dyn = file.slice(dynHeaderOffset, dynHeaderOffset + 1024);
  // Place the BAT inside the dynamic header region.
  writeU64BE(dyn, 16, 1024n);
  writeU32BE(dyn, 36, vhdChecksum(dyn, 36));
  file.set(dyn, dynHeaderOffset);

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD BAT overlaps dynamic header/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects dynamic VHD with absurd BAT size", async () => {
  const footerSize = 512;
  const dynHeaderOffset = 512;
  const dynHeaderSize = 1024;
  const batOffset = 1536;

  const blockSize = 1024;
  const logicalSize = 1024;
  const maxTableEntries = 33_554_433; // (maxTableEntries * 4) > 128 MiB cap

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  writeU64BE(footer, 16, BigInt(dynHeaderOffset)); // data offset
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 3); // disk type dynamic
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const dyn = new Uint8Array(dynHeaderSize);
  dyn.set(new TextEncoder().encode("cxsparse"), 0);
  writeU64BE(dyn, 8, 0xffff_ffff_ffff_ffffn);
  writeU64BE(dyn, 16, BigInt(batOffset));
  writeU32BE(dyn, 24, 0x0001_0000);
  writeU32BE(dyn, 28, maxTableEntries);
  writeU32BE(dyn, 32, blockSize);
  writeU32BE(dyn, 36, vhdChecksum(dyn, 36));

  const fileSize = batOffset + footerSize;
  const file = new Uint8Array(fileSize);
  // Footer copy at offset 0.
  file.set(footer, 0);
  file.set(dyn, dynHeaderOffset);
  file.set(footer, fileSize - footerSize);

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD BAT too large/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects dynamic VHD with huge block_size", async () => {
  const footerSize = 512;
  const dynHeaderOffset = 512;
  const dynHeaderSize = 1024;
  const batOffset = 1536;

  const blockSize = 128 * 1024 * 1024; // > 64 MiB cap
  const logicalSize = blockSize;
  const maxTableEntries = 1;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  writeU64BE(footer, 16, BigInt(dynHeaderOffset)); // data offset
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 3); // disk type dynamic
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const dyn = new Uint8Array(dynHeaderSize);
  dyn.set(new TextEncoder().encode("cxsparse"), 0);
  writeU64BE(dyn, 8, 0xffff_ffff_ffff_ffffn);
  writeU64BE(dyn, 16, BigInt(batOffset));
  writeU32BE(dyn, 24, 0x0001_0000);
  writeU32BE(dyn, 28, maxTableEntries);
  writeU32BE(dyn, 32, blockSize);
  writeU32BE(dyn, 36, vhdChecksum(dyn, 36));

  const fileSize = 512 + 1024 + 512 + 512;
  const file = new Uint8Array(fileSize);
  file.set(footer, 0);
  file.set(dyn, dynHeaderOffset);
  file.fill(0xff, batOffset, batOffset + 512); // BAT: all unallocated
  file.set(footer, fileSize - footerSize);

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD block_size too large/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects fixed VHD with data_offset != u64::MAX", async () => {
  const footerSize = 512;
  const logicalSize = 512;

  const footer = new Uint8Array(footerSize);
  footer.set(new TextEncoder().encode("conectix"), 0);
  writeU32BE(footer, 8, 2); // features
  writeU32BE(footer, 12, 0x0001_0000); // file_format_version
  writeU64BE(footer, 16, 0xffff_ffff_ffff_fffen); // invalid for fixed disks
  writeU64BE(footer, 48, BigInt(logicalSize)); // current size
  writeU32BE(footer, 60, 2); // disk type fixed
  writeU32BE(footer, 64, vhdChecksum(footer, 64));

  const file = new Uint8Array(logicalSize + footerSize);
  file.set(footer, file.byteLength - footerSize);

  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /invalid VHD data_offset/i.test(err.message),
  );
});

test("convertToAeroSparse: rejects VHD with misaligned file length", async () => {
  const src = new MemSource(new Uint8Array(513));
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /VHD file length misaligned/i.test(err.message),
  );
});

test("convertToAeroSparse: fixed VHD footer copy at offset 0 is ignored", async () => {
  const { file, logical } = buildFixedVhdFixtureWithFooterCopy();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "vhd");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: non-identical fixed VHD footer copy at offset 0 is ignored", async () => {
  const { file, logical } = buildFixedVhdFixtureWithFooterCopyNonIdentical();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "vhd");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: fixed VHD without footer copy does not mis-detect sector 0 as a footer copy", async () => {
  const { file, logical } = buildFixedVhdFixtureWithoutFooterCopyButSector0LooksLikeFooter();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  const { manifest } = await convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 });
  assert.equal(manifest.originalFormat, "vhd");
  assert.equal(manifest.logicalSize, logical.byteLength);

  const parsed = parseAeroSparse(sync.toBytes());
  const roundtrip = readLogical(parsed, 0, logical.byteLength);
  assert.deepEqual(roundtrip, logical);
  assert.equal(manifest.checksum.value, sparseChecksumCrc32(parsed));
});

test("convertToAeroSparse: rejects fixed VHD with invalid data_offset", async () => {
  const { file } = buildFixedVhdFixtureInvalidFixedDataOffset();
  const src = new MemSource(file);
  const sync = new MemSyncAccessHandle();
  await assert.rejects(
    convertToAeroSparse(src, "vhd", sync, { blockSizeBytes: 512 }),
    (err: any) => err instanceof Error && /invalid VHD data_offset/i.test(err.message),
  );
});

test("convertToAeroSparse: supports cancellation via AbortSignal", async () => {
  const blockSize = 512;
  const logical = new Uint8Array(blockSize * 8);
  logical[blockSize * 4] = 0x5a;

  const src = new MemSource(logical);
  const sync = new MemSyncAccessHandle();
  const ac = new AbortController();

  await assert.rejects(
    convertToAeroSparse(src, "raw", sync, {
      blockSizeBytes: blockSize,
      signal: ac.signal,
      onProgress(p) {
        if (p.processedBytes >= blockSize) ac.abort();
      },
    }),
    (err: any) => err instanceof DOMException && err.name === "AbortError",
  );
});
