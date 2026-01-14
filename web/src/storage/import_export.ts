/**
 * Disk image import/export primitives.
 *
 * These operate on the selected storage backend (OPFS preferred, IDB fallback).
 * Heavy operations (streaming read/write, checksums, compression) are intended
 * to run inside a dedicated Worker.
 */

import { crc32Final, crc32Init, crc32ToHex, crc32Update } from "./crc32.ts";
import { CHUNKED_DISK_CHUNK_SIZE, RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes.ts";
import { OPFS_DISKS_PATH, idbReq, idbTxDone, openDiskManagerDb, opfsGetDir, opfsGetDisksDir } from "./metadata.ts";

/**
 * Chunk sizing notes (different subsystems use different units):
 *
 * - **Remote Range streaming** (`RemoteStreamingDisk`) defaults to **1 MiB** blocks.
 * - **Chunked disk-image delivery** (manifest + chunk objects; see `docs/18-chunked-disk-image-format.md`)
 *   defaults to **4 MiB** chunks.
 *
 * This file's constants are browser-local implementation details:
 *
 * - `IDB_CHUNK_SIZE` controls the record size in the IndexedDB `chunks` store (runtime fallback when
 *   OPFS is unavailable). Keeping this at 4 MiB balances transaction overhead vs. per-record size,
 *   and aligns with the default chunked-delivery chunk size to avoid unnecessary re-chunking.
 * - `EXPORT_CHUNK_SIZE` controls the streaming unit used for CRC32/checksum and message passing
 *   during import/export flows; smaller chunks keep memory bounded and provide smoother progress.
 */
export const IDB_CHUNK_SIZE = CHUNKED_DISK_CHUNK_SIZE;
export const EXPORT_CHUNK_SIZE = RANGE_STREAM_CHUNK_SIZE;
const MAX_IMPORT_CHECKSUM_BYTES = 32 * 1024 * 1024;

export type ImportProgress = {
  phase: "import" | "export" | "create" | "resize";
  processedBytes: number;
  totalBytes?: number;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function report(
  onProgress: ((p: ImportProgress) => void) | undefined,
  payload: ImportProgress,
): void {
  if (!onProgress) return;
  try {
    onProgress(payload);
  } catch (err) {
    // Progress callbacks must never crash the worker operation.
  }
}

export async function opfsGetDiskFileHandle(
  fileName: string,
  options?: { create?: boolean; dirPath?: string },
): Promise<FileSystemFileHandle> {
  const dirPath = options?.dirPath ?? OPFS_DISKS_PATH;
  const disksDir = dirPath === OPFS_DISKS_PATH ? await opfsGetDisksDir() : await opfsGetDir(dirPath, { create: options?.create ?? false });
  return disksDir.getFileHandle(fileName, { create: options?.create ?? false });
}

/**
 * @param {string} fileName
 * @returns {Promise<number>}
 */
export async function opfsGetDiskSizeBytes(fileName: string, dirPath?: string): Promise<number> {
  const file = await (await opfsGetDiskFileHandle(fileName, { create: false, dirPath })).getFile();
  return file.size;
}

/**
 * @param {string} fileName
 * @param {number} sizeBytes
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ sizeBytes: number; checksumCrc32: string | undefined }>}
 */
export async function opfsCreateBlankDisk(
  fileName: string,
  sizeBytes: number,
  onProgress: ((p: ImportProgress) => void) | undefined,
  dirPath?: string,
): Promise<{ sizeBytes: number; checksumCrc32: string | undefined }> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true, dirPath });
  let writable: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    writable = await handle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
    truncateFallback = true;
  }
  if (truncateFallback) {
    // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
    // unsupported. Ensure we truncate before resizing so old bytes cannot linger.
    try {
      await writable.truncate(0);
    } catch {
      // ignore
    }
  }
  report(onProgress, { phase: "create", processedBytes: 0, totalBytes: sizeBytes });
  try {
    await writable.truncate(sizeBytes);
    await writable.close();
  } catch (err) {
    try {
      await writable.abort(err);
    } catch {
      // ignore abort failures
    }
    throw err;
  }
  report(onProgress, { phase: "create", processedBytes: sizeBytes, totalBytes: sizeBytes });

  // We do not compute a checksum for large sparse files (too expensive).
  if (sizeBytes > MAX_IMPORT_CHECKSUM_BYTES) return { sizeBytes, checksumCrc32: undefined };

  // Cheap checksum for small blank images.
  let crc = crc32Init();
  const zeroChunk = new Uint8Array(Math.min(EXPORT_CHUNK_SIZE, sizeBytes));
  let remaining = sizeBytes;
  while (remaining > 0) {
    const slice = zeroChunk.subarray(0, Math.min(zeroChunk.length, remaining));
    crc = crc32Update(crc, slice);
    remaining -= slice.length;
  }
  const checksum = crc32ToHex(crc32Final(crc));
  return { sizeBytes, checksumCrc32: checksum };
}

/**
 * @param {string} fileName
 * @param {File} file
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ sizeBytes: number; checksumCrc32: string }>}
 */
export async function opfsImportFile(
  fileName: string,
  file: File,
  onProgress: ((p: ImportProgress) => void) | undefined,
  dirPath?: string,
): Promise<{ sizeBytes: number; checksumCrc32: string | undefined }> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true, dirPath });
  let writable: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    writable = await handle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
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
  const reader = file.stream().getReader();

  const shouldChecksum = file.size <= MAX_IMPORT_CHECKSUM_BYTES;
  let crc = crc32Init();
  let processed = 0;

  try {
    report(onProgress, { phase: "import", processedBytes: 0, totalBytes: file.size });
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = value;
      await writable.write(chunk);
      processed += chunk.byteLength;
      if (shouldChecksum) {
        crc = crc32Update(crc, chunk);
      }
      report(onProgress, { phase: "import", processedBytes: processed, totalBytes: file.size });
    }

    await writable.close();
  } catch (err) {
    try {
      await reader.cancel(err);
    } catch {
      // ignore
    }
    try {
      await writable.abort(err);
    } catch {
      // ignore abort failures
    }
    throw err;
  }
  if (!shouldChecksum) {
    return { sizeBytes: file.size, checksumCrc32: undefined };
  }
  const checksumCrc32 = crc32ToHex(crc32Final(crc));
  return { sizeBytes: file.size, checksumCrc32 };
}

/**
 * @param {string} fileName
 * @returns {Promise<void>}
 */
export async function opfsDeleteDisk(fileName: string, dirPath?: string): Promise<void> {
  const path = dirPath ?? OPFS_DISKS_PATH;
  const disksDir = path === OPFS_DISKS_PATH ? await opfsGetDisksDir() : await opfsGetDir(path, { create: false });
  try {
    await disksDir.removeEntry(fileName);
  } catch (err) {
    // ignore NotFoundError
  }
}

/**
 * @param {string} fileName
 * @param {number} newSizeBytes
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<void>}
 */
export async function opfsResizeDisk(
  fileName: string,
  newSizeBytes: number,
  onProgress: ((p: ImportProgress) => void) | undefined,
  dirPath?: string,
): Promise<void> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: false, dirPath });
  let writable: FileSystemWritableFileStream;
  try {
    writable = await handle.createWritable({ keepExistingData: true });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
  }
  report(onProgress, { phase: "resize", processedBytes: 0, totalBytes: newSizeBytes });
  try {
    await writable.truncate(newSizeBytes);
    await writable.close();
  } catch (err) {
    try {
      await writable.abort(err);
    } catch {
      // ignore abort failures
    }
    throw err;
  }
  report(onProgress, { phase: "resize", processedBytes: newSizeBytes, totalBytes: newSizeBytes });
}

/**
 * @param {ReadableStream<Uint8Array>} stream
 * @param {MessagePort} port
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @param {number | undefined} totalBytes
 * @param {"export"} phase
 * @returns {Promise<{ checksumCrc32: string }>}
 */
export async function streamToPortWithChecksum(
  stream: ReadableStream<Uint8Array>,
  port: MessagePort,
  onProgress: ((p: ImportProgress) => void) | undefined,
  totalBytes: number | undefined,
  phase: "export",
): Promise<{ checksumCrc32: string }> {
  const reader = stream.getReader();
  let crc = crc32Init();
  let processed = 0;
  report(onProgress, { phase, processedBytes: 0, totalBytes });
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    const chunk = value;
    processed += chunk.byteLength;
    crc = crc32Update(crc, chunk);
    // Transfer the underlying buffer where possible to avoid copies.
    port.postMessage({ type: "chunk", chunk }, [chunk.buffer]);
    report(onProgress, { phase, processedBytes: processed, totalBytes });
  }
  const checksumCrc32 = crc32ToHex(crc32Final(crc));
  port.postMessage({ type: "done", checksumCrc32 });
  return { checksumCrc32 };
}

/**
 * @param {string} fileName
 * @param {MessagePort} port
 * @param {{ gzip?: boolean } | undefined} options
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ checksumCrc32: string }>}
 */
export async function opfsExportToPort(
  fileName: string,
  port: MessagePort,
  options: { gzip?: boolean } | undefined,
  onProgress: ((p: ImportProgress) => void) | undefined,
  dirPath?: string,
): Promise<{ checksumCrc32: string }> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: false, dirPath });
  const file = await handle.getFile();
  let stream = file.stream() as ReadableStream<Uint8Array>;

  if (options?.gzip) {
    if (typeof CompressionStream === "undefined") {
      throw new Error("CompressionStream not supported in this browser");
    }
    stream = stream.pipeThrough(new CompressionStream("gzip") as unknown as TransformStream<Uint8Array, Uint8Array>);
    // When compressing, we do not know final size ahead of time.
    return streamToPortWithChecksum(stream, port, onProgress, undefined, "export");
  }

  return streamToPortWithChecksum(stream, port, onProgress, file.size, "export");
}

/**
 * @param {IDBDatabase} db
 * @param {string} diskId
 * @param {number} index
 * @param {ArrayBuffer} data
 * @returns {Promise<void>}
 */
async function idbPutChunks(db: IDBDatabase, diskId: string, entries: Array<[number, ArrayBuffer]>): Promise<void> {
  if (entries.length === 0) return;
  const tx = db.transaction(["chunks"], "readwrite");
  const done = idbTxDone(tx);
  const store = tx.objectStore("chunks");
  for (const [index, data] of entries) {
    store.put({ id: diskId, index, data });
  }
  await done;
}

function safeDataFromIdbChunkRecord(rec: unknown, diskId: string, index: number): ArrayBuffer | undefined {
  if (!isRecord(rec)) return undefined;
  const id = hasOwn(rec, "id") ? rec.id : undefined;
  const idx = hasOwn(rec, "index") ? rec.index : undefined;
  if (id !== diskId || idx !== index) return undefined;
  if (!hasOwn(rec, "data")) return undefined;
  const dataRaw = rec.data;
  const dataAny = dataRaw as any;
  if (dataAny instanceof ArrayBuffer) return dataAny;
  // Legacy/foreign implementations may store Uint8Array instead of ArrayBuffer.
  if (dataAny instanceof Uint8Array) {
    if (
      dataAny.buffer instanceof ArrayBuffer &&
      dataAny.byteOffset === 0 &&
      dataAny.byteLength === dataAny.buffer.byteLength
    ) {
      return dataAny.buffer;
    }
    return dataAny.slice().buffer;
  }
  return undefined;
}

/**
 * @param {IDBDatabase} db
 * @param {string} diskId
 * @param {number} index
 * @returns {Promise<ArrayBuffer | undefined>}
 */
async function idbGetChunk(db: IDBDatabase, diskId: string, index: number): Promise<ArrayBuffer | undefined> {
  const tx = db.transaction(["chunks"], "readonly");
  const store = tx.objectStore("chunks");
  const done = idbTxDone(tx);
  const rec = (await idbReq(store.get([diskId, index]))) as unknown;
  await done;
  return safeDataFromIdbChunkRecord(rec, diskId, index);
}

/**
 * @param {IDBDatabase} db
 * @param {string} diskId
 * @returns {Promise<void>}
 */
export async function idbDeleteDiskData(db: IDBDatabase, diskId: string): Promise<void> {
  const tx = db.transaction(["chunks"], "readwrite");
  const index = tx.objectStore("chunks").index("by_id");
  const range = IDBKeyRange.only(diskId);
  await new Promise<void>((resolve, reject) => {
    const req = index.openCursor(range);
    req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return resolve(undefined);
      cursor.delete();
      cursor.continue();
    };
  });
  await new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve(undefined);
    tx.onerror = () => reject(tx.error || new Error("IndexedDB delete tx failed"));
    tx.onabort = () => reject(tx.error || new Error("IndexedDB delete tx aborted"));
  });
}

/**
 * @param {string} diskId
 * @param {number} sizeBytes
 * @returns {Promise<void>}
 */
export async function idbCreateBlankDisk(diskId: string, sizeBytes: number): Promise<void> {
  // Sparse: do nothing. Absence of chunks means zeros on read/export.
  // The size is stored in metadata by the disk worker.
  void diskId;
  void sizeBytes;
}

/**
 * @param {string} diskId
 * @param {File} file
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ sizeBytes: number; checksumCrc32: string }>}
 */
export async function idbImportFile(
  diskId: string,
  file: File,
  onProgress: ((p: ImportProgress) => void) | undefined,
): Promise<{ sizeBytes: number; checksumCrc32: string | undefined }> {
  const db = await openDiskManagerDb();
  let processed = 0;
  const shouldChecksum = file.size <= MAX_IMPORT_CHECKSUM_BYTES;
  let crc = crc32Init();
  let chunkIndex = 0;
  const putBatch: Array<[number, ArrayBuffer]> = [];
  const PUT_BATCH_ENTRIES = 8;

  report(onProgress, { phase: "import", processedBytes: 0, totalBytes: file.size });

  const reader = file.stream().getReader();
  let pending: Uint8Array[] = [];
  let pendingBytes = 0;

  const containsNonZero = (buf: Uint8Array): boolean => {
    for (let i = 0; i < buf.length; i++) {
      if (buf[i] !== 0) return true;
    }
    return false;
  };

  const flushPutBatch = async () => {
    if (putBatch.length === 0) return;
    await idbPutChunks(db, diskId, putBatch);
    putBatch.length = 0;
  };

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const part = value;
      pending.push(part);
      pendingBytes += part.byteLength;

      while (pendingBytes >= IDB_CHUNK_SIZE) {
        const chunk = new Uint8Array(IDB_CHUNK_SIZE);
        let offset = 0;
        let anyNonZero = false;

        while (offset < chunk.byteLength) {
          const head = pending[0];
          const take = Math.min(head.byteLength, chunk.byteLength - offset);
          const slice = head.subarray(0, take);
          chunk.set(slice, offset);
          if (!anyNonZero && containsNonZero(slice)) anyNonZero = true;
          offset += take;
          if (take === head.byteLength) {
            pending.shift();
          } else {
            pending[0] = head.subarray(take);
          }
        }

        pendingBytes -= IDB_CHUNK_SIZE;
        const index = chunkIndex++;
        processed += chunk.byteLength;
        if (shouldChecksum) crc = crc32Update(crc, chunk);
        if (anyNonZero) putBatch.push([index, chunk.buffer]);
        report(onProgress, { phase: "import", processedBytes: processed, totalBytes: file.size });

        if (putBatch.length >= PUT_BATCH_ENTRIES) {
          await flushPutBatch();
        }
      }
    }

    if (pendingBytes > 0) {
      const chunk = new Uint8Array(pendingBytes);
      let offset = 0;
      let anyNonZero = false;
      for (const part of pending) {
        chunk.set(part, offset);
        if (!anyNonZero && containsNonZero(part)) anyNonZero = true;
        offset += part.byteLength;
      }

      const index = chunkIndex++;
      processed += chunk.byteLength;
      if (shouldChecksum) crc = crc32Update(crc, chunk);
      if (anyNonZero) putBatch.push([index, chunk.buffer]);
    }

    await flushPutBatch();
  } finally {
    db.close();
  }

  report(onProgress, { phase: "import", processedBytes: file.size, totalBytes: file.size });
  if (!shouldChecksum) {
    return { sizeBytes: file.size, checksumCrc32: undefined };
  }
  const checksumCrc32 = crc32ToHex(crc32Final(crc));
  return { sizeBytes: file.size, checksumCrc32 };
}

/**
 * @param {string} diskId
 * @param {number} sizeBytes
 * @param {MessagePort} port
 * @param {{ gzip?: boolean } | undefined} options
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ checksumCrc32: string }>}
 */
export async function idbExportToPort(
  diskId: string,
  sizeBytes: number,
  port: MessagePort,
  options: { gzip?: boolean } | undefined,
  onProgress: ((p: ImportProgress) => void) | undefined,
): Promise<{ checksumCrc32: string }> {
  const db = await openDiskManagerDb();
  try {
    if (options?.gzip) {
      if (typeof CompressionStream === "undefined") {
        throw new Error("CompressionStream not supported in this browser");
      }

      let index = 0;
      const totalChunks = Math.ceil(sizeBytes / IDB_CHUNK_SIZE);
      let processedRaw = 0;
      report(onProgress, { phase: "export", processedBytes: 0, totalBytes: sizeBytes });

      const rawStream = new ReadableStream<Uint8Array>({
        async pull(controller) {
          if (index >= totalChunks) {
            controller.close();
            return;
          }
          const buf = await idbGetChunk(db, diskId, index);
          const remaining = sizeBytes - index * IDB_CHUNK_SIZE;
          const outLen = Math.min(IDB_CHUNK_SIZE, remaining);

          let chunk: Uint8Array;
          if (!buf) {
            chunk = new Uint8Array(outLen);
          } else {
            chunk = new Uint8Array(buf, 0, Math.min(outLen, buf.byteLength));
            if (chunk.byteLength < outLen) {
              const padded = new Uint8Array(outLen);
              padded.set(chunk);
              chunk = padded;
            }
          }

          processedRaw += chunk.byteLength;
          report(onProgress, { phase: "export", processedBytes: processedRaw, totalBytes: sizeBytes });
          index++;
          controller.enqueue(chunk);
        },
      });

      const stream = rawStream.pipeThrough(new CompressionStream("gzip") as unknown as TransformStream<Uint8Array, Uint8Array>);
      // Report raw (pre-compression) progress, but checksum the actual stream output.
      return await streamToPortWithChecksum(stream, port, undefined, undefined, "export");
    }

    const tx = db.transaction(["chunks"], "readonly");
    const store = tx.objectStore("chunks");
    const txDone = idbTxDone(tx);

    let crc = crc32Init();
    let processed = 0;
    report(onProgress, { phase: "export", processedBytes: 0, totalBytes: sizeBytes });

    const totalChunks = Math.ceil(sizeBytes / IDB_CHUNK_SIZE);
    for (let index = 0; index < totalChunks; index++) {
      const rec = (await idbReq(store.get([diskId, index]))) as unknown;
      const buf = safeDataFromIdbChunkRecord(rec, diskId, index);
      const remaining = sizeBytes - index * IDB_CHUNK_SIZE;
      const outLen = Math.min(IDB_CHUNK_SIZE, remaining);

      let chunk: Uint8Array;
      if (!buf) {
        chunk = new Uint8Array(outLen);
      } else {
        chunk = new Uint8Array(buf, 0, Math.min(outLen, buf.byteLength));
        if (chunk.byteLength < outLen) {
          const padded = new Uint8Array(outLen);
          padded.set(chunk);
          chunk = padded;
        }
      }

      processed += chunk.byteLength;
      crc = crc32Update(crc, chunk);
      port.postMessage({ type: "chunk", chunk }, [chunk.buffer]);
      report(onProgress, { phase: "export", processedBytes: processed, totalBytes: sizeBytes });
    }

    await txDone;

    const checksumCrc32 = crc32ToHex(crc32Final(crc));
    port.postMessage({ type: "done", checksumCrc32 });
    return { checksumCrc32 };
  } finally {
    db.close();
  }
}

/**
 * @param {string} diskId
 * @param {number} oldSizeBytes
 * @param {number} newSizeBytes
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<void>}
 */
export async function idbResizeDisk(
  diskId: string,
  oldSizeBytes: number,
  newSizeBytes: number,
  onProgress: ((p: ImportProgress) => void) | undefined,
): Promise<void> {
  report(onProgress, { phase: "resize", processedBytes: 0, totalBytes: newSizeBytes });
  if (newSizeBytes >= oldSizeBytes) {
    report(onProgress, { phase: "resize", processedBytes: newSizeBytes, totalBytes: newSizeBytes });
    return;
  }

  const db = await openDiskManagerDb();
  const keepChunks = Math.ceil(newSizeBytes / IDB_CHUNK_SIZE);

  const tx = db.transaction(["chunks"], "readwrite");
  const index = tx.objectStore("chunks").index("by_id");
  const range = IDBKeyRange.only(diskId);
  try {
    await new Promise<void>((resolve, reject) => {
      const req = index.openCursor(range);
      req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
      req.onsuccess = () => {
        const cursor = req.result;
        if (!cursor) return resolve(undefined);
        const value = cursor.value as unknown;
        if (isRecord(value) && hasOwn(value, "index")) {
          const idx = value.index;
          if (typeof idx === "number" && Number.isInteger(idx) && idx >= keepChunks) {
            cursor.delete();
          }
        }
        cursor.continue();
      };
    });

    await new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve(undefined);
      tx.onerror = () => reject(tx.error || new Error("IndexedDB resize tx failed"));
      tx.onabort = () => reject(tx.error || new Error("IndexedDB resize tx aborted"));
    });
  } finally {
    db.close();
  }
  report(onProgress, { phase: "resize", processedBytes: newSizeBytes, totalBytes: newSizeBytes });
}
