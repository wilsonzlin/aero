import { describe, expect, it } from "vitest";

import { OpfsAeroSparseDisk } from "./opfs_sparse";

// Keep these in sync with the implementation under test.
const HEADER_SIZE = 64;
const MAX_TABLE_BYTES = 64 * 1024 * 1024;

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

class MemoryFile {
  data = new Uint8Array();
}

class MemorySyncAccessHandle implements SyncAccessHandle {
  private closed = false;
  readCalls = 0;

  private readonly file: MemoryFile;

  constructor(file: MemoryFile) {
    this.file = file;
  }

  get isClosed(): boolean {
    return this.closed;
  }

  read(buffer: ArrayBufferView, options?: { at: number }): number {
    if (this.closed) throw new Error("SyncAccessHandle closed");
    this.readCalls += 1;
    const at = options?.at ?? 0;
    if (!Number.isSafeInteger(at) || at < 0) throw new Error("invalid read offset");
    const dst = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    if (at >= this.file.data.byteLength) return 0;
    const src = this.file.data.subarray(at, at + dst.byteLength);
    dst.set(src);
    return src.byteLength;
  }

  write(buffer: ArrayBufferView, options?: { at: number }): number {
    if (this.closed) throw new Error("SyncAccessHandle closed");
    const at = options?.at ?? 0;
    if (!Number.isSafeInteger(at) || at < 0) throw new Error("invalid write offset");
    const src = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    const end = at + src.byteLength;
    if (!Number.isSafeInteger(end) || end < 0) throw new Error("write overflow");
    if (end > this.file.data.byteLength) {
      const next = new Uint8Array(end);
      next.set(this.file.data);
      this.file.data = next;
    }
    this.file.data.set(src, at);
    return src.byteLength;
  }

  flush(): void {
    if (this.closed) throw new Error("SyncAccessHandle closed");
  }

  close(): void {
    this.closed = true;
  }

  getSize(): number {
    if (this.closed) throw new Error("SyncAccessHandle closed");
    return this.file.data.byteLength;
  }

  truncate(size: number): void {
    if (this.closed) throw new Error("SyncAccessHandle closed");
    if (!Number.isSafeInteger(size) || size < 0) throw new Error("invalid truncate size");
    if (size === this.file.data.byteLength) return;
    if (size < this.file.data.byteLength) {
      this.file.data = this.file.data.subarray(0, size).slice();
      return;
    }
    const next = new Uint8Array(size);
    next.set(this.file.data);
    this.file.data = next;
  }
}

class MemoryFileHandle implements FileHandle {
  readonly file = new MemoryFile();
  readonly handles: MemorySyncAccessHandle[] = [];

  async createSyncAccessHandle(): Promise<SyncAccessHandle> {
    const h = new MemorySyncAccessHandle(this.file);
    this.handles.push(h);
    return h;
  }
}

class MemoryDirectoryHandle implements DirectoryHandle {
  private readonly dirs = new Map<string, MemoryDirectoryHandle>();
  private readonly files = new Map<string, MemoryFileHandle>();

  async getDirectoryHandle(name: string, options: { create?: boolean } = {}): Promise<DirectoryHandle> {
    const existing = this.dirs.get(name);
    if (existing) return existing;
    if (!options.create) throw new Error("NotFound");
    const dir = new MemoryDirectoryHandle();
    this.dirs.set(name, dir);
    return dir;
  }

  async getFileHandle(name: string, options: { create?: boolean } = {}): Promise<FileHandle> {
    const existing = this.files.get(name);
    if (existing) return existing;
    if (!options.create) throw new Error("NotFound");
    const file = new MemoryFileHandle();
    this.files.set(name, file);
    return file;
  }

  // Test-only helper
  getFileHandleSync(name: string): MemoryFileHandle | null {
    return this.files.get(name) ?? null;
  }
}

function alignUpBigInt(value: bigint, alignment: bigint): bigint {
  if (alignment <= 0n) throw new Error("alignment must be > 0");
  return ((value + alignment - 1n) / alignment) * alignment;
}

function patchHeader(dir: MemoryDirectoryHandle, fileName: string, fn: (view: DataView) => void): void {
  const fh = dir.getFileHandleSync(fileName);
  if (!fh) throw new Error("missing file handle");
  if (fh.file.data.byteLength < HEADER_SIZE) throw new Error("file too small");
  const view = new DataView(fh.file.data.buffer, fh.file.data.byteOffset, fh.file.data.byteLength);
  fn(view);
}

function patchTableEntry(dir: MemoryDirectoryHandle, fileName: string, entryIndex: number, phys: bigint): void {
  const fh = dir.getFileHandleSync(fileName);
  if (!fh) throw new Error("missing file handle");
  const off = HEADER_SIZE + entryIndex * 8;
  if (off + 8 > fh.file.data.byteLength) throw new Error("table entry out of range");
  const view = new DataView(fh.file.data.buffer, fh.file.data.byteOffset, fh.file.data.byteLength);
  view.setBigUint64(off, phys, true);
}

function resizeFile(dir: MemoryDirectoryHandle, fileName: string, size: number): void {
  const fh = dir.getFileHandleSync(fileName);
  if (!fh) throw new Error("missing file handle");
  if (!Number.isSafeInteger(size) || size < 0) throw new Error("invalid size");
  if (size === fh.file.data.byteLength) return;
  if (size < fh.file.data.byteLength) {
    fh.file.data = fh.file.data.subarray(0, size).slice();
    return;
  }
  const next = new Uint8Array(size);
  next.set(fh.file.data);
  fh.file.data = next;
}

describe("OpfsAeroSparseDisk.open header validation", () => {
  it("roundtrips create() then open()", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "roundtrip.aerospar";
    const diskSizeBytes = 1024 * 1024;
    const blockSizeBytes = 4096;

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes, blockSizeBytes, dir });
    await created.close();

    const opened = await OpfsAeroSparseDisk.open(name, { dir });
    expect(opened.capacityBytes).toBe(diskSizeBytes);
    expect(opened.blockSizeBytes).toBe(blockSizeBytes);
    await opened.close();
  });

  it("rejects headers with tableEntries that exceed MAX_TABLE_BYTES", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "huge-table.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    // Create a self-consistent header with a pathological table size.
    const blockSizeBytes = 512;
    const tableEntries = 1_000_000_000_000n;
    const tableBytes = tableEntries * 8n;
    expect(tableBytes).toBeGreaterThan(BigInt(MAX_TABLE_BYTES));
    const diskSizeBytes = tableEntries * BigInt(blockSizeBytes);
    const dataOffset = alignUpBigInt(BigInt(HEADER_SIZE) + tableBytes, BigInt(blockSizeBytes));

    patchHeader(dir, name, (view) => {
      view.setUint32(16, blockSizeBytes, true);
      view.setBigUint64(24, diskSizeBytes, true);
      view.setBigUint64(40, tableEntries, true);
      view.setBigUint64(48, dataOffset, true);
      view.setBigUint64(56, 0n, true); // allocatedBlocks
    });

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/sparse table too large/i);

    const fh = dir.getFileHandleSync(name)!;
    const openHandle = fh.handles.at(-1)!;
    expect(openHandle.isClosed).toBe(true);
    expect(openHandle.readCalls).toBe(1); // header only; must fail before table read
  });

  it("rejects headers with invalid blockSizeBytes", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "bad-block-size.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    patchHeader(dir, name, (view) => {
      view.setUint32(16, 513, true); // not a multiple of 512
    });

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/blockSizeBytes must be a multiple of 512/i);

    const fh = dir.getFileHandleSync(name)!;
    const openHandle = fh.handles.at(-1)!;
    expect(openHandle.isClosed).toBe(true);
    expect(openHandle.readCalls).toBe(1);
  });

  it("rejects headers with inconsistent dataOffset", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "bad-data-offset.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    patchHeader(dir, name, (view) => {
      const current = view.getBigUint64(48, true);
      view.setBigUint64(48, current + 512n, true);
    });

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/dataOffset mismatch/i);

    const fh = dir.getFileHandleSync(name)!;
    const openHandle = fh.handles.at(-1)!;
    expect(openHandle.isClosed).toBe(true);
    expect(openHandle.readCalls).toBe(1);
  });
});

describe("OpfsAeroSparseDisk.open allocation table validation", () => {
  it("rejects when allocatedBlocks does not match allocation table", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "alloc-count-mismatch.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    // Claim a single allocated block without setting any table entries.
    patchHeader(dir, name, (view) => {
      view.setBigUint64(56, 1n, true);
    });
    // Ensure file is long enough for the claimed block so we hit the allocation table validation.
    resizeFile(dir, name, 8192);

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/allocatedBlocks does not match allocation table/i);
  });

  it("rejects data block offsets before the data region", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "alloc-before-data.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    patchHeader(dir, name, (view) => {
      view.setBigUint64(56, 1n, true); // allocatedBlocks
    });
    // Point the first table entry at the header region.
    patchTableEntry(dir, name, 0, 64n);
    resizeFile(dir, name, 8192);

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/data block offset before data region/i);
  });

  it("rejects misaligned data block offsets", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "alloc-misaligned.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    patchHeader(dir, name, (view) => {
      view.setBigUint64(56, 1n, true); // allocatedBlocks
    });
    // Misaligned offset: dataOffset=4096 for this fixture, so 4097 is not block-aligned.
    patchTableEntry(dir, name, 0, 4097n);
    resizeFile(dir, name, 8192);

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/misaligned data block offset/i);
  });

  it("rejects duplicate data block offsets", async () => {
    const dir = new MemoryDirectoryHandle();
    const name = "alloc-duplicate.aerospar";

    const created = await OpfsAeroSparseDisk.create(name, { diskSizeBytes: 1024 * 1024, blockSizeBytes: 4096, dir });
    await created.close();

    patchHeader(dir, name, (view) => {
      view.setBigUint64(56, 2n, true); // allocatedBlocks
    });
    // Two table entries referencing the same physical block index 0.
    patchTableEntry(dir, name, 0, 4096n);
    patchTableEntry(dir, name, 1, 4096n);
    resizeFile(dir, name, 4096 + 2 * 4096);

    await expect(OpfsAeroSparseDisk.open(name, { dir })).rejects.toThrow(/duplicate data block offset/i);
  });
});
