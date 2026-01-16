import { importConvertToOpfs, type ImportSource, type ImportConvertOptions } from "./import_convert.ts";
import { serializeErrorForWorker, type WorkerSerializedError } from "../errors/serialize";

type WorkerError = WorkerSerializedError;

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function requireSafeNonNegativeInt(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
  return value;
}

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

self.onmessage = (event: MessageEvent<unknown>) => {
  const msg = event.data;
  if (!isRecord(msg)) return;
  // Treat postMessage payloads as untrusted; ignore inherited fields (prototype pollution).
  const type = hasOwn(msg, "type") ? msg.type : undefined;

  if (type === "abort") {
    const requestId = hasOwn(msg, "requestId") ? msg.requestId : undefined;
    if (typeof requestId === "number" && Number.isSafeInteger(requestId) && requestId >= 0) {
      aborters.get(requestId)?.abort();
    }
    return;
  }

  if (type !== "convert") return;

  let requestId: number;
  try {
    requestId = requireSafeNonNegativeInt(hasOwn(msg, "requestId") ? msg.requestId : undefined, "requestId");
  } catch {
    return;
  }
  const destDirPath = hasOwn(msg, "destDirPath") ? msg.destDirPath : "";
  const baseName = hasOwn(msg, "baseName") ? msg.baseName : "";
  const source = hasOwn(msg, "source") ? msg.source : undefined;
  const options = hasOwn(msg, "options") ? msg.options : undefined;

  const ac = new AbortController();
  aborters.set(requestId, ac);

  void (async () => {
    try {
      const destDir = await getDirectoryHandleForPath(String(destDirPath));
      const optsObj = isRecord(options) ? options : null;
      const rawBlockSizeBytes =
        optsObj && hasOwn(optsObj, "blockSizeBytes") ? (optsObj as Record<string, unknown>).blockSizeBytes : undefined;
      const blockSizeBytes =
        rawBlockSizeBytes === undefined
          ? undefined
          : (() => {
              if (typeof rawBlockSizeBytes !== "number" || !Number.isSafeInteger(rawBlockSizeBytes) || rawBlockSizeBytes <= 0) {
                throw new Error("options.blockSizeBytes must be a positive safe integer");
              }
              return rawBlockSizeBytes;
            })();
      const manifest = await importConvertToOpfs(source as ImportSource, destDir, String(baseName), {
        blockSizeBytes,
        signal: ac.signal,
        onProgress(p) {
          const payload: OutgoingMessage = { type: "progress", requestId, processedBytes: p.processedBytes, totalBytes: p.totalBytes };
          (self as DedicatedWorkerGlobalScope).postMessage(payload);
        },
      });
      const payload: OutgoingMessage = { type: "result", requestId, ok: true, manifest };
      (self as DedicatedWorkerGlobalScope).postMessage(payload);
    } catch (err) {
      const payload: OutgoingMessage = { type: "result", requestId, ok: false, error: serializeErrorForWorker(err) };
      (self as DedicatedWorkerGlobalScope).postMessage(payload);
    } finally {
      aborters.delete(requestId);
    }
  })();
};
