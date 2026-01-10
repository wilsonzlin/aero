import { importConvertToOpfs, type ImportSource, type ImportConvertOptions } from "./import_convert.ts";

type WorkerError = { name?: string; message: string; stack?: string };

function serializeError(err: unknown): WorkerError {
  if (err instanceof DOMException) {
    return { name: err.name, message: err.message, stack: err.stack };
  }
  if (err instanceof Error) {
    return { name: err.name, message: err.message, stack: err.stack };
  }
  return { message: String(err) };
}

type ConvertRequest = {
  type: "convert";
  requestId: number;
  source: ImportSource;
  /**
   * OPFS directory path relative to `navigator.storage.getDirectory()`, e.g.
   * `"images"` or `"aero/disks"`.
   */
  destDirPath: string;
  baseName: string;
  options?: Pick<ImportConvertOptions, "blockSizeBytes">;
};

type AbortRequest = { type: "abort"; requestId: number };

type IncomingMessage = ConvertRequest | AbortRequest;

type ProgressMessage = { type: "progress"; requestId: number; processedBytes: number; totalBytes: number };
type ResultMessage =
  | { type: "result"; requestId: number; ok: true; manifest: unknown }
  | { type: "result"; requestId: number; ok: false; error: WorkerError };

type OutgoingMessage = ProgressMessage | ResultMessage;

async function getDirectoryHandleForPath(path: string): Promise<FileSystemDirectoryHandle> {
  const trimmed = path.trim();
  if (!trimmed) throw new Error("destDirPath must not be empty");
  if (!navigator.storage?.getDirectory) throw new Error("OPFS is not available (navigator.storage.getDirectory missing)");
  const root = await navigator.storage.getDirectory();
  const parts = trimmed.split("/").filter((p) => p.length > 0);
  let dir = root;
  for (const part of parts) {
    if (part === "." || part === "..") throw new Error('destDirPath must not contain "." or ".."');
    dir = await dir.getDirectoryHandle(part, { create: true });
  }
  return dir;
}

const aborters = new Map<number, AbortController>();

self.onmessage = (event: MessageEvent<IncomingMessage>) => {
  const msg = event.data;
  if (!msg || typeof msg !== "object") return;

  if (msg.type === "abort") {
    aborters.get(msg.requestId)?.abort();
    return;
  }

  if (msg.type !== "convert") return;

  const requestId = msg.requestId;
  const ac = new AbortController();
  aborters.set(requestId, ac);

  void (async () => {
    try {
      const destDir = await getDirectoryHandleForPath(msg.destDirPath);
      const manifest = await importConvertToOpfs(msg.source, destDir, msg.baseName, {
        blockSizeBytes: msg.options?.blockSizeBytes,
        signal: ac.signal,
        onProgress(p) {
          const payload: OutgoingMessage = { type: "progress", requestId, processedBytes: p.processedBytes, totalBytes: p.totalBytes };
          (self as DedicatedWorkerGlobalScope).postMessage(payload);
        },
      });
      const payload: OutgoingMessage = { type: "result", requestId, ok: true, manifest };
      (self as DedicatedWorkerGlobalScope).postMessage(payload);
    } catch (err) {
      const payload: OutgoingMessage = { type: "result", requestId, ok: false, error: serializeError(err) };
      (self as DedicatedWorkerGlobalScope).postMessage(payload);
    } finally {
      aborters.delete(requestId);
    }
  })();
};
