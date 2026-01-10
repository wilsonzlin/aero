// @ts-check

import {
  buildDiskFileName,
  createMetadataStore,
  inferFormatFromFileName,
  inferKindFromFileName,
  newDiskId,
  openDiskManagerDb,
} from "./metadata.ts";
import {
  idbCreateBlankDisk,
  idbDeleteDiskData,
  idbExportToPort,
  idbImportFile,
  idbResizeDisk,
  opfsCreateBlankDisk,
  opfsDeleteDisk,
  opfsExportToPort,
  opfsGetDiskSizeBytes,
  opfsImportFile,
  opfsResizeDisk,
} from "./import_export.ts";

/**
 * @typedef {import("./metadata.ts").DiskBackend} DiskBackend
 */

/**
 * @typedef {import("./metadata.ts").DiskKind} DiskKind
 */

/**
 * @typedef {import("./metadata.ts").DiskFormat} DiskFormat
 */

/**
 * @typedef {import("./metadata.ts").DiskImageMetadata} DiskImageMetadata
 */

/**
 * @typedef {import("./metadata.ts").MountConfig} MountConfig
 */

/**
 * @param {any} err
 */
function serializeError(err) {
  if (err instanceof Error) {
    return { message: err.message, name: err.name, stack: err.stack };
  }
  return { message: String(err) };
}

/**
 * @param {number} requestId
 * @param {any} payload
 */
function postProgress(requestId, payload) {
  self.postMessage({ type: "progress", requestId, ...payload });
}

/**
 * @param {number} requestId
 * @param {any} result
 */
function postOk(requestId, result) {
  self.postMessage({ type: "response", requestId, ok: true, result });
}

/**
 * @param {number} requestId
 * @param {any} error
 */
function postErr(requestId, error) {
  self.postMessage({ type: "response", requestId, ok: false, error: serializeError(error) });
}

/**
 * @param {DiskBackend} backend
 */
function getStore(backend) {
  return createMetadataStore(backend);
}

/**
 * @param {DiskBackend} backend
 * @param {string} id
 * @returns {Promise<DiskImageMetadata>}
 */
async function requireDisk(backend, id) {
  const meta = await getStore(backend).getDisk(id);
  if (!meta) throw new Error(`Disk not found: ${id}`);
  return meta;
}

/**
 * @param {DiskBackend} backend
 * @param {DiskImageMetadata} meta
 */
async function putDisk(backend, meta) {
  await getStore(backend).putDisk(meta);
}

/**
 * @param {DiskBackend} backend
 * @param {{ hddId?: string; cdId?: string }} mounts
 */
async function validateMounts(backend, mounts) {
  if (mounts.hddId) {
    const hdd = await requireDisk(backend, mounts.hddId);
    if (hdd.kind !== "hdd") throw new Error("hddId must refer to a HDD image");
  }
  if (mounts.cdId) {
    const cd = await requireDisk(backend, mounts.cdId);
    if (cd.kind !== "cd") throw new Error("cdId must refer to a CD image");
  }
}

self.onmessage = (event) => {
  const msg = event.data;
  if (!msg || msg.type !== "request") return;
  const { requestId } = msg;
  handleRequest(msg).catch((err) => postErr(requestId, err));
};

/**
 * @param {any} msg
 */
async function handleRequest(msg) {
  /** @type {number} */
  const requestId = msg.requestId;
  /** @type {DiskBackend} */
  const backend = msg.backend;
  /** @type {string} */
  const op = msg.op;
  const store = getStore(backend);

  switch (op) {
    case "list_disks": {
      const disks = await store.listDisks();
      postOk(requestId, disks);
      return;
    }

    case "get_mounts": {
      const mounts = await store.getMounts();
      postOk(requestId, mounts);
      return;
    }

    case "set_mounts": {
      /** @type {MountConfig} */
      const mounts = msg.payload || {};
      await validateMounts(backend, mounts);

      const now = Date.now();
      if (mounts.hddId) {
        const meta = await requireDisk(backend, mounts.hddId);
        meta.lastUsedAtMs = now;
        await putDisk(backend, meta);
      }
      if (mounts.cdId) {
        const meta = await requireDisk(backend, mounts.cdId);
        meta.lastUsedAtMs = now;
        await putDisk(backend, meta);
      }

      await store.setMounts(mounts);
      postOk(requestId, mounts);
      return;
    }

    case "create_blank": {
      const { name, sizeBytes } = msg.payload;
      /** @type {DiskKind} */
      const kind = msg.payload.kind || "hdd";
      /** @type {DiskFormat} */
      const format = msg.payload.format || "raw";
      if (kind !== "hdd") throw new Error("Only HDD images can be created as blank disks");

      const id = newDiskId();
      const fileName = buildDiskFileName(id, format);

      const progressCb = /** @type {any} */ ((p) => postProgress(requestId, p));

      let checksumCrc32;
      if (backend === "opfs") {
        const res = await opfsCreateBlankDisk(fileName, sizeBytes, progressCb);
        checksumCrc32 = res.checksumCrc32;
      } else {
        await idbCreateBlankDisk(id, sizeBytes);
        checksumCrc32 = undefined;
      }

      /** @type {DiskImageMetadata} */
      const meta = {
        id,
        name,
        backend,
        kind,
        format,
        fileName,
        sizeBytes,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: checksumCrc32 ? { algorithm: "crc32", value: checksumCrc32 } : undefined,
      };

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "import_file": {
      /** @type {File} */
      const file = msg.payload.file;
      if (!file) throw new Error("Missing file");

      const fileNameOverride = msg.payload.name;
      const name = (fileNameOverride && String(fileNameOverride)) || file.name;

      /** @type {DiskKind} */
      const kind = msg.payload.kind || inferKindFromFileName(file.name);
      /** @type {DiskFormat} */
      const format = msg.payload.format || inferFormatFromFileName(file.name);

      const id = newDiskId();
      const fileName = buildDiskFileName(id, format);

      const progressCb = /** @type {any} */ ((p) => postProgress(requestId, p));

      let sizeBytes;
      let checksumCrc32;

      if (backend === "opfs") {
        const res = await opfsImportFile(fileName, file, progressCb);
        sizeBytes = res.sizeBytes;
        checksumCrc32 = res.checksumCrc32;
      } else {
        const res = await idbImportFile(id, file, progressCb);
        sizeBytes = res.sizeBytes;
        checksumCrc32 = res.checksumCrc32;
      }

      /** @type {DiskImageMetadata} */
      const meta = {
        id,
        name,
        backend,
        kind,
        format,
        fileName,
        sizeBytes,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: { algorithm: "crc32", value: checksumCrc32 },
        sourceFileName: file.name,
      };

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "stat_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      let actualSize = meta.sizeBytes;
      if (backend === "opfs") {
        actualSize = await opfsGetDiskSizeBytes(meta.fileName);
      }
      postOk(requestId, { meta, actualSizeBytes: actualSize });
      return;
    }

    case "resize_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      const newSizeBytes = msg.payload.newSizeBytes;
      if (typeof newSizeBytes !== "number" || newSizeBytes < 0) throw new Error("Invalid newSizeBytes");

      const progressCb = /** @type {any} */ ((p) => postProgress(requestId, p));

      if (backend === "opfs") {
        await opfsResizeDisk(meta.fileName, newSizeBytes, progressCb);
      } else {
        await idbResizeDisk(meta.id, meta.sizeBytes, newSizeBytes, progressCb);
      }

      meta.sizeBytes = newSizeBytes;
      // Resizing invalidates checksums.
      meta.checksum = undefined;
      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "delete_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      if (backend === "opfs") {
        await opfsDeleteDisk(meta.fileName);
      } else {
        const db = await openDiskManagerDb();
        await idbDeleteDiskData(db, meta.id);
        db.close();
      }
      await store.deleteDisk(meta.id);
      postOk(requestId, { ok: true });
      return;
    }

    case "export_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      /** @type {MessagePort} */
      const port = msg.port;
      if (!port) throw new Error("Missing MessagePort for export");

      const options = msg.payload.options || {};
      const progressCb = /** @type {any} */ ((p) => postProgress(requestId, p));

      // Respond immediately so the main thread can start consuming the stream.
      postOk(requestId, { started: true, meta });

      const now = Date.now();
      meta.lastUsedAtMs = now;
      await store.putDisk(meta);

      void (async () => {
        try {
          if (backend === "opfs") {
            await opfsExportToPort(meta.fileName, port, options, progressCb);
          } else {
            await idbExportToPort(meta.id, meta.sizeBytes, port, options, progressCb);
          }
        } catch (err) {
          try {
            port.postMessage({ type: "error", error: serializeError(err) });
          } finally {
            port.close();
          }
        }
      })();

      return;
    }

    default:
      throw new Error(`Unknown op: ${op}`);
  }
}

