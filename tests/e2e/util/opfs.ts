import type { Page } from "@playwright/test";

export type OpfsSyncAccessHandleProbeResult =
  | { ok: true; supported: true }
  | { ok: true; supported: false; reason: string }
  | { ok: false; supported: false; reason: string };

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

