// @ts-check

/**
 * Disk image import/export primitives.
 *
 * These operate on the selected storage backend (OPFS preferred, IDB fallback).
 * Heavy operations (streaming read/write, checksums, compression) are intended
 * to run inside a dedicated Worker.
 */

import { crc32Final, crc32Init, crc32ToHex, crc32Update } from "./crc32.ts";
import { openDiskManagerDb, opfsGetDisksDir } from "./metadata.ts";

export const IDB_CHUNK_SIZE = 4 * 1024 * 1024;
export const EXPORT_CHUNK_SIZE = 1024 * 1024;

/**
 * @typedef ImportProgress
 * @property {"import" | "export" | "create" | "resize"} phase
 * @property {number} processedBytes
 * @property {number | undefined} totalBytes
 */

/**
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @param {ImportProgress} payload
 */
function report(onProgress, payload) {
  if (!onProgress) return;
  try {
    onProgress(payload);
  } catch (err) {
    // Progress callbacks must never crash the worker operation.
  }
}

/**
 * @param {string} fileName
 * @param {{ create?: boolean } | undefined} options
 * @returns {Promise<FileSystemFileHandle>}
 */
export async function opfsGetDiskFileHandle(fileName, options) {
  const disksDir = await opfsGetDisksDir();
  return disksDir.getFileHandle(fileName, { create: options?.create ?? false });
}

/**
 * @param {string} fileName
 * @returns {Promise<number>}
 */
export async function opfsGetDiskSizeBytes(fileName) {
  const file = await (await opfsGetDiskFileHandle(fileName, { create: false })).getFile();
  return file.size;
}

/**
 * @param {string} fileName
 * @param {number} sizeBytes
 * @param {(p: ImportProgress) => void | undefined} onProgress
 * @returns {Promise<{ sizeBytes: number; checksumCrc32: string | undefined }>}
 */
export async function opfsCreateBlankDisk(fileName, sizeBytes, onProgress) {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  report(onProgress, { phase: "create", processedBytes: 0, totalBytes: sizeBytes });
  await writable.truncate(sizeBytes);
  await writable.close();
  report(onProgress, { phase: "create", processedBytes: sizeBytes, totalBytes: sizeBytes });

  // We do not compute a checksum for large sparse files (too expensive).
  const MAX_CHECKSUM_BYTES = 32 * 1024 * 1024;
  if (sizeBytes > MAX_CHECKSUM_BYTES) return { sizeBytes, checksumCrc32: undefined };

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
export async function opfsImportFile(fileName, file, onProgress) {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  const reader = file.stream().getReader();

  let crc = crc32Init();
  let processed = 0;

  report(onProgress, { phase: "import", processedBytes: 0, totalBytes: file.size });
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    const chunk = value;
    await writable.write(chunk);
    processed += chunk.byteLength;
    crc = crc32Update(crc, chunk);
    report(onProgress, { phase: "import", processedBytes: processed, totalBytes: file.size });
  }

  await writable.close();
  const checksum = crc32ToHex(crc32Final(crc));
  return { sizeBytes: file.size, checksumCrc32: checksum };
}

/**
 * @param {string} fileName
 * @returns {Promise<void>}
 */
export async function opfsDeleteDisk(fileName) {
  const disksDir = await opfsGetDisksDir();
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
export async function opfsResizeDisk(fileName, newSizeBytes, onProgress) {
  const handle = await opfsGetDiskFileHandle(fileName, { create: false });
  const writable = await handle.createWritable({ keepExistingData: true });
  report(onProgress, { phase: "resize", processedBytes: 0, totalBytes: newSizeBytes });
  await writable.truncate(newSizeBytes);
  await writable.close();
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
export async function streamToPortWithChecksum(stream, port, onProgress, totalBytes, phase) {
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
export async function opfsExportToPort(fileName, port, options, onProgress) {
  const handle = await opfsGetDiskFileHandle(fileName, { create: false });
  const file = await handle.getFile();
  /** @type {ReadableStream<Uint8Array>} */
  let stream = /** @type {any} */ (file.stream());

  if (options?.gzip) {
    if (typeof CompressionStream === "undefined") {
      throw new Error("CompressionStream not supported in this browser");
    }
    stream = stream.pipeThrough(new CompressionStream("gzip"));
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
async function idbPutChunk(db, diskId, index, data) {
  const tx = db.transaction(["chunks"], "readwrite");
  tx.objectStore("chunks").put({ id: diskId, index, data });
  await new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve(undefined);
    tx.onerror = () => reject(tx.error || new Error("IndexedDB chunk put failed"));
    tx.onabort = () => reject(tx.error || new Error("IndexedDB chunk put aborted"));
  });
}

/**
 * @param {IDBDatabase} db
 * @param {string} diskId
 * @param {number} index
 * @returns {Promise<ArrayBuffer | undefined>}
 */
async function idbGetChunk(db, diskId, index) {
  const tx = db.transaction(["chunks"], "readonly");
  const store = tx.objectStore("chunks");
  /** @type {{id: string; index: number; data: ArrayBuffer} | undefined} */
  const rec = await new Promise((resolve, reject) => {
    const req = store.get([diskId, index]);
    req.onsuccess = () => resolve(req.result || undefined);
    req.onerror = () => reject(req.error || new Error("IndexedDB chunk get failed"));
  });
  await new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve(undefined);
    tx.onerror = () => reject(tx.error || new Error("IndexedDB chunk tx failed"));
    tx.onabort = () => reject(tx.error || new Error("IndexedDB chunk tx aborted"));
  });
  return rec?.data;
}

/**
 * @param {IDBDatabase} db
 * @param {string} diskId
 * @returns {Promise<void>}
 */
export async function idbDeleteDiskData(db, diskId) {
  const tx = db.transaction(["chunks"], "readwrite");
  const index = tx.objectStore("chunks").index("by_id");
  const range = IDBKeyRange.only(diskId);
  await new Promise((resolve, reject) => {
    const req = index.openCursor(range);
    req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return resolve(undefined);
      cursor.delete();
      cursor.continue();
    };
  });
  await new Promise((resolve, reject) => {
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
export async function idbCreateBlankDisk(diskId, sizeBytes) {
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
export async function idbImportFile(diskId, file, onProgress) {
  const db = await openDiskManagerDb();
  let processed = 0;
  let crc = crc32Init();
  let chunkIndex = 0;

  report(onProgress, { phase: "import", processedBytes: 0, totalBytes: file.size });

  const reader = file.stream().getReader();
  /** @type {Uint8Array[]} */
  let pending = [];
  let pendingBytes = 0;

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    const part = value;
    pending.push(part);
    pendingBytes += part.byteLength;

    while (pendingBytes >= IDB_CHUNK_SIZE) {
      const chunk = new Uint8Array(IDB_CHUNK_SIZE);
      let offset = 0;
      while (offset < chunk.byteLength) {
        const head = pending[0];
        const take = Math.min(head.byteLength, chunk.byteLength - offset);
        chunk.set(head.subarray(0, take), offset);
        offset += take;
        if (take === head.byteLength) {
          pending.shift();
        } else {
          pending[0] = head.subarray(take);
        }
      }
      pendingBytes -= IDB_CHUNK_SIZE;
      await idbPutChunk(db, diskId, chunkIndex++, chunk.buffer);
      processed += chunk.byteLength;
      crc = crc32Update(crc, chunk);
      report(onProgress, { phase: "import", processedBytes: processed, totalBytes: file.size });
    }
  }

  if (pendingBytes > 0) {
    const chunk = new Uint8Array(pendingBytes);
    let offset = 0;
    for (const part of pending) {
      chunk.set(part, offset);
      offset += part.byteLength;
    }
    await idbPutChunk(db, diskId, chunkIndex++, chunk.buffer);
    processed += chunk.byteLength;
    crc = crc32Update(crc, chunk);
  }

  db.close();
  report(onProgress, { phase: "import", processedBytes: file.size, totalBytes: file.size });
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
export async function idbExportToPort(diskId, sizeBytes, port, options, onProgress) {
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

      const rawStream = new ReadableStream({
        async pull(controller) {
          if (index >= totalChunks) {
            controller.close();
            return;
          }
          const buf = await idbGetChunk(db, diskId, index);
          const remaining = sizeBytes - index * IDB_CHUNK_SIZE;
          const outLen = Math.min(IDB_CHUNK_SIZE, remaining);

          let chunk;
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

      const stream = rawStream.pipeThrough(new CompressionStream("gzip"));
      // Report raw (pre-compression) progress, but checksum the actual stream output.
      return await streamToPortWithChecksum(stream, port, undefined, undefined, "export");
    }

    let crc = crc32Init();
    let processed = 0;
    report(onProgress, { phase: "export", processedBytes: 0, totalBytes: sizeBytes });

    const totalChunks = Math.ceil(sizeBytes / IDB_CHUNK_SIZE);
    for (let index = 0; index < totalChunks; index++) {
      const buf = await idbGetChunk(db, diskId, index);
      const remaining = sizeBytes - index * IDB_CHUNK_SIZE;
      const outLen = Math.min(IDB_CHUNK_SIZE, remaining);

      let chunk;
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
export async function idbResizeDisk(diskId, oldSizeBytes, newSizeBytes, onProgress) {
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
  await new Promise((resolve, reject) => {
    const req = index.openCursor(range);
    req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return resolve(undefined);
      /** @type {{id: string; index: number; data: ArrayBuffer}} */
      const value = cursor.value;
      if (value.index >= keepChunks) {
        cursor.delete();
      }
      cursor.continue();
    };
  });

  await new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve(undefined);
    tx.onerror = () => reject(tx.error || new Error("IndexedDB resize tx failed"));
    tx.onabort = () => reject(tx.error || new Error("IndexedDB resize tx aborted"));
  });
  db.close();
  report(onProgress, { phase: "resize", processedBytes: newSizeBytes, totalBytes: newSizeBytes });
}
