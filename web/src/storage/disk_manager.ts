import {
  clearIdb,
  clearOpfs,
  extensionForFormat,
  pickDefaultBackend,
  type DiskBackend,
  type DiskImageMetadata,
  type DiskKind,
  type DiskFormat,
  type MountConfig,
  type RemoteDiskDelivery,
  type RemoteDiskUrls,
  type RemoteDiskValidator,
} from "./metadata";
import type { ImportProgress } from "./import_export";

export type ExportHandle = {
  stream: ReadableStream<Uint8Array>;
  done: Promise<{ checksumCrc32: string }>;
  meta: DiskImageMetadata;
};

type DiskWorkerError = { message: string; name?: string; stack?: string };

type DiskWorkerProgressMessage = { type: "progress"; requestId: number } & ImportProgress;
type DiskWorkerResponseMessage =
  | { type: "response"; requestId: number; ok: true; result: unknown }
  | { type: "response"; requestId: number; ok: false; error: DiskWorkerError };

type DiskWorkerMessage = DiskWorkerProgressMessage | DiskWorkerResponseMessage;

type PendingRequest = {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
  onProgress?: (p: ImportProgress) => void;
};

function defaultExportFileName(meta: DiskImageMetadata, gzip: boolean): string {
  const ext = extensionForFormat(meta.format);
  const base = meta.name?.trim() ? meta.name.trim() : meta.id;
  const withExt = base.toLowerCase().endsWith(`.${ext}`) ? base : `${base}.${ext}`;
  return gzip ? `${withExt}.gz` : withExt;
}

function isHddKind(kind: DiskKind | undefined): kind is DiskKind {
  return kind === "hdd" || kind === "cd";
}

function isFormat(format: DiskFormat | undefined): format is DiskFormat {
  return (
    format === "raw" ||
    format === "iso" ||
    format === "qcow2" ||
    format === "vhd" ||
    format === "aerospar" ||
    format === "unknown"
  );
}

/**
 * Main-thread API for disk image management.
 *
 * Heavy lifting is done in a dedicated worker (`disk_worker.ts`) to avoid blocking the UI.
 */
export class DiskManager {
  readonly backend: DiskBackend;
  private readonly worker: Worker;
  private nextRequestId = 1;
  private readonly pending = new Map<number, PendingRequest>();

  constructor(options: { backend: DiskBackend; worker?: Worker }) {
    this.backend = options.backend;
    this.worker =
      options.worker ??
      new Worker(new URL("./disk_worker.ts", import.meta.url), {
        type: "module",
      });

    this.worker.onmessage = (event: MessageEvent<DiskWorkerMessage>) => {
      const msg = event.data;
      if (!msg || typeof msg !== "object") return;
      if (msg.type === "progress") {
        const entry = this.pending.get(msg.requestId);
        entry?.onProgress?.(msg);
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

  static async create(options?: { backend?: DiskBackend }): Promise<DiskManager> {
    const backend = options?.backend ?? pickDefaultBackend();
    return new DiskManager({ backend });
  }

  static pickDefaultBackend(): DiskBackend {
    return pickDefaultBackend();
  }

  static async clearAllStorage(backend?: DiskBackend): Promise<void> {
    if (!backend || backend === "opfs") await clearOpfs();
    if (!backend || backend === "idb") await clearIdb();
  }

  close(): void {
    this.worker.terminate();
    this.pending.clear();
  }

  private request<T>(
    op: string,
    payload: unknown,
    options?: { onProgress?: (p: ImportProgress) => void; transfer?: Transferable[] },
  ): Promise<T> {
    const requestId = this.nextRequestId++;
    const transfer = options?.transfer ?? [];
    return new Promise<T>((resolve, reject) => {
      this.pending.set(requestId, { resolve: resolve as PendingRequest["resolve"], reject, onProgress: options?.onProgress });
      this.worker.postMessage(
        {
          type: "request",
          requestId,
          backend: this.backend,
          op,
          payload,
          port: (payload as { port?: MessagePort } | undefined)?.port,
        },
        transfer,
      );
    });
  }

  /**
   * @returns {Promise<DiskImageMetadata[]>}
   */
  async listDisks(): Promise<DiskImageMetadata[]> {
    return this.request("list_disks", {});
  }

  /**
   * @returns {Promise<MountConfig>}
   */
  async getMounts(): Promise<MountConfig> {
    return this.request("get_mounts", {});
  }

  /**
   * @param {MountConfig} mounts
   * @returns {Promise<MountConfig>}
   */
  async setMounts(mounts: MountConfig): Promise<MountConfig> {
    return this.request("set_mounts", mounts);
  }

  /**
   * Create a new blank disk image.
   *
   * @param {{ name: string; sizeBytes: number; kind?: "hdd"; format?: "raw"; onProgress?: (p: ImportProgress) => void }} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async createBlankDisk(options: {
    name: string;
    sizeBytes: number;
    kind?: "hdd";
    format?: "raw";
    onProgress?: (p: ImportProgress) => void;
  }): Promise<DiskImageMetadata> {
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
  async importDisk(
    file: File,
    options?: {
      name?: string;
      kind?: DiskKind;
      format?: DiskFormat;
      onProgress?: (p: ImportProgress) => void;
    },
  ): Promise<DiskImageMetadata> {
    return this.request(
      "import_file",
      { file, name: options?.name, kind: isHddKind(options?.kind) ? options.kind : undefined, format: isFormat(options?.format) ? options.format : undefined },
      { onProgress: options?.onProgress, transfer: [] },
    );
  }

  /**
   * Import and convert an image into Aero's internal sparse-on-OPFS format.
   *
   * This runs inside the disk worker so it can use OPFS SyncAccessHandles.
   * Only supported for the OPFS backend.
   *
   * @param {File} file
   * @param {{ name?: string; blockSizeBytes?: number; onProgress?: (p: ImportProgress) => void } | undefined} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async importDiskConverted(
    file: File,
    options?: { name?: string; blockSizeBytes?: number; onProgress?: (p: ImportProgress) => void },
  ): Promise<DiskImageMetadata> {
    return this.request(
      "import_convert",
      { file, name: options?.name, blockSizeBytes: options?.blockSizeBytes },
      { onProgress: options?.onProgress, transfer: [] },
    );
  }

  /**
   * @param {string} id
   * @returns {Promise<{ meta: DiskImageMetadata; actualSizeBytes: number }>}
   */
  async statDisk(id: string): Promise<{ meta: DiskImageMetadata; actualSizeBytes: number }> {
    return this.request("stat_disk", { id });
  }

  /**
   * @param {string} id
   * @param {number} newSizeBytes
   * @param {{ onProgress?: (p: ImportProgress) => void } | undefined} options
   * @returns {Promise<DiskImageMetadata>}
   */
  async resizeDisk(
    id: string,
    newSizeBytes: number,
    options?: { onProgress?: (p: ImportProgress) => void },
  ): Promise<DiskImageMetadata> {
    return this.request("resize_disk", { id, newSizeBytes }, { onProgress: options?.onProgress });
  }

  /**
   * @param {string} id
   * @returns {Promise<void>}
   */
  async deleteDisk(id: string): Promise<void> {
    await this.request("delete_disk", { id });
  }

  async addRemoteDisk(options: {
    name: string;
    imageId: string;
    version: string;
    delivery: RemoteDiskDelivery;
    urls: RemoteDiskUrls;
    sizeBytes: number;
    validator?: RemoteDiskValidator;
    kind?: DiskKind;
    format?: DiskFormat;
    cacheBackend?: DiskBackend;
    /**
     * Local cache chunk size.
     *
     * Defaults:
     * - `delivery="range"`: 1 MiB
     * - `delivery="chunked"`: 4 MiB
     */
    chunkSizeBytes?: number;
    cacheFileName?: string;
    overlayFileName?: string;
    overlayBlockSizeBytes?: number;
  }): Promise<DiskImageMetadata> {
    return this.request("create_remote", {
      name: options.name,
      imageId: options.imageId,
      version: options.version,
      delivery: options.delivery,
      urls: options.urls,
      sizeBytes: options.sizeBytes,
      validator: options.validator,
      kind: isHddKind(options.kind) ? options.kind : undefined,
      format: isFormat(options.format) ? options.format : undefined,
      cacheBackend: options.cacheBackend,
      chunkSizeBytes: options.chunkSizeBytes,
      cacheFileName: options.cacheFileName,
      overlayFileName: options.overlayFileName,
      overlayBlockSizeBytes: options.overlayBlockSizeBytes,
    });
  }

  async updateRemoteDisk(
    id: string,
    patch: Partial<{
      name: string;
      imageId: string;
      version: string;
      delivery: RemoteDiskDelivery;
      urls: RemoteDiskUrls;
      sizeBytes: number;
      validator: RemoteDiskValidator;
      kind: DiskKind;
      format: DiskFormat;
      cacheBackend: DiskBackend;
      chunkSizeBytes: number;
      cacheFileName: string;
      overlayFileName: string;
      overlayBlockSizeBytes: number;
    }>,
  ): Promise<DiskImageMetadata> {
    return this.request("update_remote", { id, ...patch });
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
  async exportDiskStream(id: string, options?: { gzip?: boolean; onProgress?: (p: ImportProgress) => void }): Promise<ExportHandle> {
    const channel = new MessageChannel();
    const port = channel.port1;

    const start = (await this.request(
      "export_disk",
      { id, options: { gzip: !!options?.gzip }, port },
      { onProgress: options?.onProgress, transfer: [port] },
    )) as { started: true; meta: DiskImageMetadata };

    const meta = start.meta;

    let doneResolve: (value: { checksumCrc32: string }) => void;
    let doneReject: (reason: unknown) => void;
    const done = new Promise<{ checksumCrc32: string }>((resolve, reject) => {
      doneResolve = resolve;
      doneReject = reject;
    });

    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        channel.port2.onmessage = (event: MessageEvent<any>) => {
          const msg = event.data as any;
          if (!msg || typeof msg !== "object") return;
          if (msg.type === "chunk" && msg.chunk instanceof Uint8Array) {
            controller.enqueue(msg.chunk as Uint8Array);
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
        // `MessagePort` queues messages until it is started; `onmessage` implicitly starts it
        // in most engines, but calling `start()` is safe and prevents early messages being dropped.
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

  /**
   * Convenience wrapper: export a disk image and save it to a user-selected file.
   *
   * Uses the File System Access API when available; otherwise falls back to an in-memory Blob
   * download (not suitable for multi-GB images).
   */
  async exportDiskToFile(
    id: string,
    options?: { gzip?: boolean; suggestedName?: string; onProgress?: (p: ImportProgress) => void },
  ): Promise<{ checksumCrc32: string; fileName: string; meta: DiskImageMetadata }> {
    const handle = await this.exportDiskStream(id, options);
    const fileName = options?.suggestedName ?? defaultExportFileName(handle.meta, !!options?.gzip);

    const showSaveFilePicker = (globalThis as any).showSaveFilePicker as
      | ((options?: any) => Promise<any>)
      | undefined;

    try {
      if (typeof showSaveFilePicker === "function") {
        const pickerHandle = await showSaveFilePicker({ suggestedName: fileName });
        const writable = await pickerHandle.createWritable();
        await handle.stream.pipeTo(writable);
      } else {
        const blob = await new Response(handle.stream).blob();
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = fileName;
        a.rel = "noopener";
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
    } catch (err) {
      try {
        await handle.stream.cancel(err);
      } catch {
        // ignore
      }
      throw err;
    }

    const done = await handle.done;
    return { checksumCrc32: done.checksumCrc32, fileName, meta: handle.meta };
  }
}
