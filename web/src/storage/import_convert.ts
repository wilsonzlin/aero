import { crc32Final, crc32Init, crc32ToHex, crc32Update } from "./crc32.ts";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes.ts";
import { readResponseBytesWithLimit } from "./response_json.ts";
import {
  MAX_CACHE_CONTROL_HEADER_VALUE_LEN,
  MAX_CONTENT_ENCODING_HEADER_VALUE_LEN,
  commaSeparatedTokenListHasToken,
  contentEncodingIsIdentity,
  formatHeaderValueForError,
} from "./http_headers.ts";

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

// Browser conversion happens in-memory; avoid allocating absurdly large per-block buffers.
const MAX_CONVERT_BLOCK_BYTES = 64 * 1024 * 1024; // 64 MiB

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

function assertIdentityContentEncoding(headers: Headers, label: string): void {
  const raw = headers.get("content-encoding");
  if (!raw) return;
  if (raw.length > MAX_CONTENT_ENCODING_HEADER_VALUE_LEN) {
    throw new Error(`${label} unexpected Content-Encoding (too long)`);
  }
  if (contentEncodingIsIdentity(raw, { maxLen: MAX_CONTENT_ENCODING_HEADER_VALUE_LEN })) return;
  throw new Error(`${label} unexpected Content-Encoding: ${formatHeaderValueForError(raw)}`);
}

function assertNoTransformCacheControl(headers: Headers, label: string): void {
  // Range reads are byte-addressed. Any intermediary transform can break deterministic byte
  // semantics. Require `Cache-Control: no-transform` as defence-in-depth.
  //
  // Note: Cache-Control is CORS-safelisted, so it is readable cross-origin without
  // `Access-Control-Expose-Headers`.
  const raw = headers.get("cache-control");
  if (!raw) {
    throw new Error(`${label} missing Cache-Control header (expected include 'no-transform')`);
  }
  if (raw.length > MAX_CACHE_CONTROL_HEADER_VALUE_LEN) {
    throw new Error(`${label} Cache-Control too long`);
  }
  if (!commaSeparatedTokenListHasToken(raw, "no-transform", { maxLen: MAX_CACHE_CONTROL_HEADER_VALUE_LEN })) {
    throw new Error(`${label} Cache-Control missing no-transform: ${formatHeaderValueForError(raw)}`);
  }
}

async function cancelBody(resp: Response): Promise<void> {
  try {
    await resp.body?.cancel();
  } catch {
    // ignore best-effort cancellation failures
  }
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
    if (length === 0) return new Uint8Array(new ArrayBuffer(0));
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
    try {
      if (!res.ok) throw new Error(`range fetch failed (${res.status})`);
      if (res.status !== 206 && !(res.status === 200 && offset === 0 && end === this.size)) {
        throw new Error(`range fetch did not return 206 Partial Content (status=${res.status})`);
      }
      assertIdentityContentEncoding(res.headers, "range fetch");
      assertNoTransformCacheControl(res.headers, "range fetch");
      const bytes = await readResponseBytesWithLimit(res, { maxBytes: length, label: "range fetch body" });
      if (bytes.byteLength !== length) {
        throw new Error(`short range read: expected ${length}, got ${bytes.byteLength}`);
      }
      return bytes;
    } finally {
      await cancelBody(res);
    }
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
    let writable: FileSystemWritableFileStream;
    let truncateFallback = false;
    try {
      writable = await fileHandle.createWritable({ keepExistingData: false });
    } catch {
      // Some implementations may not accept options; fall back to default.
      writable = await fileHandle.createWritable();
      truncateFallback = true;
    }
    if (truncateFallback) {
      // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
      // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
      try {
        await writable.truncate(0);
      } catch {
        // ignore
      }
    }
    let crc32: number;
    try {
      ({ crc32 } = await copySequentialCrc32(src, writable, options.signal, options.onProgress));
      await writable.close();
    } catch (err) {
      try {
        await writable.abort(err);
      } catch {
        // ignore abort failures
      }
      throw err;
    }

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

  // qcow2: "QFI\xfb" at offset 0 + a plausible version at offset 4.
  //
  // For truncated images (< 72 bytes, the minimum v2 header size) that still match the magic,
  // treat them as QCOW2 so callers get a corruption error instead of silently falling back to raw.
  if (src.size >= 4 && src.size < 72) {
    const sig4 = await src.readAt(0, 4);
    if (sig4[0] === 0x51 && sig4[1] === 0x46 && sig4[2] === 0x49 && sig4[3] === 0xfb) return "qcow2";
  }
  if (src.size >= 72) {
    const sig = await src.readAt(0, 8);
    if (sig[0] === 0x51 && sig[1] === 0x46 && sig[2] === 0x49 && sig[3] === 0xfb) {
      const version = readU32BE(sig, 4);
      if (version === 2 || version === 3) return "qcow2";
    }
  }

  // VHD: if we only have the cookie but not enough bytes for a full footer, still classify as VHD
  // so the subsequent open/conversion step can fail with a meaningful corruption error.
  if (src.size >= 8 && src.size < 512) {
    const cookie0 = await src.readAt(0, 8);
    if (ascii(cookie0) === "conectix") return "vhd";
  }

  // VHD: check for a *plausible* footer at end (fixed and dynamic disks).
  //
  // Note: We intentionally do not require a valid checksum here. A corrupted VHD should still
  // detect as VHD so that the subsequent conversion/open step can fail with a meaningful error,
  // instead of silently falling back to treating it as a raw disk.
  if (src.size >= 512) {
    const cookieEnd = await src.readAt(src.size - 512, 8);
    if (ascii(cookieEnd) === "conectix") {
      const footer = await src.readAt(src.size - 512, 512);
      if (looksLikeVhdFooter(footer, src.size)) return "vhd";
    }
  }

  // VHD: dynamic disks typically store a footer copy at offset 0. Some fixed disks may also
  // contain a redundant footer copy at offset 0.
  if (src.size >= 512) {
    const cookie0 = await src.readAt(0, 8);
    if (ascii(cookie0) === "conectix") {
      const footer = await src.readAt(0, 512);
      if (looksLikeVhdFooter(footer, src.size)) {
        const diskType = readU32BE(footer, 60);
        if (diskType === VHD_TYPE_FIXED) {
          // A fixed VHD footer at offset 0 implies an additional required footer at EOF, so the
          // file must be large enough to contain:
          //   footer_copy (512) + data (current_size) + eof_footer (512)
          const currentSize = Number(readU64BE(footer, 48));
          const required = currentSize + 1024;
          if (Number.isSafeInteger(required) && src.size >= required) return "vhd";
        } else {
          return "vhd";
        }
      }
    }
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
  try {
    const cr = res.headers.get("content-range");
    if (!res.ok || !cr) throw new Error("unable to determine remote size (missing Content-Range)");
    assertIdentityContentEncoding(res.headers, "size probe");
    assertNoTransformCacheControl(res.headers, "size probe");
    // Best-effort: consume the body so servers that ignore Range don't accidentally stream the
    // full object. (We only expect a single byte.)
    const body = await readResponseBytesWithLimit(res, { maxBytes: 1, label: "size probe body" });
    if (body.byteLength !== 1) {
      throw new Error(`unexpected size probe body length ${body.byteLength} (expected 1)`);
    }
    const match = cr.match(/\/(\d+)$/);
    if (!match) throw new Error(`unexpected Content-Range: ${cr}`);
    const size = Number(match[1]);
    if (!Number.isSafeInteger(size) || size < 0) throw new Error(`invalid size from Content-Range: ${cr}`);
    return size;
  } finally {
    await cancelBody(res);
  }
}

async function writeManifest(
  dir: FileSystemDirectoryHandle,
  baseName: string,
  manifest: ImportManifest,
): Promise<void> {
  const fh = await dir.getFileHandle(`${baseName}.manifest.json`, { create: true });
  let w: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    w = await fh.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    w = await fh.createWritable();
    truncateFallback = true;
  }
  if (truncateFallback) {
    // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
    // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
    try {
      await w.truncate(0);
    } catch {
      // ignore
    }
  }
  try {
    await w.write(JSON.stringify(manifest, null, 2));
    await w.close();
  } catch (err) {
    try {
      await w.abort(err);
    } catch {
      // ignore abort failures
    }
    throw err;
  }
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
  if (blockSize > MAX_CONVERT_BLOCK_BYTES) throw new Error("blockSizeBytes too large");

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
// Must remain <= the browser-side runtime limit in `web/src/storage/opfs_sparse.ts` so converted
// disks can be opened by `OpfsAeroSparseDisk`.
const AEROSPAR_MAX_TABLE_BYTES = 64 * 1024 * 1024;
const AEROSPAR_MAX_TABLE_ENTRIES = AEROSPAR_MAX_TABLE_BYTES / 8;

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
  if (!Number.isSafeInteger(n) || !Number.isSafeInteger(d) || d <= 0) {
    throw new Error("divCeil: arguments must be safe positive integers");
  }
  const out = Number((BigInt(n) + BigInt(d) - 1n) / BigInt(d));
  if (!Number.isSafeInteger(out)) throw new Error("divCeil overflow");
  return out;
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
    if (!Number.isSafeInteger(diskSizeBytes) || diskSizeBytes <= 0) {
      throw new Error("invalid disk size");
    }
    if (diskSizeBytes % 512 !== 0) {
      throw new Error("disk size must be a multiple of 512");
    }
    this.tableEntries = divCeil(diskSizeBytes, blockSizeBytes);
    if (!Number.isSafeInteger(this.tableEntries) || this.tableEntries <= 0) {
      throw new Error("invalid aerosparse table size");
    }
    if (this.tableEntries > AEROSPAR_MAX_TABLE_ENTRIES) {
      throw new Error("aerosparse allocation table too large");
    }
    const tableBytes = this.tableEntries * 8;
    if (!Number.isSafeInteger(tableBytes) || tableBytes > AEROSPAR_MAX_TABLE_BYTES) {
      throw new Error("aerosparse allocation table too large");
    }
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
    // Zero the on-disk table region in chunks to avoid allocating `tableBytes` all at once.
    // This matters for large but still-valid sparse images (e.g. multi-GB disks with small block sizes).
    const zeroChunk = new Uint8Array(Math.min(64 * 1024, tableBytes));
    let remaining = tableBytes;
    let off = AEROSPAR_HEADER_SIZE;
    while (remaining > 0) {
      const len = Math.min(remaining, zeroChunk.byteLength);
      const written = this.sync.write(zeroChunk.subarray(0, len), { at: off });
      if (written !== len) throw new Error(`short write at=${off}: expected=${len} actual=${written}`);
      off += len;
      remaining -= len;
    }
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
const QCOW2_MAX_TABLE_BYTES = 128 * 1024 * 1024;
const QCOW2_MAX_CLUSTER_OFFSETS_BYTES = 128 * 1024 * 1024;
// Avoid pathological images forcing huge JS `Set` allocations while validating metadata overlap.
const QCOW2_MAX_METADATA_CLUSTERS = 1_000_000;

async function convertQcow2ToSparse(
  src: RandomAccessSource,
  sync: SyncAccessHandleLike,
  blockSize: number,
  options: ImportConvertOptions,
): Promise<{ manifest: ImportManifest }> {
  const qcow = await Qcow2.open(src, blockSize, options.signal);
  const outBlockSize = nextPow2(Math.max(blockSize, qcow.clusterSize));
  assertBlockSize(outBlockSize);

  const writer = new AeroSparseWriter(sync, qcow.logicalSize, outBlockSize);
  let crc = crc32Init();

  // Cap individual reads when copying cluster data so remote imports don't issue extremely large
  // HTTP range requests when `outBlockSize` is large.
  const QCOW2_COPY_RUN_MAX_BYTES = 8 * 1024 * 1024; // 8 MiB

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
    // Merge contiguous clusters into larger reads to reduce IO calls (especially important for
    // remote URL imports where each readAt may translate to a separate HTTP Range request).
    let i = startCluster;
    while (i < endCluster) {
      if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
      const phys = qcow.clusterOffsets[i];
      if (phys === 0) {
        i++;
        continue;
      }

      let runClusters = 1;
      while (i + runClusters < endCluster) {
        const nextPhys = qcow.clusterOffsets[i + runClusters];
        if (nextPhys === 0) break;
        if (nextPhys !== phys + runClusters * qcow.clusterSize) break;
        if ((runClusters + 1) * qcow.clusterSize > QCOW2_COPY_RUN_MAX_BYTES) break;
        runClusters++;
      }

      const guestOff = i * qcow.clusterSize;
      const runBytes = Math.min(runClusters * qcow.clusterSize, qcow.logicalSize - guestOff);
      const chunk = await src.readAt(phys, runBytes);
      buf.set(chunk, (i - startCluster) * qcow.clusterSize);
      i += runClusters;
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
const VHD_MAX_BAT_BYTES = 128 * 1024 * 1024;
const VHD_MAX_BLOCK_BYTES = MAX_CONVERT_BLOCK_BYTES;
const VHD_MAX_BITMAP_BYTES = 32 * 1024 * 1024;

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
    // valid (but not necessarily byte-for-byte identical to the EOF footer), the data region
    // begins immediately after it.
    let baseOffset = 0;
    const requiredWithFooterCopy = footer.currentSize + 1024;
    if (src.size >= 1024) {
      const cookie0 = await src.readAt(0, 8);
      if (ascii(cookie0) === "conectix") {
        try {
          const footer0 = VhdFooter.parse(await src.readAt(0, 512));
          if (
            footer0.diskType === VHD_TYPE_FIXED &&
            footer0.currentSize === footer.currentSize &&
            Number.isSafeInteger(requiredWithFooterCopy) &&
            src.size >= requiredWithFooterCopy
          ) {
            baseOffset = 512;
          }
        } catch {
          // Ignore: offset 0 may be raw disk data that happens to start with the VHD cookie.
        }
      }
    }
    const requiredLen = baseOffset + logicalSize + 512;
    if (!Number.isSafeInteger(requiredLen) || src.size < requiredLen) {
      throw new Error("VHD fixed disk truncated");
    }
    return await convertRawSliceToSparse(src, sync, blockSize, logicalSize, baseOffset, options, "vhd");
  }

  if (footer.diskType !== VHD_TYPE_DYNAMIC) {
    throw new Error(`unsupported VHD type ${footer.diskType}`);
  }

  // Dynamic VHDs require:
  // - a footer copy at offset 0 that matches the EOF footer
  // - a dynamic header fully contained before the EOF footer
  const footerOffset = src.size - 512;
  const dynHeaderEnd = footer.dataOffset + 1024;
  if (!Number.isSafeInteger(dynHeaderEnd) || dynHeaderEnd > footerOffset) {
    throw new Error("VHD dynamic header overlaps footer");
  }
  let footerCopyBytes: Uint8Array;
  try {
    footerCopyBytes = await src.readAt(0, 512);
  } catch {
    throw new Error("VHD footer copy truncated");
  }
  let footerCopy: VhdFooter;
  try {
    footerCopy = VhdFooter.parse(footerCopyBytes);
  } catch {
    throw new Error("VHD footer copy mismatch");
  }
  if (!bytesEqual(footerCopy.raw, footer.raw)) {
    throw new Error("VHD footer copy mismatch");
  }

  const dyn = await VhdDynamicHeader.read(src, footer.dataOffset);
  if (!Number.isSafeInteger(dyn.blockSize) || dyn.blockSize <= 0) throw new Error("invalid VHD block size");
  assertBlockSize(dyn.blockSize);
  if (dyn.blockSize > VHD_MAX_BLOCK_BYTES) throw new Error("VHD block_size too large");

  const expectedEntries = divCeil(logicalSize, dyn.blockSize);
  if (dyn.maxTableEntries < expectedEntries) throw new Error("VHD max_table_entries too small");

  // Validate the on-disk BAT size based on max_table_entries. We only need to read entries required
  // for the advertised disk size, but the metadata region must still be coherent and bounded.
  const batBytesOnDisk = dyn.maxTableEntries * 4;
  if (!Number.isSafeInteger(batBytesOnDisk) || batBytesOnDisk > VHD_MAX_BAT_BYTES) throw new Error("VHD BAT too large");
  const batSizeOnDisk = alignUp(batBytesOnDisk, 512);
  const batEndOnDisk = dyn.tableOffset + batSizeOnDisk;
  if (!Number.isSafeInteger(batEndOnDisk) || batEndOnDisk > footerOffset) {
    throw new Error("VHD BAT truncated");
  }
  if (rangesOverlap(footer.dataOffset, dynHeaderEnd, dyn.tableOffset, batEndOnDisk)) {
    throw new Error("VHD BAT overlaps dynamic header");
  }

  // Fail fast if the output AeroSparse allocation table would exceed the runtime cap (64MiB).
  // This avoids allocating/reading a potentially large VHD BAT only to reject later when creating
  // the output sparse writer.
  if (expectedEntries > AEROSPAR_MAX_TABLE_ENTRIES) {
    throw new Error("aerosparse allocation table too large");
  }

  // Only read the entries required for the advertised virtual size. This avoids allocating
  // memory proportional to max_table_entries when it is larger than needed.
  const bat = await VhdBat.read(src, dyn.tableOffset, expectedEntries);
  const writer = new AeroSparseWriter(sync, logicalSize, dyn.blockSize);
  let crc = crc32Init();

  // Cap individual reads when copying dynamic block data so remote imports don't issue extremely
  // large HTTP range requests when `block_size` is large.
  const VHD_COPY_RUN_MAX_BYTES = 8 * 1024 * 1024; // 8 MiB

  const sectorsPerBlock = dyn.blockSize / 512;
  const bitmapBytes = Math.ceil(sectorsPerBlock / 8);
  const bitmapSize = nextPow2(Math.max(512, bitmapBytes));
  if (bitmapSize > VHD_MAX_BITMAP_BYTES) throw new Error("VHD bitmap too large");
  const blockTotalSize = bitmapSize + dyn.blockSize;
  if (!Number.isSafeInteger(blockTotalSize) || blockTotalSize <= 0) throw new Error("invalid VHD block size");
  const bitmap = new Uint8Array(bitmapSize);
  const buf = new Uint8Array(dyn.blockSize);
  const totalBlocks = expectedEntries;

  // Pre-validate BAT entries to avoid reading metadata as block payload when the image is corrupt.
  const blockTotalSectors = blockTotalSize / 512;
  if (!Number.isSafeInteger(blockTotalSectors) || blockTotalSectors <= 0) throw new Error("invalid VHD block size");
  const footerSector = footerOffset / 512;
  const metadataRanges: Array<{ start: number; end: number }> = [
    { start: 0, end: 1 }, // footer copy
    { start: footer.dataOffset / 512, end: dynHeaderEnd / 512 }, // dynamic header
    { start: dyn.tableOffset / 512, end: batEndOnDisk / 512 }, // BAT (including padding)
  ];

  // First pass: validate all allocated BAT entries and count them.
  let allocatedCount = 0;
  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    const entry = bat.entries[blockIndex]!;
    if (entry === VHD_BAT_FREE) continue;
    if (entry < 1) throw new Error("VHD block offset invalid");
    const startSector = entry;
    const endSector = startSector + blockTotalSectors;
    if (endSector > footerSector) throw new Error("VHD block out of range");
    for (const r of metadataRanges) {
      if (rangesOverlap(startSector, endSector, r.start, r.end)) throw new Error("VHD block overlaps metadata");
    }
    allocatedCount++;
  }

  // Second pass: collect allocated block starts, sort, and detect overlaps/duplicates.
  let allocatedStarts: Uint32Array;
  try {
    allocatedStarts = new Uint32Array(allocatedCount);
  } catch {
    throw new Error("VHD too many allocated blocks");
  }
  let pos = 0;
  for (let blockIndex = 0; blockIndex < totalBlocks; blockIndex++) {
    const entry = bat.entries[blockIndex]!;
    if (entry === VHD_BAT_FREE) continue;
    allocatedStarts[pos++] = entry;
  }
  allocatedStarts.sort();
  for (let i = 1; i < allocatedStarts.length; i++) {
    const prev = allocatedStarts[i - 1]!;
    const cur = allocatedStarts[i]!;
    if (cur < prev + blockTotalSectors) throw new Error("VHD blocks overlap");
  }

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
      let remaining = bytes;
      let rel = start * 512;
      while (remaining > 0) {
        if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
        const len = Math.min(remaining, VHD_COPY_RUN_MAX_BYTES);
        const chunk = await src.readAt(dataBase + rel, len);
        buf.set(chunk, rel);
        rel += len;
        remaining -= len;
      }
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

function looksLikeVhdFooter(footerBytes: Uint8Array, fileSize: number): boolean {
  if (footerBytes.byteLength !== 512) return false;
  if (ascii(footerBytes.subarray(0, 8)) !== "conectix") return false;

  // Fixed file format version for VHD footers.
  if (readU32BE(footerBytes, 12) !== 0x0001_0000) return false;

  const currentSize = Number(readU64BE(footerBytes, 48));
  if (!Number.isSafeInteger(currentSize) || currentSize <= 0) return false;
  if (currentSize % 512 !== 0) return false;

  const diskType = readU32BE(footerBytes, 60);
  if (diskType !== VHD_TYPE_FIXED && diskType !== VHD_TYPE_DYNAMIC) return false;

  const dataOffsetBig = readU64BE(footerBytes, 16);
  if (diskType === VHD_TYPE_FIXED) {
    if (dataOffsetBig !== 0xffff_ffff_ffff_ffffn) return false;
    const requiredLen = currentSize + 512;
    if (!Number.isSafeInteger(requiredLen) || fileSize < requiredLen) return false;
  } else {
    if (dataOffsetBig === 0xffff_ffff_ffff_ffffn) return false;
    const dataOffset = Number(dataOffsetBig);
    if (!Number.isSafeInteger(dataOffset) || dataOffset < 512) return false;
    if (dataOffset % 512 !== 0) return false;
    const end = dataOffset + 1024;
    if (!Number.isSafeInteger(end) || end > fileSize) return false;
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
  static async open(src: RandomAccessSource, minOutputBlockSize: number, signal?: AbortSignal): Promise<Qcow2> {
    if (src.size < 72) throw new Error("qcow2 header truncated");
    if (!Number.isSafeInteger(minOutputBlockSize) || minOutputBlockSize <= 0) throw new Error("invalid blockSizeBytes");
    const hdr72 = await src.readAt(0, 72);
    if (hdr72[0] !== 0x51 || hdr72[1] !== 0x46 || hdr72[2] !== 0x49 || hdr72[3] !== 0xfb) {
      throw new Error("invalid qcow2 magic");
    }
    const version = readU32BE(hdr72, 4);
    if (version !== 2 && version !== 3) throw new Error(`unsupported qcow2 version ${version}`);
    const hdr = version === 3 ? new Uint8Array(104) : hdr72;
    if (version === 3) {
      if (src.size < 104) throw new Error("qcow2 v3 header truncated");
      hdr.set(hdr72, 0);
      hdr.set(await src.readAt(72, 32), 72);
    }

    const headerLength = version === 3 ? readU32BE(hdr, 100) : 72;
    if (!Number.isSafeInteger(headerLength) || headerLength <= 0) throw new Error("invalid qcow2 header_length");
    if (version === 3 && headerLength < 104) throw new Error("invalid qcow2 header_length");
    if (src.size < headerLength) throw new Error("qcow2 header truncated");
    if (version === 3) {
      const incompatibleFeatures = readU64BE(hdr, 72);
      if (incompatibleFeatures !== 0n) throw new Error("qcow2 incompatible features unsupported");
      const refcountOrder = readU32BE(hdr, 96);
      if (refcountOrder !== 4) throw new Error("qcow2 refcount order unsupported");
    }

    const backingFileOffset = readU64BE(hdr, 8);
    const backingFileSize = readU32BE(hdr, 16);
    if (backingFileOffset !== 0n || backingFileSize !== 0) throw new Error("qcow2 backing files unsupported");
    const clusterBits = readU32BE(hdr, 20);
    if (clusterBits < 9 || clusterBits > 21) throw new Error(`unsupported qcow2 cluster_bits ${clusterBits}`);
    const logicalSize = Number(readU64BE(hdr, 24));
    if (!Number.isSafeInteger(logicalSize) || logicalSize <= 0) throw new Error("invalid qcow2 virtual size");
    if (logicalSize % 512 !== 0) throw new Error("qcow2 size not multiple of sector size");
    const cryptMethod = readU32BE(hdr, 32);
    if (cryptMethod !== 0) throw new Error("qcow2 encryption unsupported");
    const l1Size = readU32BE(hdr, 36);
    if (l1Size === 0) throw new Error("qcow2 l1_size is zero");
    const l1TableOffset = Number(readU64BE(hdr, 40));
    if (!Number.isSafeInteger(l1TableOffset) || l1TableOffset <= 0) throw new Error("invalid qcow2 l1_table_offset");
    const refcountTableOffset = Number(readU64BE(hdr, 48));
    if (!Number.isSafeInteger(refcountTableOffset) || refcountTableOffset <= 0) throw new Error("invalid qcow2 refcount_table_offset");
    const refcountTableClusters = readU32BE(hdr, 56);
    if (refcountTableClusters === 0) throw new Error("qcow2 refcount_table_clusters is zero");
    if (l1TableOffset < headerLength || refcountTableOffset < headerLength) throw new Error("qcow2 table overlaps header");
    const nbSnapshots = readU32BE(hdr, 60);
    const snapshotsOffset = readU64BE(hdr, 64);
    if (nbSnapshots !== 0 || snapshotsOffset !== 0n) throw new Error("qcow2 snapshots unsupported");

    const clusterSize = 1 << clusterBits;
    if (l1TableOffset % clusterSize !== 0) throw new Error("invalid qcow2 l1_table_offset");
    if (refcountTableOffset % clusterSize !== 0) throw new Error("invalid qcow2 refcount_table_offset");
    const l2Entries = clusterSize / 8;
    const totalClusters = divCeil(logicalSize, clusterSize);
    const requiredL1 = divCeil(totalClusters, l2Entries);
    if (l1Size < requiredL1) throw new Error("qcow2 l1 table too small");

    const refcountTableBytesBig = BigInt(refcountTableClusters) * BigInt(clusterSize);
    if (refcountTableBytesBig > BigInt(QCOW2_MAX_TABLE_BYTES)) throw new Error("qcow2 refcount table too large");
    const refcountTableBytes = Number(refcountTableBytesBig);
    if (!Number.isSafeInteger(refcountTableBytes) || refcountTableBytes <= 0) throw new Error("qcow2 refcount table too large");

    const l1Bytes = requiredL1 * 8;
    if (!Number.isSafeInteger(l1Bytes) || l1Bytes > QCOW2_MAX_TABLE_BYTES) {
      throw new Error("qcow2 l1 table too large");
    }
    const clusterOffsetsBytes = totalClusters * 8;
    if (!Number.isSafeInteger(clusterOffsetsBytes) || clusterOffsetsBytes > QCOW2_MAX_CLUSTER_OFFSETS_BYTES) {
      throw new Error("qcow2 too many clusters");
    }

    const l1End = l1TableOffset + l1Bytes;
    if (!Number.isSafeInteger(l1End) || l1End > src.size) throw new Error("qcow2 l1 table truncated");
    const refcountEnd = refcountTableOffset + refcountTableBytes;
    if (!Number.isSafeInteger(refcountEnd) || refcountEnd > src.size) throw new Error("qcow2 refcount table truncated");
    if (rangesOverlap(l1TableOffset, l1End, refcountTableOffset, refcountEnd)) {
      throw new Error("qcow2 metadata tables overlap");
    }

    // Fail fast if the output AeroSparse allocation table would exceed the runtime cap (64MiB).
    // This avoids allocating qcow2 metadata structures that are proportional to the virtual disk
    // size when we cannot produce an output sparse disk anyway.
    const outBlockSize = nextPow2(Math.max(minOutputBlockSize, clusterSize));
    const outTableEntries = divCeil(logicalSize, outBlockSize);
    if (outTableEntries > AEROSPAR_MAX_TABLE_ENTRIES) {
      throw new Error("aerosparse allocation table too large");
    }

    function validateClusterNotOverlappingMetadata(off: number): void {
      const end = off + clusterSize;
      if (!Number.isSafeInteger(end) || end <= 0) throw new Error("invalid qcow2 cluster offset");
      if (off < headerLength) throw new Error("qcow2 cluster overlaps header");
      if (rangesOverlap(off, end, l1TableOffset, l1End)) throw new Error("qcow2 cluster overlaps l1 table");
      if (rangesOverlap(off, end, refcountTableOffset, refcountEnd)) throw new Error("qcow2 cluster overlaps refcount table");
    }

    // Track all qcow2 metadata clusters (L2 tables and refcount blocks) to detect overlaps and to
    // reject data clusters that point into metadata regions.
    const metadataClusters = new Set<number>();
    const lowMask = (1n << BigInt(clusterBits)) - 1n;

    // Parse the refcount table to record refcount block cluster offsets (even though conversion
    // doesn't currently interpret refcount contents). This matches Rust's corruption hardening and
    // prevents treating refcount blocks as guest data when images are malformed.
    const refcountChunkSize = 8 * 1024 * 1024; // 8 MiB
    const refcountBufLen = Math.min(refcountChunkSize, refcountTableBytes);
    let refcountRemaining = refcountTableBytes;
    let refcountOff = refcountTableOffset;
    while (refcountRemaining > 0) {
      if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
      const len = Math.min(refcountRemaining, refcountBufLen);
      const chunk = await src.readAt(refcountOff, len);
      for (let i = 0; i < len; i += 8) {
        const entry = readU64BE(chunk, i);
        if (entry === 0n) continue;
        if ((entry & QCOW2_OFLAG_COMPRESSED) !== 0n) throw new Error("qcow2 compressed refcount block unsupported");
        if ((entry & lowMask) !== 0n) throw new Error("qcow2 unaligned refcount block entry");
        const blockOffBig = entry & QCOW2_OFFSET_MASK;
        if (blockOffBig === 0n) throw new Error("qcow2 invalid refcount block entry");
        const blockOff = Number(blockOffBig);
        if (!Number.isSafeInteger(blockOff) || blockOff <= 0) throw new Error("qcow2 invalid refcount block entry");
        if (blockOff % clusterSize !== 0) throw new Error("qcow2 invalid refcount block entry");
        validateClusterNotOverlappingMetadata(blockOff);
        if (metadataClusters.has(blockOff)) throw new Error("qcow2 metadata clusters overlap");
        if (metadataClusters.size >= QCOW2_MAX_METADATA_CLUSTERS) throw new Error("qcow2 too many metadata clusters");
        metadataClusters.add(blockOff);
        const end = blockOff + clusterSize;
        if (!Number.isSafeInteger(end) || end > src.size) throw new Error("qcow2 refcount block truncated");
      }
      refcountOff += len;
      refcountRemaining -= len;
    }

    const clusterOffsets = new Float64Array(totalClusters);

    // Read the L1 table in chunks to avoid allocating `l1Bytes` all at once (can be up to 128MiB).
    const l1ChunkSize = 8 * 1024 * 1024; // 8 MiB
    const l1BufLen = Math.min(l1ChunkSize, l1Bytes);
    let l1Remaining = l1Bytes;
    let l1Off = l1TableOffset;
    let l1Index = 0;
    while (l1Remaining > 0) {
      if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
      const len = Math.min(l1Remaining, l1BufLen);
      const chunk = await src.readAt(l1Off, len);

      for (let off = 0; off < len; off += 8) {
        const rawL1Entry = readU64BE(chunk, off);
        if (rawL1Entry !== 0n) {
          if ((rawL1Entry & QCOW2_OFLAG_COMPRESSED) !== 0n) throw new Error("qcow2 compressed l1 unsupported");
          if ((rawL1Entry & lowMask) !== 0n) throw new Error("qcow2 unaligned l1 entry");

          const l2OffBig = rawL1Entry & QCOW2_OFFSET_MASK;
          const l2Off = Number(l2OffBig);
          if (!Number.isSafeInteger(l2Off) || l2Off <= 0) throw new Error("invalid qcow2 l2 table offset");
          if (l2Off % clusterSize !== 0) throw new Error("invalid qcow2 l2 table offset");
          validateClusterNotOverlappingMetadata(l2Off);
          if (metadataClusters.has(l2Off)) throw new Error("qcow2 metadata clusters overlap");
          if (metadataClusters.size >= QCOW2_MAX_METADATA_CLUSTERS) throw new Error("qcow2 too many metadata clusters");
          metadataClusters.add(l2Off);

          const l2End = l2Off + clusterSize;
          if (!Number.isSafeInteger(l2End) || l2End > src.size) throw new Error("qcow2 l2 table truncated");
          const l2 = await src.readAt(l2Off, clusterSize);

          for (let l2Index = 0; l2Index < l2Entries; l2Index++) {
            const clusterIndex = l1Index * l2Entries + l2Index;
            if (clusterIndex >= totalClusters) break;
            const val = readU64BE(l2, l2Index * 8);
            if (val === 0n) continue;
            if ((val & QCOW2_OFLAG_COMPRESSED) !== 0n) throw new Error("qcow2 compressed clusters unsupported");
            if ((val & QCOW2_OFLAG_ZERO) !== 0n) {
              // For "zero clusters", the offset bits must be zero and only the zero flag may be set in
              // the low (alignment) bits.
              if ((val & lowMask) !== QCOW2_OFLAG_ZERO) throw new Error("qcow2 invalid zero cluster entry");
              if ((val & QCOW2_OFFSET_MASK) !== 0n) throw new Error("qcow2 invalid zero cluster entry");
              continue;
            }
            if ((val & lowMask) !== 0n) throw new Error("qcow2 unaligned l2 entry");

            const dataOffBig = val & QCOW2_OFFSET_MASK;
            if (dataOffBig === 0n) throw new Error("invalid qcow2 data offset");
            const dataOff = Number(dataOffBig);
            if (!Number.isSafeInteger(dataOff) || dataOff <= 0) throw new Error("invalid qcow2 data offset");
            if (dataOff % clusterSize !== 0) throw new Error("invalid qcow2 data offset");
            validateClusterNotOverlappingMetadata(dataOff);
            if (metadataClusters.has(dataOff)) throw new Error("qcow2 data cluster overlaps metadata");
            const guestOff = clusterIndex * clusterSize;
            const copyLen = Math.min(clusterSize, logicalSize - guestOff);
            const dataEnd = dataOff + copyLen;
            if (!Number.isSafeInteger(dataEnd) || dataEnd > src.size) throw new Error("qcow2 data cluster truncated");
            clusterOffsets[clusterIndex] = dataOff;
          }
        }

        l1Index++;
      }

      l1Off += len;
      l1Remaining -= len;
    }

    return new Qcow2(logicalSize, clusterSize, clusterOffsets);
  }

  public readonly logicalSize: number;
  public readonly clusterSize: number;
  public readonly clusterOffsets: Float64Array;

  private constructor(logicalSize: number, clusterSize: number, clusterOffsets: Float64Array) {
    this.logicalSize = logicalSize;
    this.clusterSize = clusterSize;
    this.clusterOffsets = clusterOffsets;
  }
}

class VhdFooter {
  static parse(footerBytes: Uint8Array): VhdFooter {
    if (footerBytes.byteLength !== 512) throw new Error("invalid VHD footer length");
    if (ascii(footerBytes.subarray(0, 8)) !== "conectix") throw new Error("missing VHD footer cookie");

    // VHD footers use big-endian fields and have a fixed format version.
    // https://learn.microsoft.com/en-us/windows/win32/fileio/vhd-specification
    const fileFormatVersion = readU32BE(footerBytes, 12);
    if (fileFormatVersion !== 0x0001_0000) throw new Error("invalid VHD file_format_version");

    const storedChecksum = readU32BE(footerBytes, 64);
    const copy = footerBytes.slice();
    copy.fill(0, 64, 68);
    let sum = 0;
    for (const b of copy) sum = (sum + b) >>> 0;
    const expected = (~sum) >>> 0;
    if (expected !== storedChecksum) throw new Error("VHD footer checksum mismatch");

    const dataOffsetBig = readU64BE(footerBytes, 16);
    const currentSize = Number(readU64BE(footerBytes, 48));
    const diskType = readU32BE(footerBytes, 60);
    if (!Number.isSafeInteger(currentSize) || currentSize <= 0) throw new Error("invalid VHD current_size");
    if (currentSize % 512 !== 0) throw new Error("invalid VHD current_size");

    if (diskType !== VHD_TYPE_FIXED && diskType !== VHD_TYPE_DYNAMIC) {
      throw new Error("unsupported VHD disk_type");
    }
    let dataOffset: number;
    if (diskType === VHD_TYPE_FIXED) {
      // Use bigint to avoid precision loss for u64::MAX (which is not exactly representable as a JS number).
      if (dataOffsetBig !== 0xffff_ffff_ffff_ffffn) throw new Error("invalid VHD data_offset");
      dataOffset = Number(dataOffsetBig);
    } else {
      if (dataOffsetBig === 0xffff_ffff_ffff_ffffn) throw new Error("invalid VHD data_offset");
      if (dataOffsetBig > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("invalid VHD data_offset");
      dataOffset = Number(dataOffsetBig);
      if (!Number.isSafeInteger(dataOffset) || dataOffset <= 0) throw new Error("invalid VHD data_offset");
      if (dataOffset % 512 !== 0) throw new Error("invalid VHD data_offset");
      if (dataOffset < 512) throw new Error("invalid VHD data_offset");
    }
    return new VhdFooter(dataOffset, currentSize, diskType, footerBytes.slice());
  }

  static async read(src: RandomAccessSource): Promise<VhdFooter> {
    if (src.size < 512) throw new Error("VHD too small");
    if (src.size % 512 !== 0) throw new Error("VHD file length misaligned");
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

    const dataOffset = readU64BE(dh, 8);
    if (dataOffset !== 0xffff_ffff_ffff_ffffn) throw new Error("invalid VHD dynamic header data_offset");

    const tableOffset = Number(readU64BE(dh, 16));
    if (!Number.isSafeInteger(tableOffset) || tableOffset <= 0) throw new Error("invalid VHD BAT offset");
    if (tableOffset % 512 !== 0) throw new Error("invalid VHD BAT offset");

    const headerVersion = readU32BE(dh, 24);
    if (headerVersion !== 0x0001_0000) throw new Error("invalid VHD dynamic header version");

    const maxTableEntries = readU32BE(dh, 28);
    const blockSize = readU32BE(dh, 32);
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
    const bytes = entries * 4;
    if (!Number.isSafeInteger(bytes) || bytes > VHD_MAX_BAT_BYTES) throw new Error("VHD BAT too large");
    const out = new Uint32Array(entries);
    // Avoid allocating `bytes` twice (batBytes + Uint32Array) for large BATs. Read in chunks and
    // decode entries incrementally.
    const CHUNK_BYTES = 8 * 1024 * 1024; // 8 MiB (must be a multiple of 4)
    let off = 0;
    while (off < bytes) {
      const len = Math.min(CHUNK_BYTES, bytes - off);
      const chunk = await src.readAt(tableOffset + off, len);
      for (let i = 0; i < len; i += 4) {
        out[(off + i) >>> 2] = readU32BE(chunk, i);
      }
      off += len;
    }
    return new VhdBat(out);
  }

  public readonly entries: Uint32Array;

  private constructor(entries: Uint32Array) {
    this.entries = entries;
  }
}

function vhdBitmapBit(bitmap: Uint8Array, sector: number): boolean {
  const b = bitmap[Math.floor(sector / 8)];
  const bit = 7 - (sector % 8);
  return ((b >> bit) & 1) !== 0;
}

function rangesOverlap(aStart: number, aEnd: number, bStart: number, bEnd: number): boolean {
  return aStart < bEnd && bStart < aEnd;
}
