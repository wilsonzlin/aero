import type { PruneRemoteCachesDryRunResult, PruneRemoteCachesResult } from "./disk_manager";

const MS_PER_DAY = 24 * 60 * 60 * 1000;

/**
 * Convert a "days" UI input into an age threshold in milliseconds.
 *
 * - Clamps non-finite/negative inputs to 0.
 * - Uses whole days (floors fractional days).
 */
export function computeOlderThanMsFromDays(days: number): number {
  if (!Number.isFinite(days) || days <= 0) return 0;
  const wholeDays = Math.floor(days);
  const ms = wholeDays * MS_PER_DAY;
  return Number.isFinite(ms) && ms >= 0 ? ms : 0;
}

type PruneRemoteCachesManager = {
  backend: string;
  pruneRemoteCaches: {
    (options: { olderThanMs: number; maxCaches?: number | undefined; dryRun: true }): Promise<PruneRemoteCachesDryRunResult>;
    (options: {
      olderThanMs: number;
      maxCaches?: number | undefined;
      dryRun?: false | undefined;
    }): Promise<PruneRemoteCachesResult>;
  };
};

export type PruneRemoteCachesAndRefreshResult =
  | { supported: false; message: string }
  | { supported: true; result: PruneRemoteCachesResult };

export async function pruneRemoteCachesAndRefresh(options: {
  manager: PruneRemoteCachesManager;
  olderThanDays: number;
  maxCaches?: number | undefined;
  dryRun: boolean;
  refresh: () => Promise<void> | void;
}): Promise<PruneRemoteCachesAndRefreshResult> {
  const { manager, olderThanDays, maxCaches, dryRun, refresh } = options;

  if (manager.backend !== "opfs") {
    return { supported: false, message: `Remote cache pruning is not supported for backend: ${manager.backend}` };
  }

  const olderThanMs = computeOlderThanMsFromDays(olderThanDays);

  const base: { olderThanMs: number; maxCaches?: number } = { olderThanMs };
  if (maxCaches !== undefined) base.maxCaches = maxCaches;
  const result = dryRun ? await manager.pruneRemoteCaches({ ...base, dryRun: true }) : await manager.pruneRemoteCaches(base);

  if (!dryRun) {
    await refresh();
  }

  return { supported: true, result };
}
