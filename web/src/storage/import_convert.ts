import { crc32Final, crc32Init, crc32ToHex, crc32Update } from "./crc32.ts";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes.ts";

export type ImageFormat = "raw" | "qcow2" | "vhd" | "iso";
export type ConvertedFormat = "aerospar" | "iso";

export interface ImportProgress {
  processedBytes: number;
  totalBytes: number;
}

export interface ImportManifest {
  manifestVersion: 1;
  originalFormat: ImageFormat;
  convertedFormat: ConvertedFormat;
  logicalSize: number;
  convertedSize: number;
  checksum: { algorithm: "crc32"; value: string };
  blockSizeBytes: number | null;
  readOnly: boolean;
}

export type ImportSource =
  | { kind: "file"; file: File }
  | { kind: "url"; url: string; filename?: string; size?: number };

export interface ImportConvertOptions {
  /**
   * Allocation unit for the output Aero sparse file.
   *
   * Must be a power of two and a multiple of 512.
   */
  blockSizeBytes?: number;
  signal?: AbortSignal;
  onProgress?: (p: ImportProgress) => void;
}

export type SyncAccessHandleLike = {
  read(buffer: ArrayBufferView, options?: { at: number }): number;
  write(buffer: ArrayBufferView, options?: { at: number }): number;
  flush(): void;
  close(): void;
  getSize(): number;
  truncate(size: number): void;
};

interface RandomAccessSource {
  readonly size: number;
  readAt(offset: number, length: number): Promise<Uint8Array<ArrayBuffer>>;
}

class FileSource implements RandomAccessSource {
  private readonly file: File;
  constructor(file: File) {
    this.file = file;
  }
  get size(): number {
    return this.file.size;
  }
  async readAt(offset: number, length: number): Promise<Uint8Array<ArrayBuffer>> {
    const end = offset + length;
    if (offset < 0 || length < 0 || end > this.file.size) {
      throw new RangeError(`readAt out of range: ${offset}+${length} (size=${this.file.size})`);
    }
    const ab = await this.file.slice(offset, end).arrayBuffer();
    if (ab.byteLength !== length) {
      throw new Error(`short read: expected ${length}, got ${ab.byteLength}`);
    }
    return new Uint8Array(ab);
  }
}

class UrlSource implements RandomAccessSource {
  public readonly url: string;
  public readonly size: number;
  private readonly signal: AbortSignal | undefined;
  constructor(url: string, size: number, signal: AbortSignal | undefined) {
    this.url = url;
    this.size = size;
    this.signal = signal;
  }
  async readAt(offset: number, length: number): Promise<Uint8Array<ArrayBuffer>> {
    const end = offset + length;
    if (offset < 0 || length < 0 || end > this.size) {
      throw new RangeError(`readAt out of range: ${offset}+${length} (size=${this.size})`);
    }
    const res = await fetch(this.url, {
      headers: {
        Range: `bytes=${offset}-${end - 1}`,
      },
      signal: this.signal,
    });
    if (!res.ok) throw new Error(`range fetch failed (${res.status})`);
    const ab = await res.arrayBuffer();
    if (ab.byteLength !== length) {
      throw new Error(`short range read: expected ${length}, got ${ab.byteLength}`);
    }
    return new Uint8Array(ab);
  }
}

export async function importConvertToOpfs(
  source: ImportSource,
  destDir: FileSystemDirectoryHandle,
  baseName: string,
  options: ImportConvertOptions = {},
): Promise<ImportManifest> {
  const { src, filename } = await openSource(source, options.signal);
  const format = await detectFormat(src, filename);

  if (format === "iso") {
    const outName = `${baseName}.iso`;
    const fileHandle = await destDir.getFileHandle(outName, { create: true });
    const writable = await fileHandle.createWritable({ keepExistingData: false });
    const { crc32 } = await copySequentialCrc32(src, writable, options.signal, options.onProgress);
    await writable.close();

    const manifest: ImportManifest = {
      manifestVersion: 1,
      originalFormat: "iso",
      convertedFormat: "iso",
      logicalSize: src.size,
      convertedSize: src.size,
      checksum: { algorithm: "crc32", value: crc32ToHex(crc32Final(crc32)) },
      blockSizeBytes: null,
      readOnly: true,
    };
    await writeManifest(destDir, baseName, manifest);
    return manifest;
  }

  // For all HDD-ish formats we convert to Aero sparse.
  const outName = `${baseName}.aerospar`;
  const fileHandle = await destDir.getFileHandle(outName, { create: true });
  const sync = await createSyncAccessHandle(fileHandle);
  try {
    const { manifest } = await convertToAeroSparse(src, format, sync, options);
    await writeManifest(destDir, baseName, manifest);
    return manifest;
  } finally {
    try {
      sync.flush();
    } finally {
      sync.close();
    }
  }
}

export async function detectFormat(src: RandomAccessSource, filename?: string): Promise<ImageFormat> {
  const ext = filename?.split(".").pop()?.toLowerCase();

  // qcow2: "QFI\xfb" at offset 0
  if (src.size >= 4) {
    const sig = await src.readAt(0, 4);
    if (sig[0] === 0x51 && sig[1] === 0x46 && sig[2] === 0x49 && sig[3] === 0xfb) return "qcow2";
  }

  // VHD: "conectix" at start (dynamic) and at end (all VHDs).
  if (src.size >= 8) {
    const cookie0 = await src.readAt(0, 8);
    if (ascii(cookie0) === "conectix") return "vhd";
  }
  if (src.size >= 512) {
    const cookieEnd = await src.readAt(src.size - 512, 8);
    if (ascii(cookieEnd) === "conectix") return "vhd";
  }

  // ISO9660: "CD001" at 0x8001.
  if (src.size >= 0x8001 + 5) {
    const cd001 = await src.readAt(0x8001, 5);
    if (ascii(cd001) === "CD001") return "iso";
  }

  if (ext === "qcow2") return "qcow2";
  if (ext === "vhd") return "vhd";
  if (ext === "iso") return "iso";
  if (ext === "img" || ext === "raw") return "raw";

  return "raw";
}

async function openSource(
  source: ImportSource,
  signal: AbortSignal | undefined,
): Promise<{ src: RandomAccessSource; filename?: string }> {
  if (source.kind === "file") return { src: new FileSource(source.file), filename: source.file.name };
  const filename = source.filename ?? new URL(source.url).pathname.split("/").pop() ?? "image";
  const size = source.size ?? (await fetchSize(source.url, signal));
  return { src: new UrlSource(source.url, size, signal), filename };
}

async function fetchSize(url: string, signal: AbortSignal | undefined): Promise<number> {
  // HEAD is preferred but not always supported.
  const head = await fetch(url, { method: "HEAD", signal });
  const len = head.headers.get("content-length");
  if (head.ok && len) {
    const parsed = Number(len);
    if (Number.isSafeInteger(parsed) && parsed >= 0) return parsed;
  }

  // Fallback: Range GET and parse Content-Range.
  const res = await fetch(url, { headers: { Range: "bytes=0-0" }, signal });
  const cr = res.headers.get("content-range");
  if (!res.ok || !cr) throw new Error("unable to determine remote size (missing Content-Range)");
  const match = cr.match(/\/(\d+)$/);
  if (!match) throw new Error(`unexpected Content-Range: ${cr}`);
  const size = Number(match[1]);
  if (!Number.isSafeInteger(size) || size < 0) throw new Error(`invalid size from Content-Range: ${cr}`);
  return size;
}

async function writeManifest(
  dir: FileSystemDirectoryHandle,
  baseName: string,
  manifest: ImportManifest,
): Promise<void> {
  const fh = await dir.getFileHandle(`${baseName}.manifest.json`, { create: true });
  const w = await fh.createWritable({ keepExistingData: false });
  await w.write(JSON.stringify(manifest, null, 2));
  await w.close();
}

async function createSyncAccessHandle(fileHandle: FileSystemFileHandle): Promise<SyncAccessHandleLike> {
  const fn = (fileHandle as FileSystemFileHandle & { createSyncAccessHandle?: unknown })
    .createSyncAccessHandle as ((this: FileSystemFileHandle) => Promise<SyncAccessHandleLike>) | undefined;
  if (!fn) {
    throw new Error("OPFS sync access handles are not supported in this browser/context");
  }
  return await fn.call(fileHandle);
}

async function copySequentialCrc32(
  src: RandomAccessSource,
  writable: FileSystemWritableFileStream,
  signal: AbortSignal | undefined,
  onProgress: ((p: ImportProgress) => void) | undefined,
): Promise<{ crc32: number }> {
  let crc = crc32Init();
  const chunkSize = 8 * 1024 * 1024;
  let offset = 0;
  while (offset < src.size) {
    if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
    const len = Math.min(chunkSize, src.size - offset);
    const chunk = await src.readAt(offset, len);
    await writable.write(chunk);
    crc = crc32Update(crc, chunk);
    offset += len;
    onProgress?.({ processedBytes: offset, totalBytes: src.size });
  }
  return { crc32: crc };
}

export async function convertToAeroSparse(
  src: RandomAccessSource,
  originalFormat: Exclude<ImageFormat, "iso">,
  sync: SyncAccessHandleLike,
  options: ImportConvertOptions,
): Promise<{ manifest: ImportManifest }> {
  const blockSize = options.blockSizeBytes ?? RANGE_STREAM_CHUNK_SIZE;
  assertBlockSize(blockSize);

  switch (originalFormat) {
    case "raw":
      return await convertRawToSparse(src, sync, blockSize, options);
    case "qcow2":
      return await convertQcow2ToSparse(src, sync, blockSize, options);
    case "vhd":
      return await convertVhdToSparse(src, sync, blockSize, options);
  }
}

function assertBlockSize(blockSize: number): void {
  if (!Number.isSafeInteger(blockSize) || blockSize <= 0) throw new Error(`invalid blockSizeBytes=${blockSize}`);
  if (blockSize % 512 !== 0) throw new Error("blockSizeBytes must be a multiple of 512");
  if ((BigInt(blockSize) & (BigInt(blockSize) - 1n)) !== 0n) throw new Error("blockSizeBytes must be a power of two");
}

const AEROSPAR_MAGIC = "AEROSPAR";
const AEROSPAR_VERSION = 1;
const AEROSPAR_HEADER_SIZE = 64;

type AeroSparseHeader = {
  version: number;
  blockSizeBytes: number;
  diskSizeBytes: number;
  tableEntries: number;
  dataOffset: number;
  allocatedBlocks: number;
};

function encodeAeroSparseHeader(h: AeroSparseHeader): Uint8Array {
  const buf = new ArrayBuffer(AEROSPAR_HEADER_SIZE);
  const bytes = new Uint8Array(buf);
  bytes.set(new TextEncoder().encode(AEROSPAR_MAGIC), 0);
  const view = new DataView(buf);
  view.setUint32(8, h.version, true);
  view.setUint32(12, AEROSPAR_HEADER_SIZE, true);
  view.setUint32(16, h.blockSizeBytes, true);
  view.setUint32(20, 0, true);
  view.setBigUint64(24, BigInt(h.diskSizeBytes), true);
  view.setBigUint64(32, BigInt(AEROSPAR_HEADER_SIZE), true); // table_offset
  view.setBigUint64(40, BigInt(h.tableEntries), true);
  view.setBigUint64(48, BigInt(h.dataOffset), true);
  view.setBigUint64(56, BigInt(h.allocatedBlocks), true);
  return bytes;
}

function alignUp(value: number, alignment: number): number {
  const aligned = Math.ceil(value / alignment) * alignment;
  if (!Number.isSafeInteger(aligned)) throw new Error("alignUp overflow");
  return aligned;
}

function divCeil(n: number, d: number): number {
  return Math.floor((n + d - 1) / d);
}

class AeroSparseWriter {
  private readonly tableEntries: number;
  private readonly dataOffset: number;
  private readonly table: Float64Array;
  private allocatedBlocks = 0;
  private nextPhys: number;

  private readonly sync: SyncAccessHandleLike;
  private readonly diskSizeBytes: number;
  private readonly blockSizeBytes: number;

  constructor(sync: SyncAccessHandleLike, diskSizeBytes: number, blockSizeBytes: number) {
    this.sync = sync;
    this.diskSizeBytes = diskSizeBytes;
    this.blockSizeBytes = blockSizeBytes;
    this.tableEntries = divCeil(diskSizeBytes, blockSizeBytes);
    const tableBytes = this.tableEntries * 8;
    this.dataOffset = alignUp(AEROSPAR_HEADER_SIZE + tableBytes, blockSizeBytes);
    this.table = new Float64Array(this.tableEntries);
    this.nextPhys = this.dataOffset;

    // Ensure header + table region exists (filled with zeros).
    this.sync.truncate(this.dataOffset);
    const header: AeroSparseHeader = {
      version: AEROSPAR_VERSION,
      blockSizeBytes: this.blockSizeBytes,
      diskSizeBytes: this.diskSizeBytes,
      tableEntries: this.tableEntries,
      dataOffset: this.dataOffset,
      allocatedBlocks: 0,
    };
    this.sync.write(encodeAeroSparseHeader(header), { at: 0 });
    this.sync.write(new Uint8Array(tableBytes), { at: AEROSPAR_HEADER_SIZE });
  }

  get convertedSize(): number {
    return this.nextPhys;
  }

  writeBlock(blockIndex: number, data: Uint8Array): void {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.tableEntries) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
    if (data.byteLength !== this.blockSizeBytes) throw new Error("writeBlock: incorrect block size");

    let phys = this.table[blockIndex];
    if (phys === 0) {
      phys = this.nextPhys;
      this.table[blockIndex] = phys;
      this.allocatedBlocks += 1;
      this.nextPhys += this.blockSizeBytes;
      if (this.nextPhys > this.sync.getSize()) {
        this.sync.truncate(this.nextPhys);
      }
    }

    const written = this.sync.write(data, { at: phys });
    if (written !== data.byteLength) throw new Error(`short write at=${phys}: expected=${data.byteLength} actual=${written}`);
  }

  finalize(): void {
    // Persist header.
    const header: AeroSparseHeader = {
      version: AEROSPAR_VERSION,
      blockSizeBytes: this.blockSizeBytes,
      diskSizeBytes: this.diskSizeBytes,
      tableEntries: this.tableEntries,
      dataOffset: this.dataOffset,
      allocatedBlocks: this.allocatedBlocks,
    };
    this.sync.write(encodeAeroSparseHeader(header), { at: 0 });

    // Persist allocation table.
    const chunkEntries = 1024;
    const buf = new ArrayBuffer(chunkEntries * 8);
    const view = new DataView(buf);
    for (let i = 0; i < this.tableEntries; i += chunkEntries) {
      const count = Math.min(chunkEntries, this.tableEntries - i);
      for (let j = 0; j < count; j++) {
        const phys = this.table[i + j];
        view.setBigUint64(j * 8, BigInt(phys), true);
      }
      this.sync.write(new Uint8Array(buf, 0, count * 8), { at: AEROSPAR_HEADER_SIZE + i * 8 });
    }

    // Trim to the end of the last allocated block for nicer UX.
    this.sync.truncate(this.nextPhys);
    this.sync.flush();
  }
}

async function convertRawToSparse(
  src: RandomAccessSource,
  sync: SyncAccessHandleLike,
  blockSize: number,
  options: ImportConvertOptions,
): Promise<{ manifest: ImportManifest }> {
  const logicalSize = src.size;
  const writer = new AeroSparseWriter(sync, logicalSize, blockSize);
  let crc = crc32Init();

  const buf = new Uint8Array(blockSize);
  const totalBlocks = divCeil(logicalSize, blockSize);

  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
    buf.fill(0);
    const off = blockIndex * blockSize;
    const len = Math.min(blockSize, logicalSize - off);
    if (len > 0) buf.set(await src.readAt(off, len), 0);

    let any = false;
    for (const b of buf) {
      if (b !== 0) {
        any = true;
        break;
      }
    }
    if (any) {
      writer.writeBlock(blockIndex, buf);
      crc = crc32Update(crc, u64le(BigInt(blockIndex)));
      crc = crc32Update(crc, buf);
    }

    options.onProgress?.({ processedBytes: Math.min((blockIndex + 1) * blockSize, logicalSize), totalBytes: logicalSize });
  }

  writer.finalize();
  const manifest: ImportManifest = {
    manifestVersion: 1,
    originalFormat: "raw",
    convertedFormat: "aerospar",
    logicalSize,
    convertedSize: writer.convertedSize,
    checksum: { algorithm: "crc32", value: crc32ToHex(crc32Final(crc)) },
    blockSizeBytes: blockSize,
    readOnly: false,
  };
  return { manifest };
}

const QCOW2_OFFSET_MASK = 0x00ff_ffff_ffff_fe00n;
const QCOW2_OFLAG_COMPRESSED = 1n << 62n;
const QCOW2_OFLAG_ZERO = 1n;

async function convertQcow2ToSparse(
  src: RandomAccessSource,
  sync: SyncAccessHandleLike,
  blockSize: number,
  options: ImportConvertOptions,
): Promise<{ manifest: ImportManifest }> {
  const qcow = await Qcow2.open(src);
  const outBlockSize = nextPow2(Math.max(blockSize, qcow.clusterSize));
  assertBlockSize(outBlockSize);

  const writer = new AeroSparseWriter(sync, qcow.logicalSize, outBlockSize);
  let crc = crc32Init();

  const clustersPerBlock = outBlockSize / qcow.clusterSize;
  const totalBlocks = divCeil(qcow.logicalSize, outBlockSize);
  const buf = new Uint8Array(outBlockSize);

  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");

    const startCluster = blockIndex * clustersPerBlock;
    const endCluster = Math.min(startCluster + clustersPerBlock, qcow.clusterOffsets.length);
    let hasAny = false;
    for (let i = startCluster; i < endCluster; i++) {
      if (qcow.clusterOffsets[i] !== 0) {
        hasAny = true;
        break;
      }
    }
    if (!hasAny) {
      options.onProgress?.({
        processedBytes: Math.min((blockIndex + 1) * outBlockSize, qcow.logicalSize),
        totalBytes: qcow.logicalSize,
      });
      continue;
    }

    buf.fill(0);
    for (let i = startCluster; i < endCluster; i++) {
      const phys = qcow.clusterOffsets[i];
      if (phys === 0) continue;
      const guestOff = i * qcow.clusterSize;
      const len = Math.min(qcow.clusterSize, qcow.logicalSize - guestOff);
      const chunk = await src.readAt(phys, len);
      buf.set(chunk, (i - startCluster) * qcow.clusterSize);
    }

    writer.writeBlock(blockIndex, buf);
    crc = crc32Update(crc, u64le(BigInt(blockIndex)));
    crc = crc32Update(crc, buf);
    options.onProgress?.({
      processedBytes: Math.min((blockIndex + 1) * outBlockSize, qcow.logicalSize),
      totalBytes: qcow.logicalSize,
    });
  }

  writer.finalize();

  const manifest: ImportManifest = {
    manifestVersion: 1,
    originalFormat: "qcow2",
    convertedFormat: "aerospar",
    logicalSize: qcow.logicalSize,
    convertedSize: writer.convertedSize,
    checksum: { algorithm: "crc32", value: crc32ToHex(crc32Final(crc)) },
    blockSizeBytes: outBlockSize,
    readOnly: false,
  };
  return { manifest };
}

const VHD_TYPE_FIXED = 2;
const VHD_TYPE_DYNAMIC = 3;
const VHD_BAT_FREE = 0xffff_ffff;

async function convertVhdToSparse(
  src: RandomAccessSource,
  sync: SyncAccessHandleLike,
  blockSize: number,
  options: ImportConvertOptions,
): Promise<{ manifest: ImportManifest }> {
  const footer = await VhdFooter.read(src);
  const logicalSize = footer.currentSize;

  if (footer.diskType === VHD_TYPE_FIXED) {
    // Fixed VHD is raw data followed by footer.
    //
    // Some tools may also write a redundant copy of the footer at offset 0. When present and
    // identical to the EOF footer, the data region begins immediately after it.
    let baseOffset = 0;
    if (src.size >= 1024) {
      const cookie0 = await src.readAt(0, 8);
      if (ascii(cookie0) === "conectix") {
        try {
          const footer0 = VhdFooter.parse(await src.readAt(0, 512));
          if (footer0.diskType === VHD_TYPE_FIXED && bytesEqual(footer0.raw, footer.raw)) {
            baseOffset = 512;
          }
        } catch {
          // Ignore: offset 0 may be raw disk data that happens to start with the VHD cookie.
        }
      }
    }
    return await convertRawSliceToSparse(src, sync, blockSize, logicalSize, baseOffset, options, "vhd");
  }

  if (footer.diskType !== VHD_TYPE_DYNAMIC) {
    throw new Error(`unsupported VHD type ${footer.diskType}`);
  }
  const dyn = await VhdDynamicHeader.read(src, footer.dataOffset);
  if (!Number.isSafeInteger(dyn.blockSize) || dyn.blockSize <= 0) throw new Error("invalid VHD block size");
  assertBlockSize(dyn.blockSize);

  const expectedEntries = divCeil(logicalSize, dyn.blockSize);
  if (dyn.maxTableEntries !== expectedEntries) throw new Error("VHD max_table_entries mismatch");

  const bat = await VhdBat.read(src, dyn.tableOffset, dyn.maxTableEntries);
  const writer = new AeroSparseWriter(sync, logicalSize, dyn.blockSize);
  let crc = crc32Init();

  const sectorsPerBlock = dyn.blockSize / 512;
  const bitmapBytes = Math.ceil(sectorsPerBlock / 8);
  const bitmapSize = nextPow2(Math.max(512, bitmapBytes));
  const bitmap = new Uint8Array(bitmapSize);
  const buf = new Uint8Array(dyn.blockSize);
  const totalBlocks = divCeil(logicalSize, dyn.blockSize);

  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
    const entry = bat.entries[blockIndex];
    if (entry === VHD_BAT_FREE) {
      options.onProgress?.({
        processedBytes: Math.min((blockIndex + 1) * dyn.blockSize, logicalSize),
        totalBytes: logicalSize,
      });
      continue;
    }

    const blockOff = entry * 512;
    bitmap.set(await src.readAt(blockOff, bitmapSize));
    let any = false;
    for (let i = 0; i < bitmapBytes; i++) {
      if (bitmap[i] !== 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      options.onProgress?.({
        processedBytes: Math.min((blockIndex + 1) * dyn.blockSize, logicalSize),
        totalBytes: logicalSize,
      });
      continue;
    }

    buf.fill(0);
    const dataBase = blockOff + bitmapSize;
    let sector = 0;
    while (sector < sectorsPerBlock) {
      if (!vhdBitmapBit(bitmap, sector)) {
        sector++;
        continue;
      }
      const start = sector;
      while (sector < sectorsPerBlock && vhdBitmapBit(bitmap, sector)) sector++;
      const runLen = sector - start;
      const bytes = runLen * 512;
      const chunk = await src.readAt(dataBase + start * 512, bytes);
      buf.set(chunk, start * 512);
    }

    writer.writeBlock(blockIndex, buf);
    crc = crc32Update(crc, u64le(BigInt(blockIndex)));
    crc = crc32Update(crc, buf);
    options.onProgress?.({
      processedBytes: Math.min((blockIndex + 1) * dyn.blockSize, logicalSize),
      totalBytes: logicalSize,
    });
  }

  writer.finalize();
  const manifest: ImportManifest = {
    manifestVersion: 1,
    originalFormat: "vhd",
    convertedFormat: "aerospar",
    logicalSize,
    convertedSize: writer.convertedSize,
    checksum: { algorithm: "crc32", value: crc32ToHex(crc32Final(crc)) },
    blockSizeBytes: dyn.blockSize,
    readOnly: false,
  };
  return { manifest };
}

async function convertRawSliceToSparse(
  src: RandomAccessSource,
  sync: SyncAccessHandleLike,
  blockSize: number,
  logicalSize: number,
  baseOffset: number,
  options: ImportConvertOptions,
  originalFormat: Exclude<ImageFormat, "iso">,
): Promise<{ manifest: ImportManifest }> {
  const writer = new AeroSparseWriter(sync, logicalSize, blockSize);
  let crc = crc32Init();
  const buf = new Uint8Array(blockSize);
  const totalBlocks = divCeil(logicalSize, blockSize);

  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
    buf.fill(0);
    const off = blockIndex * blockSize;
    const len = Math.min(blockSize, logicalSize - off);
    if (len > 0) buf.set(await src.readAt(baseOffset + off, len), 0);
    let any = false;
    for (const b of buf) {
      if (b !== 0) {
        any = true;
        break;
      }
    }
    if (any) {
      writer.writeBlock(blockIndex, buf);
      crc = crc32Update(crc, u64le(BigInt(blockIndex)));
      crc = crc32Update(crc, buf);
    }
    options.onProgress?.({ processedBytes: Math.min((blockIndex + 1) * blockSize, logicalSize), totalBytes: logicalSize });
  }
  writer.finalize();

  const manifest: ImportManifest = {
    manifestVersion: 1,
    originalFormat,
    convertedFormat: "aerospar",
    logicalSize,
    convertedSize: writer.convertedSize,
    checksum: { algorithm: "crc32", value: crc32ToHex(crc32Final(crc)) },
    blockSizeBytes: blockSize,
    readOnly: false,
  };
  return { manifest };
}

function ascii(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += String.fromCharCode(b);
  return out;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function readU32BE(buf: Uint8Array, offset: number): number {
  return (
    (buf[offset] << 24) |
    (buf[offset + 1] << 16) |
    (buf[offset + 2] << 8) |
    buf[offset + 3]
  ) >>> 0;
}

function readU64BE(buf: Uint8Array, offset: number): bigint {
  const hi = BigInt(readU32BE(buf, offset));
  const lo = BigInt(readU32BE(buf, offset + 4));
  return (hi << 32n) | lo;
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

function nextPow2(n: number): number {
  if (!Number.isSafeInteger(n) || n <= 0) throw new Error(`invalid n=${n}`);
  let v = 1;
  while (v < n) v <<= 1;
  return v;
}

class Qcow2 {
  static async open(src: RandomAccessSource): Promise<Qcow2> {
    const hdr72 = await src.readAt(0, 72);
    if (hdr72[0] !== 0x51 || hdr72[1] !== 0x46 || hdr72[2] !== 0x49 || hdr72[3] !== 0xfb) {
      throw new Error("invalid qcow2 magic");
    }
    const version = readU32BE(hdr72, 4);
    if (version !== 2 && version !== 3) throw new Error(`unsupported qcow2 version ${version}`);
    const hdr = version === 3 ? new Uint8Array(104) : hdr72;
    if (version === 3) {
      hdr.set(hdr72, 0);
      hdr.set(await src.readAt(72, 32), 72);
    }

    const backingFileOffset = readU64BE(hdr, 8);
    const backingFileSize = readU32BE(hdr, 16);
    if (backingFileOffset !== 0n || backingFileSize !== 0) throw new Error("qcow2 backing files unsupported");
    const clusterBits = readU32BE(hdr, 20);
    if (clusterBits < 9 || clusterBits > 21) throw new Error(`unsupported qcow2 cluster_bits ${clusterBits}`);
    const logicalSize = Number(readU64BE(hdr, 24));
    if (!Number.isSafeInteger(logicalSize) || logicalSize <= 0) throw new Error("invalid qcow2 virtual size");
    const cryptMethod = readU32BE(hdr, 32);
    if (cryptMethod !== 0) throw new Error("qcow2 encryption unsupported");
    const l1Size = readU32BE(hdr, 36);
    if (l1Size === 0) throw new Error("qcow2 l1_size is zero");
    const l1TableOffset = Number(readU64BE(hdr, 40));
    if (!Number.isSafeInteger(l1TableOffset) || l1TableOffset <= 0) throw new Error("invalid qcow2 l1_table_offset");
    const nbSnapshots = readU32BE(hdr, 60);
    if (nbSnapshots !== 0) throw new Error("qcow2 snapshots unsupported");

    const clusterSize = 1 << clusterBits;
    const l1Bytes = l1Size * 8;
    const l1 = await src.readAt(l1TableOffset, l1Bytes);
    const l2Entries = clusterSize / 8;
    const totalClusters = divCeil(logicalSize, clusterSize);
    const clusterOffsets = new Array<number>(totalClusters).fill(0);

    for (let l1Index = 0; l1Index < l1Size; l1Index++) {
      const entry = readU64BE(l1, l1Index * 8) & QCOW2_OFFSET_MASK;
      if (entry === 0n) continue;
      const l2Off = Number(entry);
      if (!Number.isSafeInteger(l2Off) || l2Off <= 0) throw new Error("invalid qcow2 l2 table offset");
      const l2 = await src.readAt(l2Off, clusterSize);

      for (let l2Index = 0; l2Index < l2Entries; l2Index++) {
        const clusterIndex = l1Index * l2Entries + l2Index;
        if (clusterIndex >= totalClusters) break;
        const val = readU64BE(l2, l2Index * 8);
        if (val === 0n) continue;
        if ((val & QCOW2_OFLAG_COMPRESSED) !== 0n) throw new Error("qcow2 compressed clusters unsupported");
        if ((val & QCOW2_OFLAG_ZERO) !== 0n) continue;
        const dataOff = Number(val & QCOW2_OFFSET_MASK);
        if (dataOff === 0) continue;
        clusterOffsets[clusterIndex] = dataOff;
      }
    }

    return new Qcow2(logicalSize, clusterSize, clusterOffsets);
  }

  public readonly logicalSize: number;
  public readonly clusterSize: number;
  public readonly clusterOffsets: number[];

  private constructor(logicalSize: number, clusterSize: number, clusterOffsets: number[]) {
    this.logicalSize = logicalSize;
    this.clusterSize = clusterSize;
    this.clusterOffsets = clusterOffsets;
  }
}

class VhdFooter {
  static parse(footerBytes: Uint8Array): VhdFooter {
    if (footerBytes.byteLength !== 512) throw new Error("invalid VHD footer length");
    if (ascii(footerBytes.subarray(0, 8)) !== "conectix") throw new Error("missing VHD footer cookie");

    const storedChecksum = readU32BE(footerBytes, 64);
    const copy = footerBytes.slice();
    copy.fill(0, 64, 68);
    let sum = 0;
    for (const b of copy) sum = (sum + b) >>> 0;
    const expected = (~sum) >>> 0;
    if (expected !== storedChecksum) throw new Error("VHD footer checksum mismatch");

    const dataOffset = Number(readU64BE(footerBytes, 16));
    const currentSize = Number(readU64BE(footerBytes, 48));
    const diskType = readU32BE(footerBytes, 60);
    if (!Number.isSafeInteger(currentSize) || currentSize <= 0) throw new Error("invalid VHD current_size");
    return new VhdFooter(dataOffset, currentSize, diskType, footerBytes.slice());
  }

  static async read(src: RandomAccessSource): Promise<VhdFooter> {
    if (src.size < 512) throw new Error("VHD too small");
    const footerBytes = await src.readAt(src.size - 512, 512);
    return VhdFooter.parse(footerBytes);
  }

  public readonly dataOffset: number;
  public readonly currentSize: number;
  public readonly diskType: number;
  public readonly raw: Uint8Array;

  private constructor(dataOffset: number, currentSize: number, diskType: number, raw: Uint8Array) {
    this.dataOffset = dataOffset;
    this.currentSize = currentSize;
    this.diskType = diskType;
    this.raw = raw;
  }
}

class VhdDynamicHeader {
  static async read(src: RandomAccessSource, offset: number): Promise<VhdDynamicHeader> {
    if (!Number.isSafeInteger(offset) || offset <= 0) throw new Error("invalid VHD dynamic header offset");
    const dh = await src.readAt(offset, 1024);
    if (ascii(dh.subarray(0, 8)) !== "cxsparse") throw new Error("missing VHD dynamic header cookie");

    const storedChecksum = readU32BE(dh, 36);
    const copy = dh.slice();
    copy.fill(0, 36, 40);
    let sum = 0;
    for (const b of copy) sum = (sum + b) >>> 0;
    const expected = (~sum) >>> 0;
    if (expected !== storedChecksum) throw new Error("VHD dynamic header checksum mismatch");

    const tableOffset = Number(readU64BE(dh, 16));
    const maxTableEntries = readU32BE(dh, 28);
    const blockSize = readU32BE(dh, 32);
    if (!Number.isSafeInteger(tableOffset) || tableOffset <= 0) throw new Error("invalid VHD BAT offset");
    if (!Number.isSafeInteger(maxTableEntries) || maxTableEntries <= 0) throw new Error("invalid VHD BAT entry count");
    return new VhdDynamicHeader(tableOffset, maxTableEntries, blockSize);
  }

  public readonly tableOffset: number;
  public readonly maxTableEntries: number;
  public readonly blockSize: number;

  private constructor(tableOffset: number, maxTableEntries: number, blockSize: number) {
    this.tableOffset = tableOffset;
    this.maxTableEntries = maxTableEntries;
    this.blockSize = blockSize;
  }
}

class VhdBat {
  static async read(src: RandomAccessSource, tableOffset: number, entries: number): Promise<VhdBat> {
    const batBytes = await src.readAt(tableOffset, entries * 4);
    const out = new Array<number>(entries);
    for (let i = 0; i < entries; i++) out[i] = readU32BE(batBytes, i * 4);
    return new VhdBat(out);
  }

  public readonly entries: number[];

  private constructor(entries: number[]) {
    this.entries = entries;
  }
}

function vhdBitmapBit(bitmap: Uint8Array, sector: number): boolean {
  const b = bitmap[Math.floor(sector / 8)];
  const bit = 7 - (sector % 8);
  return ((b >> bit) & 1) !== 0;
}
