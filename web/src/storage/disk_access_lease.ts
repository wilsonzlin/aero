export interface DiskAccessLease {
  /**
   * Ephemeral URL to fetch bytes from. For signed-URL auth, this includes the signature
   * and must NOT be persisted anywhere (OPFS/IndexedDB/localStorage).
   */
  url: string;
  /**
   * Optional expiration timestamp for the lease. When present, callers should proactively
   * refresh before this time to avoid 401/403 mid-stream.
   */
  expiresAt?: Date;
  /**
   * `fetch` credentials mode to use when fetching the URL.
   * - CloudFront signed cookies require `include`.
   * - Signed URLs should use `omit` to avoid leaking ambient cookies.
   */
  credentialsMode: RequestCredentials;
  /**
   * Refreshes the lease by calling the auth service again.
   *
   * Implementations may mutate `url`/`expiresAt`/`credentialsMode` in-place.
   */
  refresh(): Promise<DiskAccessLease>;
}

export const DEFAULT_LEASE_REFRESH_MARGIN_MS = 60_000;
const LEASE_REFRESH_FAILURE_RETRY_MS = 10_000;

export function computeLeaseRefreshDelayMs(
  expiresAt: Date,
  nowMs: number,
  refreshMarginMs: number,
): number {
  const expiryMs = expiresAt.getTime();
  if (!Number.isFinite(expiryMs)) return 0;
  const delay = expiryMs - nowMs - refreshMarginMs;
  return Math.max(0, delay);
}

/**
 * Proactively refreshes a lease shortly before `expiresAt`.
 *
 * The refresher is intentionally decoupled from any disk implementation so it can be shared
 * between Range-based and chunked remote disk readers.
 */
export class DiskAccessLeaseRefresher {
  private readonly lease: DiskAccessLease;
  private readonly refreshMarginMs: number;
  private timerId: ReturnType<typeof setTimeout> | null = null;
  private stopped = false;

  constructor(lease: DiskAccessLease, options?: { refreshMarginMs?: number }) {
    this.lease = lease;
    this.refreshMarginMs = options?.refreshMarginMs ?? DEFAULT_LEASE_REFRESH_MARGIN_MS;
  }

  start(): void {
    this.stopped = false;
    this.schedule();
  }

  stop(): void {
    this.stopped = true;
    if (this.timerId !== null) {
      clearTimeout(this.timerId);
      this.timerId = null;
    }
  }

  private schedule(): void {
    if (this.stopped) return;
    if (this.timerId !== null) {
      clearTimeout(this.timerId);
      this.timerId = null;
    }

    const expiresAt = this.lease.expiresAt;
    if (!expiresAt) return;

    const delayMs = computeLeaseRefreshDelayMs(expiresAt, Date.now(), this.refreshMarginMs);
    this.timerId = setTimeout(() => {
      this.timerId = null;
      void this.runRefresh();
    }, delayMs);
  }

  private async runRefresh(): Promise<void> {
    if (this.stopped) return;
    try {
      await this.lease.refresh();
    } catch {
      if (this.stopped) return;
      // Best-effort: avoid a tight refresh loop if the auth service is temporarily down.
      this.timerId = setTimeout(() => {
        this.timerId = null;
        void this.runRefresh();
      }, LEASE_REFRESH_FAILURE_RETRY_MS);
      return;
    }

    this.schedule();
  }
}

export async function fetchWithDiskAccessLeaseForUrl(
  lease: DiskAccessLease,
  url: string | (() => string),
  init: RequestInit,
  options?: { fetch?: typeof fetch; retryAuthOnce?: boolean },
): Promise<Response> {
  const fetchFn = options?.fetch ?? fetch;
  const retryAuthOnce = options?.retryAuthOnce ?? true;
  const urlFn = typeof url === "function" ? url : () => url;

  const doFetch = () =>
    fetchFn(urlFn(), {
      ...init,
      credentials: lease.credentialsMode,
    });

  const resp = await doFetch();
  if (!retryAuthOnce || (resp.status !== 401 && resp.status !== 403)) {
    return resp;
  }

  await lease.refresh();
  return await doFetch();
}

export async function fetchWithDiskAccessLease(
  lease: DiskAccessLease,
  init: RequestInit,
  options?: { fetch?: typeof fetch; retryAuthOnce?: boolean },
): Promise<Response> {
  return await fetchWithDiskAccessLeaseForUrl(lease, () => lease.url, init, options);
}
