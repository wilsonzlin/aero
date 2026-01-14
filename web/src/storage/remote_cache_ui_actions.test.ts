import { describe, expect, it, vi } from "vitest";

import { computeOlderThanMsFromDays, pruneRemoteCachesAndRefresh } from "./remote_cache_ui_actions";

const DAY_MS = 24 * 60 * 60 * 1000;

describe("computeOlderThanMsFromDays", () => {
  it("clamps to >= 0 and rounds down to whole days", () => {
    expect(computeOlderThanMsFromDays(-1)).toBe(0);
    expect(computeOlderThanMsFromDays(Number.NaN)).toBe(0);
    expect(computeOlderThanMsFromDays(0)).toBe(0);

    expect(computeOlderThanMsFromDays(1)).toBe(1 * DAY_MS);
    expect(computeOlderThanMsFromDays(1.9)).toBe(1 * DAY_MS);
    expect(computeOlderThanMsFromDays(2.1)).toBe(2 * DAY_MS);
  });
});

describe("pruneRemoteCachesAndRefresh", () => {
  it("calls manager.pruneRemoteCaches with correct args for dryRun (and does not refresh)", async () => {
    const pruneRemoteCaches = vi.fn().mockResolvedValue({ pruned: 1, examined: 2, prunedKeys: ["k1"] });
    const refresh = vi.fn();

    const manager = { backend: "opfs", pruneRemoteCaches };

    const res = await pruneRemoteCachesAndRefresh({
      manager,
      olderThanDays: 2,
      maxCaches: 5,
      dryRun: true,
      refresh,
    });

    expect(pruneRemoteCaches).toHaveBeenCalledTimes(1);
    expect(pruneRemoteCaches).toHaveBeenCalledWith({ olderThanMs: 2 * DAY_MS, maxCaches: 5, dryRun: true });
    expect(refresh).not.toHaveBeenCalled();
    expect(res).toEqual({ supported: true, result: { pruned: 1, examined: 2, prunedKeys: ["k1"] } });
  });

  it("calls refresh after a non-dry-run prune", async () => {
    const pruneRemoteCaches = vi.fn().mockResolvedValue({ pruned: 2, examined: 5 });
    const refresh = vi.fn();

    const manager = { backend: "opfs", pruneRemoteCaches };

    const res = await pruneRemoteCachesAndRefresh({
      manager,
      olderThanDays: 2.9,
      dryRun: false,
      refresh,
    });

    expect(pruneRemoteCaches).toHaveBeenCalledTimes(1);
    // Whole-day conversion: 2.9 => 2.
    expect(pruneRemoteCaches).toHaveBeenCalledWith({ olderThanMs: 2 * DAY_MS });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(res).toEqual({ supported: true, result: { pruned: 2, examined: 5 } });
  });
});

