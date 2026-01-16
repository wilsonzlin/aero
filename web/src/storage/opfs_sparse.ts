import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { OPFS_DISKS_PATH, opfsGetDir } from "./metadata.ts";
import type { SparseBlockDisk } from "./sparse_block_disk";

type SyncAccessHandle = {
  read(buffer: ArrayBufferView, options?: { at: number }): number;
  write(buffer: ArrayBufferView, options?: { at: number }): number;
  flush(): void;
  close(): void;
  getSize(): number;
  truncate(size: number): void;
};

type FileHandle = {
  createSyncAccessHandle(): Promise<SyncAccessHandle>;
};

type DirectoryHandle = {
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<DirectoryHandle>;
  getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
};

const MAGIC = "AEROSPAR";
const VERSION = 1;
const HEADER_SIZE = 64;

// Keep sparse file decoding bounded: sparse images may be corrupted or untrusted
// (e.g. downloaded caches). These limits prevent huge allocations on open().
//
// Align with Rust storage snapshot bounds (MAX_OVERLAY_BLOCK_SIZE_BYTES).
const MAX_BLOCK_SIZE_BYTES = 64 * 1024 * 1024;
// Bound the in-memory table allocation in open() (Uint8Array + Float64Array).
const MAX_TABLE_BYTES = 64 * 1024 * 1024;

function alignUp(value: number, alignment: number): number {
  if (alignment <= 0) throw new Error("alignment must be > 0");
  const aligned = Math.ceil(value / alignment) * alignment;
  if (!Number.isSafeInteger(aligned)) {
    throw new Error("alignUp overflow");
  }
  return aligned;
}

function divCeil(n: number, d: number): number {
  if (!Number.isSafeInteger(n) || !Number.isSafeInteger(d) || d <= 0) {
    throw new Error("divCeil: arguments must be safe positive integers");
  }
  const out = Number((BigInt(n) + BigInt(d) - 1n) / BigInt(d));
  if (!Number.isSafeInteger(out)) {
    throw new Error("divCeil overflow");
  }
  return out;
}

function toSafeNumber(v: bigint, field: string): number {
  const n = Number(v);
  if (!Number.isSafeInteger(n)) {
    throw new Error(`field ${field} is not a safe JS integer (${v})`);
  }
  return n;
}

function isPowerOfTwo(n: number): boolean {
  return n > 0 && (BigInt(n) & (BigInt(n) - 1n)) === 0n;
}

function alignUpBigInt(value: bigint, alignment: bigint): bigint {
  if (alignment <= 0n) throw new Error("alignment must be > 0");
  return ((value + alignment - 1n) / alignment) * alignment;
}

async function getOpfsDir(dirPath: string): Promise<DirectoryHandle> {
  return (await opfsGetDir(dirPath, { create: true })) as unknown as DirectoryHandle;
}

type SparseHeader = {
  version: number;
  blockSizeBytes: number;
  diskSizeBytes: number;
  tableEntries: number;
  dataOffset: number;
  allocatedBlocks: number;
};

function encodeHeader(h: SparseHeader): Uint8Array {
  const buf = new ArrayBuffer(HEADER_SIZE);
  const bytes = new Uint8Array(buf);
  bytes.set(new TextEncoder().encode(MAGIC), 0);

  const view = new DataView(buf);
  view.setUint32(8, h.version, true);
  view.setUint32(12, HEADER_SIZE, true);
  view.setUint32(16, h.blockSizeBytes, true);
  view.setUint32(20, 0, true);
  view.setBigUint64(24, BigInt(h.diskSizeBytes), true);
  view.setBigUint64(32, BigInt(HEADER_SIZE), true); // table_offset
  view.setBigUint64(40, BigInt(h.tableEntries), true);
  view.setBigUint64(48, BigInt(h.dataOffset), true);
  view.setBigUint64(56, BigInt(h.allocatedBlocks), true);

  return bytes;
}

function decodeHeader(bytes: Uint8Array): SparseHeader {
  if (bytes.byteLength < HEADER_SIZE) {
    throw new Error("sparse header too small");
  }
  const magic = new TextDecoder().decode(bytes.slice(0, 8));
  if (magic !== MAGIC) {
    throw new Error(`bad sparse magic: ${magic}`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const version = view.getUint32(8, true);
  if (version !== VERSION) {
    throw new Error(`unsupported sparse version ${version}`);
  }
  const headerSize = view.getUint32(12, true);
  if (headerSize !== HEADER_SIZE) {
    throw new Error(`unexpected header size ${headerSize}`);
  }
  const blockSizeBytes = view.getUint32(16, true);
  const diskSizeBytes = toSafeNumber(view.getBigUint64(24, true), "diskSizeBytes");
  const tableOffset = toSafeNumber(view.getBigUint64(32, true), "tableOffset");
  if (tableOffset !== HEADER_SIZE) {
    throw new Error(`unsupported tableOffset ${tableOffset}`);
  }
  const tableEntries = toSafeNumber(view.getBigUint64(40, true), "tableEntries");
  const dataOffset = toSafeNumber(view.getBigUint64(48, true), "dataOffset");
  const allocatedBlocks = toSafeNumber(view.getBigUint64(56, true), "allocatedBlocks");

  // ---- Validation (bounds + consistency) ----
  if (blockSizeBytes <= 0) {
    throw new Error(`invalid blockSizeBytes=${blockSizeBytes}`);
  }
  if (blockSizeBytes % SECTOR_SIZE !== 0) {
    throw new Error("blockSizeBytes must be a multiple of 512");
  }
  if (!isPowerOfTwo(blockSizeBytes)) {
    throw new Error("blockSizeBytes must be a power of two");
  }
  if (blockSizeBytes > MAX_BLOCK_SIZE_BYTES) {
    throw new Error(`blockSizeBytes too large: ${blockSizeBytes} (max ${MAX_BLOCK_SIZE_BYTES})`);
  }

  if (diskSizeBytes <= 0) {
    throw new Error(`invalid diskSizeBytes=${diskSizeBytes}`);
  }
  if (diskSizeBytes % SECTOR_SIZE !== 0) {
    throw new Error("diskSizeBytes must be a multiple of 512");
  }

  if (tableEntries <= 0) {
    throw new Error(`invalid tableEntries=${tableEntries}`);
  }
  // Keep sparse file decoding bounded: reject pathological tables early based on
  // the header alone, before any file-size dependent validation.
  const tableBytesLenBig = BigInt(tableEntries) * 8n;
  if (tableBytesLenBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error("sparse table size overflow");
  }
  if (tableBytesLenBig > BigInt(MAX_TABLE_BYTES)) {
    throw new Error(`sparse table too large: ${tableBytesLenBig} bytes (max ${MAX_TABLE_BYTES})`);
  }
  const expectedEntries =
    (BigInt(diskSizeBytes) + BigInt(blockSizeBytes) - 1n) / BigInt(blockSizeBytes);
  if (BigInt(tableEntries) !== expectedEntries) {
    throw new Error(`tableEntries mismatch: expected=${expectedEntries} actual=${tableEntries}`);
  }

  const tableBytes = BigInt(tableEntries) * 8n;
  if (tableBytes > BigInt(MAX_TABLE_BYTES)) {
    throw new Error(`sparse table too large: ${tableBytes} bytes (max ${MAX_TABLE_BYTES})`);
  }

  const expectedDataOffset = alignUpBigInt(
    BigInt(HEADER_SIZE) + tableBytes,
    BigInt(blockSizeBytes),
  );
  if (BigInt(dataOffset) !== expectedDataOffset) {
    throw new Error(`dataOffset mismatch: expected=${expectedDataOffset} actual=${dataOffset}`);
  }

  if (allocatedBlocks > tableEntries) {
    throw new Error(`allocatedBlocks out of range: ${allocatedBlocks} (tableEntries=${tableEntries})`);
  }

  return {
    version,
    blockSizeBytes,
    diskSizeBytes,
    tableEntries,
    dataOffset,
    allocatedBlocks,
  };
}

type CacheEntry = { data: Uint8Array; dirty: boolean };

export class OpfsAeroSparseDisk implements SparseBlockDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readonly blockSizeBytes: number;

  private readonly sync: SyncAccessHandle;
  private readonly table: Float64Array;
  private header: SparseHeader;
  private readonly cache = new Map<number, CacheEntry>();
  private readonly maxCachedBlocks: number;

  private constructor(
    sync: SyncAccessHandle,
    header: SparseHeader,
    table: Float64Array,
    maxCachedBlocks: number,
  ) {
    this.sync = sync;
    this.maxCachedBlocks = maxCachedBlocks;
    this.header = header;
    this.table = table;
    this.capacityBytes = header.diskSizeBytes;
    this.blockSizeBytes = header.blockSizeBytes;
  }

  static async create(
    name: string,
    opts: {
      diskSizeBytes: number;
      blockSizeBytes: number;
      maxCachedBlocks?: number;
      dir?: DirectoryHandle;
      dirPath?: string;
    },
  ): Promise<OpfsAeroSparseDisk> {
    if (!Number.isSafeInteger(opts.diskSizeBytes) || opts.diskSizeBytes <= 0) {
      throw new Error(`invalid diskSizeBytes=${opts.diskSizeBytes}`);
    }
    if (opts.diskSizeBytes % SECTOR_SIZE !== 0) {
      throw new Error("diskSizeBytes must be a multiple of 512");
    }
    if (!Number.isSafeInteger(opts.blockSizeBytes) || opts.blockSizeBytes <= 0) {
      throw new Error(`invalid blockSizeBytes=${opts.blockSizeBytes}`);
    }
    if (opts.blockSizeBytes % SECTOR_SIZE !== 0) {
      throw new Error("blockSizeBytes must be a multiple of 512");
    }
    if ((BigInt(opts.blockSizeBytes) & (BigInt(opts.blockSizeBytes) - 1n)) !== 0n) {
      throw new Error("blockSizeBytes must be a power of two");
    }
    if (opts.blockSizeBytes > MAX_BLOCK_SIZE_BYTES) {
      throw new Error(`blockSizeBytes too large: ${opts.blockSizeBytes} (max ${MAX_BLOCK_SIZE_BYTES})`);
    }

    const tableEntries = divCeil(opts.diskSizeBytes, opts.blockSizeBytes);
    const tableBytes = tableEntries * 8;
    if (!Number.isSafeInteger(tableBytes) || tableBytes < 0 || tableBytes > MAX_TABLE_BYTES) {
      throw new Error(`sparse table too large: ${tableBytes} bytes (max ${MAX_TABLE_BYTES})`);
    }
    const dataOffset = alignUp(HEADER_SIZE + tableBytes, opts.blockSizeBytes);

    const dir = opts.dir ?? (await getOpfsDir(opts.dirPath ?? OPFS_DISKS_PATH));
    const file = await dir.getFileHandle(name, { create: true });
    const sync = await file.createSyncAccessHandle();

    const header: SparseHeader = {
      version: VERSION,
      blockSizeBytes: opts.blockSizeBytes,
      diskSizeBytes: opts.diskSizeBytes,
      tableEntries,
      dataOffset,
      allocatedBlocks: 0,
    };

    // Ensure header + table exist in the file (filled with zeros).
    sync.truncate(dataOffset);
    sync.write(encodeHeader(header), { at: 0 });
    // Zero the on-disk table region in chunks to reduce peak memory usage for large but still-valid
    // sparse images (e.g. multi-GB disks with small block sizes).
    const zeroChunk = new Uint8Array(Math.min(64 * 1024, tableBytes));
    let remaining = tableBytes;
    let off = HEADER_SIZE;
    while (remaining > 0) {
      const len = Math.min(remaining, zeroChunk.byteLength);
      const written = sync.write(zeroChunk.subarray(0, len), { at: off });
      if (written !== len) {
        throw new Error(`short table write at=${off}: expected=${len} actual=${written}`);
      }
      off += len;
      remaining -= len;
    }

    return new OpfsAeroSparseDisk(
      sync,
      header,
      new Float64Array(tableEntries),
      opts.maxCachedBlocks ?? 64,
    );
  }

  static async open(
    name: string,
    opts: { maxCachedBlocks?: number; dir?: DirectoryHandle; dirPath?: string } = {},
  ): Promise<OpfsAeroSparseDisk> {
    const dir = opts.dir ?? (await getOpfsDir(opts.dirPath ?? OPFS_DISKS_PATH));
    const file = await dir.getFileHandle(name, { create: false });
    const sync = await file.createSyncAccessHandle();

    try {
      const headerBytes = new Uint8Array(HEADER_SIZE);
      const n = sync.read(headerBytes, { at: 0 });
      if (n !== HEADER_SIZE) {
        throw new Error(`short header read: expected=${HEADER_SIZE} actual=${n}`);
      }
      const header = decodeHeader(headerBytes);
      // Validate sparse allocation table size before any file-size derived checks so corrupted
      // headers fail deterministically and can't mask an overlarge table behind a "truncated
      // file" style error (e.g. "data region out of bounds").
      const tableBytesLenBig = BigInt(header.tableEntries) * 8n;
      if (tableBytesLenBig > BigInt(Number.MAX_SAFE_INTEGER)) {
        throw new Error("sparse table size overflow");
      }
      if (tableBytesLenBig > BigInt(MAX_TABLE_BYTES)) {
        throw new Error(`sparse table too large: ${tableBytesLenBig} bytes (max ${MAX_TABLE_BYTES})`);
      }

      const fileSize = sync.getSize();
      if (!Number.isSafeInteger(fileSize) || fileSize < 0) {
        throw new Error(`invalid file size: ${fileSize}`);
      }

      if (fileSize < header.dataOffset) {
        throw new Error("data region out of bounds");
      }
      const expectedMinLenBig =
        BigInt(header.dataOffset) + BigInt(header.allocatedBlocks) * BigInt(header.blockSizeBytes);
      const expectedMinLen = toSafeNumber(expectedMinLenBig, "expectedMinLen");
      if (fileSize < expectedMinLen) {
        throw new Error("allocated blocks extend beyond end of image");
      }
      const table = new Float64Array(header.tableEntries);
      let actualAllocatedBlocks = 0;
      // Track which physical blocks (0..allocatedBlocks) are referenced by the table to detect
      // duplicates and validate `allocatedBlocks` against the table contents.
      const bitsetLen = Math.ceil(header.allocatedBlocks / 32);
      const seenPhysIdx = new Uint32Array(bitsetLen);

      // Read the on-disk table in chunks to avoid allocating a second full-size table buffer in
      // memory (worst case: 64MiB Uint8Array + 64MiB Float64Array).
      const chunkEntries = 8192; // 64KiB
      const buf = new Uint8Array(chunkEntries * 8);
      const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
      for (let i = 0; i < header.tableEntries; i += chunkEntries) {
        const count = Math.min(chunkEntries, header.tableEntries - i);
        const bytes = count * 8;
        const n = sync.read(buf.subarray(0, bytes), { at: HEADER_SIZE + i * 8 });
        if (n !== bytes) {
          throw new Error(`short table read: expected=${bytes} actual=${n}`);
        }
        for (let j = 0; j < count; j++) {
          const phys = toSafeNumber(view.getBigUint64(j * 8, true), `table[${i + j}]`);
          table[i + j] = phys;
          if (phys === 0) continue;

          actualAllocatedBlocks++;
          if (actualAllocatedBlocks > header.allocatedBlocks) {
            throw new Error("allocatedBlocks does not match allocation table");
          }
          if (phys < header.dataOffset) {
            throw new Error("data block offset before data region");
          }
          const rel = phys - header.dataOffset;
          if (rel % header.blockSizeBytes !== 0) {
            throw new Error("misaligned data block offset");
          }
          const physIdx = rel / header.blockSizeBytes;
          if (!Number.isSafeInteger(physIdx) || physIdx < 0 || physIdx >= header.allocatedBlocks) {
            throw new Error("data block offset out of bounds");
          }
          const physEnd = phys + header.blockSizeBytes;
          if (!Number.isSafeInteger(physEnd) || physEnd > expectedMinLen) {
            throw new Error("data block offset out of bounds");
          }

          const wordIdx = Math.floor(physIdx / 32);
          const bitIdx = physIdx % 32;
          const mask = 1 << bitIdx;
          if ((seenPhysIdx[wordIdx]! & mask) !== 0) {
            throw new Error("duplicate data block offset");
          }
          seenPhysIdx[wordIdx] |= mask;
        }
      }
      if (actualAllocatedBlocks !== header.allocatedBlocks) {
        throw new Error("allocatedBlocks does not match allocation table");
      }

      return new OpfsAeroSparseDisk(sync, header, table, opts.maxCachedBlocks ?? 64);
    } catch (err) {
      try {
        sync.close();
      } catch {
        // ignore best-effort close failures
      }
      throw err;
    }
  }

  isBlockAllocated(blockIndex: number): boolean {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    // A block can be "logically present" even if it hasn't been flushed to the sparse
    // file yet (e.g. newly-written overlay blocks, or freshly downloaded remote-cache
    // blocks). Those blocks live only in the in-memory cache until `flush()` (or an
    // eviction) writes them out and assigns a physical offset in `table`.
    //
    // Callers like `OpfsCowDisk` need to treat dirty cached blocks as allocated so
    // reads observe writes immediately without requiring an explicit flush.
    return this.table[blockIndex] !== 0 || this.cache.get(blockIndex)?.dirty === true;
  }

  getAllocatedBytes(): number {
    // Persisted blocks are tracked in the header/table. Additionally, a block can be
    // "logically allocated" but not yet flushed to disk (dirty cache entry with a
    // zero table slot).
    let pending = 0;
    for (const [blockIndex, entry] of this.cache.entries()) {
      if (!entry.dirty) continue;
      if (this.table[blockIndex] !== 0) continue;
      pending += 1;
    }
    return toSafeNumber(BigInt(this.header.allocatedBlocks + pending) * BigInt(this.blockSizeBytes), "allocatedBytes");
  }

  private touchCacheKey(key: number, entry: CacheEntry): void {
    // Map maintains insertion order; delete+set moves key to the end (MRU).
    this.cache.delete(key);
    this.cache.set(key, entry);
  }

  private evictIfNeeded(): void {
    while (this.cache.size > this.maxCachedBlocks) {
      const lruKey = this.cache.keys().next().value as number | undefined;
      if (lruKey === undefined) return;
      const entry = this.cache.get(lruKey)!;
      this.cache.delete(lruKey);
      if (entry.dirty) {
        this.writeBlockNow(lruKey, entry.data);
      }
    }
  }

  private persistHeader(): void {
    this.sync.write(encodeHeader(this.header), { at: 0 });
  }

  private persistTableEntry(blockIndex: number, phys: number): void {
    const buf = new ArrayBuffer(8);
    new DataView(buf).setBigUint64(0, BigInt(phys), true);
    this.sync.write(new Uint8Array(buf), { at: HEADER_SIZE + blockIndex * 8 });
  }

  private ensureAllocated(blockIndex: number): number {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    const current = this.table[blockIndex];
    if (current !== 0) return current;

    const phys = this.header.dataOffset + this.header.allocatedBlocks * this.header.blockSizeBytes;
    this.header.allocatedBlocks += 1;
    this.table[blockIndex] = phys;

    this.persistHeader();
    this.persistTableEntry(blockIndex, phys);

    // Ensure file covers the new block.
    const end = phys + this.header.blockSizeBytes;
    if (end > this.sync.getSize()) {
      this.sync.truncate(end);
    }

    return phys;
  }

  private readBlockInto(blockIndex: number, dst: Uint8Array): void {
    dst.fill(0);
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    const phys = this.table[blockIndex];
    if (phys === 0) return;
    const n = this.sync.read(dst, { at: phys });
    if (n !== dst.byteLength) {
      throw new Error(`short block read at=${phys}: expected=${dst.byteLength} actual=${n}`);
    }
  }

  private writeBlockNow(blockIndex: number, src: Uint8Array): void {
    const phys = this.ensureAllocated(blockIndex);
    const n = this.sync.write(src, { at: phys });
    if (n !== src.byteLength) {
      throw new Error(`short block write at=${phys}: expected=${src.byteLength} actual=${n}`);
    }
  }

  private getCachedBlock(blockIndex: number): CacheEntry {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    const hit = this.cache.get(blockIndex);
    if (hit) {
      this.touchCacheKey(blockIndex, hit);
      return hit;
    }

    const entry: CacheEntry = {
      data: new Uint8Array(this.blockSizeBytes),
      dirty: false,
    };
    this.readBlockInto(blockIndex, entry.data);
    this.cache.set(blockIndex, entry);
    this.evictIfNeeded();
    return entry;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, buffer.byteLength - pos);

      const entry = this.getCachedBlock(blockIndex);
      buffer.set(entry.data.subarray(within, within + chunkLen), pos);
      pos += chunkLen;
    }
  }

  async writeSectors(lba: number, data: Uint8Array): Promise<void> {
    assertSectorAligned(data.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, data.byteLength, this.sectorSize);
    if (offset + data.byteLength > this.capacityBytes) {
      throw new Error("write past end of disk");
    }

    let pos = 0;
    while (pos < data.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, data.byteLength - pos);

      if (within === 0 && chunkLen === this.blockSizeBytes) {
        // Whole-block overwrite: avoid reading the old block.
        const entry: CacheEntry = { data: new Uint8Array(this.blockSizeBytes), dirty: true };
        entry.data.set(data.subarray(pos, pos + chunkLen));
        this.cache.delete(blockIndex);
        this.cache.set(blockIndex, entry);
        this.evictIfNeeded();
      } else {
        const entry = this.getCachedBlock(blockIndex);
        entry.data.set(data.subarray(pos, pos + chunkLen), within);
        entry.dirty = true;
        this.touchCacheKey(blockIndex, entry);
      }

      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    for (const [blockIndex, entry] of this.cache) {
      if (!entry.dirty) continue;
      this.writeBlockNow(blockIndex, entry.data);
      entry.dirty = false;
    }
    this.sync.flush();
  }

  async close(): Promise<void> {
    try {
      await this.flush();
    } finally {
      try {
        this.sync.close();
      } catch {
        // Best-effort close: prefer releasing the underlying SyncAccessHandle even if flush failed.
      }
    }
  }

  /**
   * Convenience for overlay users: write an entire block.
   *
   * `data.byteLength` must equal `blockSizeBytes`.
   */
  async writeBlock(blockIndex: number, data: Uint8Array): Promise<void> {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    if (data.byteLength !== this.blockSizeBytes) {
      throw new Error("writeBlock: incorrect block size");
    }
    const entry: CacheEntry = { data: data.slice(), dirty: true };
    this.cache.delete(blockIndex);
    this.cache.set(blockIndex, entry);
    this.evictIfNeeded();
  }

  async readBlock(blockIndex: number, dst: Uint8Array): Promise<void> {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.table.length) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    if (dst.byteLength !== this.blockSizeBytes) {
      throw new Error("readBlock: incorrect block size");
    }
    const entry = this.getCachedBlock(blockIndex);
    dst.set(entry.data);
  }
}
