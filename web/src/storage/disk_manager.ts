// @ts-check

import { clearIdb, clearOpfs, pickDefaultBackend } from "./metadata.ts";

/**
 * @typedef {import("./metadata.ts").DiskBackend} DiskBackend
 */

/**
 * @typedef {import("./metadata.ts").DiskImageMetadata} DiskImageMetadata
 */

/**
 * @typedef {import("./metadata.ts").MountConfig} MountConfig
 */

/**
 * @typedef {import("./import_export.ts").ImportProgress} ImportProgress
 */

/**
 * @typedef ExportHandle
 * @property {ReadableStream<Uint8Array>} stream
 * @property {Promise<{ checksumCrc32: string }>} done
 * @property {DiskImageMetadata} meta
 */

/**
 * Main-thread API for disk image management. Heavy lifting is done in a
 * dedicated worker (`disk_worker.ts`) to avoid blocking the UI.
 */
export class DiskManager {
  /**
   * @param {{ backend: DiskBackend; worker?: Worker } } options
   */
  constructor(options) {
    this.backend = options.backend;
    /** @type {Worker} */
    this.worker =
      options.worker ||
      new Worker(new URL("./disk_worker.ts", import.meta.url), {
        type: "module",
      });

    /** @type {number} */
    this.nextRequestId = 1;
    /** @type {Map<number, { resolve: (v: any) => void; reject: (e: any) => void; onProgress?: ((p: ImportProgress) => void) }>} */
    this.pending = new Map();

    this.worker.onmessage = (event) => {
      const msg = event.data;
      if (!msg || typeof msg !== "object") return;
      if (msg.type === "progress") {
        const entry = this.pending.get(msg.requestId);
        if (entry?.onProgress) entry.onProgress(msg);
        return;
      }
      if (msg.type === "response") {
        const entry = this.pending.get(msg.requestId);
        if (!entry) return;
        this.pending.delete(msg.requestId);
        if (msg.ok) entry.resolve(msg.result);
        else entry.reject(Object.assign(new Error(msg.error?.message || "Disk worker error"), msg.error));
      }
    };
  }

  /**
   * @param {{ backend?: DiskBackend } | undefined} options
   * @returns {Promise<DiskManager>}
   */
  static async create(options) {
    const backend = options?.backend || pickDefaultBackend();
    return new DiskManager({ backend });
  }

  /**
   * @returns {DiskBackend}
   */
  static pickDefaultBackend() {
    return pickDefaultBackend();
  }

  /**
   * @param {DiskBackend | undefined} backend
   * @returns {Promise<void>}
   */
  static async clearAllStorage(backend) {
    if (!backend || backend === "opfs") await clearOpfs();
    if (!backend || backend === "idb") await clearIdb();
  }

  close() {
    this.worker.terminate();
    this.pending.clear();
  }

  /**
   * @template T
   * @param {string} op
   * @param {any} payload
   * @param {{ onProgress?: (p: ImportProgress) => void; transfer?: Transferable[] } | undefined} options
   * @returns {Promise<T>}
   */
  request(op, payload, options) {
    const requestId = this.nextRequestId++;
    const transfer = options?.transfer || [];
    return new Promise((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject, onProgress: options?.onProgress });
      this.worker.postMessage(
        {
          type: "request",
          requestId,
          backend: this.backend,
          op,
          payload,
          port: payload?.port,
        },
        transfer,
      );
    });
  }

  /**
   * @returns {Promise<DiskImageMetadata[]>}
   */
  async listDisks() {
    return this.request("list_disks", {});
  }

  /**
   * @returns {Promise<MountConfig>}
   */
  async getMounts() {
    return this.request("get_mounts", {});
  }

  /**
   * @param {MountConfig} mounts
   * @returns {Promise<MountConfig>}
   */
  async setMounts(mounts) {
    return this.request("set_mounts", mounts);
  }

  /**
   * Create a new blank disk image.
   *
   * @param {{ name: string; sizeBytes: number; kind?: "hdd"; format?: "raw"; onProgress?: (p: ImportProgress) => void }} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async createBlankDisk(options) {
    return this.request(
      "create_blank",
      { name: options.name, sizeBytes: options.sizeBytes, kind: options.kind || "hdd", format: options.format || "raw" },
      { onProgress: options.onProgress },
    );
  }

  /**
   * Import an existing image (img/iso/qcow2) into the selected backend.
   *
   * @param {File} file
   * @param {{ name?: string; kind?: "hdd" | "cd"; format?: "raw" | "iso" | "qcow2" | "unknown"; onProgress?: (p: ImportProgress) => void } | undefined} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async importDisk(file, options) {
    return this.request(
      "import_file",
      { file, name: options?.name, kind: options?.kind, format: options?.format },
      { onProgress: options?.onProgress, transfer: [] },
    );
  }

  /**
   * @param {string} id
   * @returns {Promise<{ meta: DiskImageMetadata; actualSizeBytes: number }>}
   */
  async statDisk(id) {
    return this.request("stat_disk", { id });
  }

  /**
   * @param {string} id
   * @param {number} newSizeBytes
   * @param {{ onProgress?: (p: ImportProgress) => void } | undefined} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async resizeDisk(id, newSizeBytes, options) {
    return this.request("resize_disk", { id, newSizeBytes }, { onProgress: options?.onProgress });
  }

  /**
   * @param {string} id
   * @returns {Promise<void>}
   */
  async deleteDisk(id) {
    await this.request("delete_disk", { id });
  }

  /**
   * Export a disk image as a `ReadableStream<Uint8Array>`.
   *
   * UI code can pipe this stream into `showSaveFilePicker()` (File System Access API)
   * or buffer it into a `Blob` for a classic `<a download>` flow.
   *
   * @param {string} id
   * @param {{ gzip?: boolean; onProgress?: (p: ImportProgress) => void } | undefined} options
   * @returns {Promise<ExportHandle>}
   */
  async exportDiskStream(id, options) {
    const channel = new MessageChannel();
    /** @type {MessagePort} */
    const port = channel.port1;

    const start = await this.request(
      "export_disk",
      { id, options: { gzip: !!options?.gzip }, port },
      { onProgress: options?.onProgress, transfer: [port] },
    );

    /** @type {DiskImageMetadata} */
    const meta = start.meta;

    /** @type {(value: { checksumCrc32: string }) => void} */
    let doneResolve;
    /** @type {(reason: any) => void} */
    let doneReject;
    const done = new Promise((resolve, reject) => {
      doneResolve = resolve;
      doneReject = reject;
    });

    const stream = new ReadableStream({
      start(controller) {
        channel.port2.onmessage = (event) => {
          const msg = event.data;
          if (!msg || typeof msg !== "object") return;
          if (msg.type === "chunk") {
            controller.enqueue(msg.chunk);
            return;
          }
          if (msg.type === "done") {
            controller.close();
            doneResolve({ checksumCrc32: msg.checksumCrc32 });
            channel.port2.close();
            return;
          }
          if (msg.type === "error") {
            const err = Object.assign(new Error(msg.error?.message || "Export failed"), msg.error);
            controller.error(err);
            doneReject(err);
            channel.port2.close();
          }
        };
        // `MessagePort` queues messages until it is started; setting `onmessage`
        // implicitly starts it in most engines, but calling `start()` here is safe
        // and prevents early messages being dropped.
        channel.port2.start?.();
      },
      cancel(reason) {
        doneReject(reason);
        try {
          channel.port2.close();
        } catch (err) {
          // ignore
        }
      },
    });

    return { stream, done, meta };
  }
}
