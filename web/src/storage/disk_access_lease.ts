/**
 * Refreshable lease for accessing remote disk bytes (e.g. signed URLs).
 *
 * Canonical trait note:
 *
 * This interface is part of the async browser “host layer” around remote disk delivery. It is
 * not a disk abstraction itself; the canonical TS disk interface is `AsyncSectorDisk`.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */

import { readJsonResponseWithLimit } from "./response_json";
import { unrefBestEffort } from "../unrefSafe";
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
// Defensive bound for lease API responses. These should be tiny, but keep a cap to avoid
// pathological allocations if the endpoint is misconfigured or attacker-controlled.
export const MAX_STREAM_LEASE_JSON_BYTES = 1024 * 1024; // 1 MiB
// In both browsers and Node, `setTimeout()` has an effective maximum delay of
// ~2^31-1 ms (~24.8 days). Passing a larger value can overflow/clamp and cause the
// callback to run immediately (or very soon), potentially hammering the lease
// refresh endpoint if a buggy/misconfigured server returns a far-future `expiresAt`.
export const MAX_TIMEOUT_MS = 2_147_483_647;

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
    if (delayMs > MAX_TIMEOUT_MS) {
      // `setTimeout` can't represent this delay. Instead, schedule a "check timer" for the
      // maximum supported delay and re-run scheduling when it fires.
      this.timerId = setTimeout(() => {
        this.timerId = null;
        this.schedule();
      }, MAX_TIMEOUT_MS);
      unrefBestEffort(this.timerId);
      return;
    }

    this.timerId = setTimeout(() => {
      this.timerId = null;
      void this.runRefresh();
    }, delayMs);
    unrefBestEffort(this.timerId);
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
      unrefBestEffort(this.timerId);
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

export type StreamLeaseResponse = {
  /**
   * Signed URL for Range-based delivery ("delivery=range").
   *
   * NOTE: This is an ephemeral secret and MUST NOT be persisted (IndexedDB/OPFS/localStorage).
   */
  url: string;
  /**
   * Optional RFC 3339 / ISO 8601 timestamp indicating when the lease expires.
   * (See: docs/16-disk-image-streaming-auth.md#example-lease-api-response-schema)
   */
  expiresAt?: string;
  /**
   * Optional chunked disk delivery data (for "delivery=chunked").
   */
  chunked?: { delivery?: string; manifestUrl?: string };
};

function requireNonEmptyString(value: unknown, label: string): string {
  if (typeof value !== "string") {
    throw new Error(`${label} must be a string`);
  }
  const trimmed = value.trim();
  if (!trimmed) {
    throw new Error(`${label} must be a non-empty string`);
  }
  return trimmed;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function parseStreamLeaseResponse(raw: unknown): StreamLeaseResponse {
  if (!isRecord(raw)) {
    throw new Error("stream lease response must be a JSON object");
  }
  const obj = raw as Record<string, unknown>;

  const url = requireNonEmptyString(hasOwn(obj, "url") ? obj.url : undefined, "stream lease response url");

  const expiresAt =
    hasOwn(obj, "expiresAt")
      ? obj.expiresAt === undefined
        ? undefined
        : requireNonEmptyString(obj.expiresAt, "stream lease response expiresAt")
      : undefined;

  let chunked: StreamLeaseResponse["chunked"] | undefined;
  if (hasOwn(obj, "chunked")) {
    const chunkedRaw = obj.chunked;
    if (!isRecord(chunkedRaw)) {
      throw new Error("stream lease response chunked must be an object");
    }
    const ch = chunkedRaw as Record<string, unknown>;
    const delivery =
      hasOwn(ch, "delivery")
        ? ch.delivery === undefined
          ? undefined
          : requireNonEmptyString(ch.delivery, "stream lease response chunked.delivery")
        : undefined;
    const manifestUrl =
      hasOwn(ch, "manifestUrl")
        ? ch.manifestUrl === undefined
          ? undefined
          : requireNonEmptyString(ch.manifestUrl, "stream lease response chunked.manifestUrl")
        : undefined;
    const chunkedOut = Object.create(null) as NonNullable<StreamLeaseResponse["chunked"]>;
    if (delivery !== undefined) chunkedOut.delivery = delivery;
    if (manifestUrl !== undefined) chunkedOut.manifestUrl = manifestUrl;
    chunked = chunkedOut;
  }

  // Return a null-prototype object to ensure callers don't observe values inherited
  // from `Object.prototype` (prototype pollution).
  const out = Object.create(null) as StreamLeaseResponse;
  out.url = url;
  if (expiresAt !== undefined) out.expiresAt = expiresAt;
  if (chunked !== undefined) out.chunked = chunked;
  return out;
}

function resolveUrlWithOptionalLocationBase(input: string): URL | null {
  const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
  try {
    return base ? new URL(input, base) : new URL(input);
  } catch {
    return null;
  }
}

function resolveForFetch(input: string): string {
  // In browsers, `fetch()` accepts relative URLs. In Node (tests), it does not.
  // If we have a `location.href` available, resolve relative inputs against it.
  const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
  if (base) {
    try {
      return new URL(input, base).toString();
    } catch {
      // Fall through (fetch will likely throw an Invalid URL error).
    }
  }
  return input;
}

function isSameOriginOrRelative(url: string): boolean {
  const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
  if (base) {
    const u = resolveUrlWithOptionalLocationBase(url);
    if (!u) return false;
    return u.origin === new URL(base).origin;
  }

  // Without `location`, we can't compare origins. Treat non-absolute URLs as "relative"
  // and therefore safe to use `credentials: "same-origin"`.
  try {
    // If this succeeds, it's an absolute URL.
    new URL(url);
    return false;
  } catch {
    return true;
  }
}

async function fetchStreamLease(endpoint: string, fetchFn: typeof fetch): Promise<StreamLeaseResponse> {
  const parsedEndpoint = resolveUrlWithOptionalLocationBase(endpoint);
  const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
  if (parsedEndpoint && base) {
    const origin = new URL(base).origin;
    if (parsedEndpoint.origin !== origin) {
      // Keep error messages stable; do not reflect attacker-controlled origins.
      throw new Error("leaseEndpoint must be same-origin");
    }
  }

  const resp = await fetchFn(resolveForFetch(endpoint), { method: "GET", credentials: "same-origin" });
  if (!resp.ok) {
    throw new Error(`failed to fetch stream lease: ${resp.status}`);
  }
  const json = await readJsonResponseWithLimit(resp, { maxBytes: MAX_STREAM_LEASE_JSON_BYTES, label: "stream lease response" });
  return parseStreamLeaseResponse(json);
}

/**
 * Builds a refreshable `DiskAccessLease` from a same-origin API endpoint.
 *
 * This is intended for situations where DiskManager metadata cannot store a stable URL
 * (because all data-plane URLs are short-lived signed URLs).
 *
 * SECURITY: When the lease refresh returns a URL that is not same-origin, we default to
 * `credentials: "omit"` so we never leak ambient cookies to third-party origins. For
 * same-origin (or relative) URLs, we use `same-origin` so cookie-based auth still works.
 */
export function createDiskAccessLeaseFromLeaseEndpoint(
  leaseEndpoint: string,
  options: { delivery: "range" | "chunked"; fetchFn?: typeof fetch },
): DiskAccessLease {
  const endpoint = leaseEndpoint.trim();
  if (!endpoint) {
    throw new Error("leaseEndpoint must not be empty");
  }

  const fetchFn = options.fetchFn ?? fetch;
  const delivery = options.delivery;

  let inflight: Promise<DiskAccessLease> | null = null;
  const lease: DiskAccessLease = {
    url: "",
    expiresAt: undefined,
    credentialsMode: "same-origin",
    async refresh() {
      if (inflight) return await inflight;
      inflight = (async () => {
        const resp = await fetchStreamLease(endpoint, fetchFn);

        const nextUrl =
          delivery === "range"
            ? resp.url
            : (() => {
                const chunked = resp.chunked;
                if (!chunked || typeof chunked.manifestUrl !== "string" || !chunked.manifestUrl.trim()) {
                  throw new Error("stream lease response missing chunked.manifestUrl");
                }
                if (chunked.delivery !== undefined && chunked.delivery !== "chunked") {
                  throw new Error(`unexpected stream lease chunked.delivery=${String(chunked.delivery)}`);
                }
                return chunked.manifestUrl;
              })();

        lease.url = nextUrl.trim();

        if (resp.expiresAt !== undefined) {
          const date = new Date(resp.expiresAt);
          if (!Number.isFinite(date.getTime())) {
            throw new Error("stream lease response expiresAt is not a valid timestamp");
          }
          lease.expiresAt = date;
        } else {
          lease.expiresAt = undefined;
        }

        lease.credentialsMode = isSameOriginOrRelative(lease.url) ? "same-origin" : "omit";
        return lease;
      })();

      try {
        return await inflight;
      } finally {
        inflight = null;
      }
    },
  };

  return lease;
}
