export interface StorageEstimateInfo {
  supported: boolean;
  usageBytes: number | null;
  quotaBytes: number | null;
  /** `usageBytes / quotaBytes * 100`, or `null` if unknown. */
  usagePercent: number | null;
  remainingBytes: number | null;
  warning: boolean;
}

export interface GetStorageEstimateOptions {
  /**
   * If `usagePercent >= warningThresholdPercent`, `warning` will be set.
   * Defaults to 80.
   */
  warningThresholdPercent?: number;
}

export interface PersistentStorageInfo {
  supported: boolean;
  /** Current persisted state (`null` if unsupported/unknown). */
  persisted: boolean | null;
}

export interface EnsurePersistentStorageResult extends PersistentStorageInfo {
  /**
   * Whether calling `navigator.storage.persist()` succeeded.
   *
   * - `true`: persistent storage is granted.
   * - `false`: request denied (or an exception occurred).
   * - `null`: request not attempted because the API isn't supported.
   */
  granted: boolean | null;
}

function getStorageManager(): StorageManager | undefined {
  // `navigator` doesn't exist in all runtimes (e.g. Node, some webviews).
  const nav = (globalThis as any).navigator as Navigator | undefined;
  return nav?.storage;
}

export async function getStorageEstimate(
  options: GetStorageEstimateOptions = {},
): Promise<StorageEstimateInfo> {
  const warningThresholdPercent = options.warningThresholdPercent ?? 80;

  const storage = getStorageManager() as StorageManager | undefined;
  if (!storage || typeof storage.estimate !== "function") {
    return {
      supported: false,
      usageBytes: null,
      quotaBytes: null,
      usagePercent: null,
      remainingBytes: null,
      warning: false,
    };
  }

  try {
    const estimate = await storage.estimate();
    const usageBytes = typeof estimate.usage === "number" ? estimate.usage : null;
    const quotaBytes = typeof estimate.quota === "number" ? estimate.quota : null;

    const usagePercent =
      usageBytes !== null && quotaBytes !== null && quotaBytes > 0
        ? (usageBytes / quotaBytes) * 100
        : null;

    const remainingBytes =
      usageBytes !== null && quotaBytes !== null ? Math.max(0, quotaBytes - usageBytes) : null;

    return {
      supported: true,
      usageBytes,
      quotaBytes,
      usagePercent,
      remainingBytes,
      warning: usagePercent !== null && usagePercent >= warningThresholdPercent,
    };
  } catch {
    // The API exists but failed (e.g. blocked in a privacy mode); treat as supported but unknown.
    return {
      supported: true,
      usageBytes: null,
      quotaBytes: null,
      usagePercent: null,
      remainingBytes: null,
      warning: false,
    };
  }
}

export async function getPersistentStorageInfo(): Promise<PersistentStorageInfo> {
  const storage = getStorageManager() as StorageManager | undefined;
  if (!storage || typeof storage.persisted !== "function") {
    return { supported: false, persisted: null };
  }

  try {
    return { supported: true, persisted: await storage.persisted() };
  } catch {
    return { supported: true, persisted: null };
  }
}

export async function ensurePersistentStorage(): Promise<EnsurePersistentStorageResult> {
  const storage = getStorageManager() as StorageManager | undefined;
  if (!storage || typeof storage.persisted !== "function" || typeof storage.persist !== "function") {
    return { supported: false, persisted: null, granted: null };
  }

  try {
    const alreadyPersisted = await storage.persisted();
    if (alreadyPersisted) {
      return { supported: true, persisted: true, granted: true };
    }

    const granted = await storage.persist();
    return { supported: true, persisted: granted, granted };
  } catch {
    return { supported: true, persisted: false, granted: false };
  }
}

