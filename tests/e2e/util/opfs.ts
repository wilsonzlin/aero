import type { Page } from "@playwright/test";

export type OpfsSyncAccessHandleProbeResult =
  | { ok: true; supported: true }
  | { ok: true; supported: false; reason: string }
  | { ok: false; supported: false; reason: string };

/**
 * Best-effort removal of a file entry from OPFS.
 *
 * - No-ops if OPFS APIs are unavailable.
 * - Swallows errors (missing entry, permission issues, etc.).
 * - Intended for Playwright cleanup because OPFS can persist across runs in some environments.
 */
export async function removeOpfsEntryBestEffort(page: Page, path: string): Promise<void> {
  await page.evaluate(async (path) => {
    try {
      const storage = (navigator as Navigator & { storage?: StorageManager | undefined }).storage;
      const getDir = (storage as StorageManager & { getDirectory?: unknown })?.getDirectory as
        | ((this: StorageManager) => Promise<FileSystemDirectoryHandle>)
        | undefined;
      if (typeof getDir !== "function") return;

      const parts = String(path)
        .split("/")
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      if (parts.length === 0) return;
      const filename = parts.pop();
      if (!filename) return;

      let dir = await getDir.call(storage);
      for (const part of parts) {
        dir = await dir.getDirectoryHandle(part);
      }
      await dir.removeEntry(filename);
    } catch {
      // ignore
    }
  }, path);
}

/**
 * Best-effort probe for OPFS SyncAccessHandle support.
 *
 * Notes:
 * - Worker VM snapshots require SyncAccessHandle support in the browser/profile.
 * - We probe via a small OPFS file handle and check for `createSyncAccessHandle` support.
 * - This runs in the page context; callers should invoke it *before* kicking off heavy worker/WASM
 *   initialization so unsupported environments can skip quickly.
 */
export async function probeOpfsSyncAccessHandle(page: Page): Promise<OpfsSyncAccessHandleProbeResult> {
  return await page.evaluate(async () => {
    const { formatOneLineUtf8 } = await import("/web/src/text.ts");
    const MAX_ERROR_BYTES = 512;
    try {
      const storage = navigator.storage as StorageManager & { getDirectory?: () => Promise<FileSystemDirectoryHandle> };
      if (typeof storage?.getDirectory !== "function") {
        return { ok: true as const, supported: false as const, reason: "navigator.storage.getDirectory unavailable" };
      }

      // SyncAccessHandle is only usable in workers. Probe via a tiny blob worker so we
      // don't incorrectly skip environments where the API exists only off the main thread.
      const blobUrl = URL.createObjectURL(
        new Blob(
          [
            `
              const MAX_ERROR_CHARS = 512;
              const formatErr = (err) => {
                const msg = err && typeof err === 'object' && 'message' in err ? String(err.message) : String(err);
                return msg
                  .replace(/[\\x00-\\x1F\\x7F]/g, ' ')
                  .replace(/\\s+/g, ' ')
                  .trim()
                  .slice(0, MAX_ERROR_CHARS);
              };

              self.onmessage = async () => {
                try {
                  const storage = navigator.storage;
                  if (!storage || typeof storage.getDirectory !== 'function') {
                    self.postMessage({ supported: false, reason: 'navigator.storage.getDirectory unavailable' });
                    return;
                  }

                  const root = await storage.getDirectory();
                  let dir = root;
                  try {
                    dir = await root.getDirectoryHandle('state', { create: true });
                  } catch {}

                  // Use a per-probe filename to avoid SyncAccessHandle lock contention when
                  // Playwright runs multiple tests in parallel.
                  const filename = 'aero-sync-access-handle-probe-' + Math.random().toString(16).slice(2) + '.tmp';
                  const file = await dir.getFileHandle(filename, { create: true });
                  const cleanupProbeFile = async () => {
                    try {
                      await dir.removeEntry(filename);
                    } catch {}
                  };

                  if (typeof file.createSyncAccessHandle !== 'function') {
                    await cleanupProbeFile();
                    self.postMessage({ supported: false, reason: 'FileSystemFileHandle.createSyncAccessHandle unavailable' });
                    return;
                  }

                  let handle = null;
                  try {
                    handle = await file.createSyncAccessHandle();
                    if (handle && typeof handle.close === 'function') {
                      await handle.close();
                    }
                  } catch (err) {
                    await cleanupProbeFile();
                    self.postMessage({ supported: false, reason: formatErr(err) });
                    return;
                  }

                  await cleanupProbeFile();
                  self.postMessage({ supported: true });
                } catch (err) {
                  self.postMessage({ supported: false, reason: formatErr(err) });
                }
              };
            `,
          ],
          { type: "text/javascript" },
        ),
      );

      const workerResult = await new Promise<{ supported: boolean; reason?: string }>((resolve, reject) => {
        const timeoutMs = 10_000;
        const timer = setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out probing OPFS SyncAccessHandle in worker (${timeoutMs}ms).`));
        }, timeoutMs);

        let worker: Worker | null = null;
        const cleanup = () => {
          clearTimeout(timer);
          try {
            worker?.terminate();
          } catch {
            // ignore
          }
          try {
            URL.revokeObjectURL(blobUrl);
          } catch {
            // ignore
          }
          worker = null;
        };

        try {
          worker = new Worker(blobUrl);
        } catch (err) {
          cleanup();
          reject(err);
          return;
        }

        worker.onmessage = (ev) => {
          cleanup();
          const data = ev.data as unknown;
          if (!data || typeof data !== "object") {
            resolve({ supported: false, reason: "Invalid probe worker response." });
            return;
          }
          const rec = data as { supported?: unknown; reason?: unknown };
          resolve({ supported: rec.supported === true, reason: typeof rec.reason === "string" ? rec.reason : undefined });
        };
        worker.onerror = (ev) => {
          cleanup();
          reject(new Error(ev.message || "OPFS SyncAccessHandle probe worker error."));
        };

        worker.postMessage(null);
      });

      return workerResult.supported
        ? ({ ok: true as const, supported: true as const } satisfies OpfsSyncAccessHandleProbeResult)
        : ({
            ok: true as const,
            supported: false as const,
            reason: workerResult.reason ?? "FileSystemFileHandle.createSyncAccessHandle unavailable",
          } satisfies OpfsSyncAccessHandleProbeResult);
    } catch (err) {
      const msg = err instanceof Error ? err.message : err;
      return {
        ok: false as const,
        supported: false as const,
        reason: formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error",
      } satisfies OpfsSyncAccessHandleProbeResult;
    }
  });
}
