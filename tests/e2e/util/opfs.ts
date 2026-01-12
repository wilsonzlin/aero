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
    try {
      const storage = navigator.storage as StorageManager & { getDirectory?: () => Promise<FileSystemDirectoryHandle> };
      if (typeof storage?.getDirectory !== "function") {
        return { ok: true as const, supported: false as const, reason: "navigator.storage.getDirectory unavailable" };
      }

      const root = await storage.getDirectory();
      // Ensure the snapshot directory exists (WorkerCoordinator writes under `state/` by default).
      try {
        await root.getDirectoryHandle("state", { create: true });
      } catch {
        // ignore best-effort
      }

      const handle = await root.getFileHandle("aero-sync-access-handle-probe.tmp", { create: true });
      const supported = typeof (handle as unknown as { createSyncAccessHandle?: unknown }).createSyncAccessHandle === "function";
      return supported
        ? ({ ok: true as const, supported: true as const } satisfies OpfsSyncAccessHandleProbeResult)
        : ({ ok: true as const, supported: false as const, reason: "FileSystemFileHandle.createSyncAccessHandle unavailable" } satisfies OpfsSyncAccessHandleProbeResult);
    } catch (err) {
      return {
        ok: false as const,
        supported: false as const,
        reason: err instanceof Error ? err.message : String(err),
      } satisfies OpfsSyncAccessHandleProbeResult;
    }
  });
}
