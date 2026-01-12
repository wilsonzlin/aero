import type { DiskAccessLease } from "../storage/disk_access_lease";
import { readJsonResponseWithLimit } from "../storage/response_json";

const MAX_IMAGE_GATEWAY_JSON_BYTES = 1024 * 1024; // 1 MiB

export type ImageGatewaySignedCookie = {
  name: string;
  value: string;
  attributes: string[];
};

export type ImageGatewayStreamAuth =
  | { type: "cookie"; expiresAt: string; cookies: ImageGatewaySignedCookie[] }
  | { type: "url"; expiresAt: string }
  | { type: "none" };

export type ImageGatewayStreamUrlResponse = {
  url: string;
  auth: ImageGatewayStreamAuth;
  size?: number | null;
  etag?: string | null;
};

export type ImageGatewayImageMetadataResponse = {
  url: string;
  size?: number | null;
  etag?: string | null;
  lastModified?: string | null;
};

export type ImageGatewayClientOptions = {
  /**
   * Base URL for the image-gateway service.
   *
   * Defaults to "" (same-origin).
   */
  baseUrl?: string;
  /**
   * Optional fetch override for tests.
   */
  fetch?: typeof fetch;
  /**
   * Extra headers to send on every request (e.g. X-User-Id for dev auth mode).
   */
  headers?: Record<string, string>;
  /**
   * Credentials mode for image-gateway API calls. Defaults to `include` so that
   * Set-Cookie responses (CloudFront signed cookies) work cross-origin.
   */
  credentialsMode?: RequestCredentials;
};

export class ImageGatewayClient {
  private readonly baseUrl: string;
  private readonly fetchFn: typeof fetch;
  private readonly baseHeaders: Record<string, string>;
  private readonly credentialsMode: RequestCredentials;

  constructor(options: ImageGatewayClientOptions = {}) {
    this.baseUrl = options.baseUrl ?? "";
    this.fetchFn = options.fetch ?? fetch;
    this.baseHeaders = options.headers ?? {};
    this.credentialsMode = options.credentialsMode ?? "include";
  }

  async getImageMetadata(imageId: string): Promise<ImageGatewayImageMetadataResponse> {
    const url = this.buildUrl(`/v1/images/${encodeURIComponent(imageId)}/metadata`);
    const resp = await this.fetchFn(url, {
      method: "GET",
      headers: this.baseHeaders,
      credentials: this.credentialsMode,
    });
    if (!resp.ok) {
      throw new Error(`image-gateway metadata failed (${resp.status})`);
    }
    return (await readJsonResponseWithLimit(resp, {
      maxBytes: MAX_IMAGE_GATEWAY_JSON_BYTES,
      label: "image-gateway metadata response",
    })) as ImageGatewayImageMetadataResponse;
  }

  async getStreamUrl(imageId: string): Promise<ImageGatewayStreamUrlResponse> {
    const url = this.buildUrl(`/v1/images/${encodeURIComponent(imageId)}/stream-url`);
    const resp = await this.fetchFn(url, {
      method: "GET",
      headers: this.baseHeaders,
      credentials: this.credentialsMode,
    });
    if (!resp.ok) {
      throw new Error(`image-gateway stream-url failed (${resp.status})`);
    }
    return (await readJsonResponseWithLimit(resp, {
      maxBytes: MAX_IMAGE_GATEWAY_JSON_BYTES,
      label: "image-gateway stream-url response",
    })) as ImageGatewayStreamUrlResponse;
  }

  /**
   * Convenience: creates a `DiskAccessLease` backed by `/stream-url`.
   *
   * The lease is memory-only; callers should persist only stable identifiers (e.g. `imageId`,
   * or the stable URL from `/metadata`).
   */
  async createDiskAccessLease(imageId: string): Promise<DiskAccessLease> {
    const lease = new ImageGatewayDiskAccessLease(this, imageId);
    await lease.refresh();
    return lease;
  }

  private buildUrl(path: string): string {
    if (!this.baseUrl) return path;
    return new URL(path, this.baseUrl).toString();
  }
}

function credentialsModeFromAuth(auth: ImageGatewayStreamAuth): RequestCredentials {
  if (auth.type === "cookie") return "include";
  return "omit";
}

function expiresAtFromAuth(auth: ImageGatewayStreamAuth): Date | undefined {
  if (auth.type === "cookie" || auth.type === "url") {
    const parsed = new Date(auth.expiresAt);
    if (Number.isFinite(parsed.getTime())) return parsed;
  }
  return undefined;
}

class ImageGatewayDiskAccessLease implements DiskAccessLease {
  url = "";
  expiresAt?: Date;
  credentialsMode: RequestCredentials = "omit";

  private readonly client: ImageGatewayClient;
  private readonly imageId: string;
  private refreshPromise: Promise<DiskAccessLease> | null = null;

  constructor(client: ImageGatewayClient, imageId: string) {
    this.client = client;
    this.imageId = imageId;
  }

  refresh(): Promise<DiskAccessLease> {
    const existing = this.refreshPromise;
    if (existing) return existing;

    const task = (async () => {
      const res = await this.client.getStreamUrl(this.imageId);
      this.url = res.url;
      this.credentialsMode = credentialsModeFromAuth(res.auth);
      this.expiresAt = expiresAtFromAuth(res.auth);
      return this;
    })();

    this.refreshPromise = task.finally(() => {
      this.refreshPromise = null;
    });

    return this.refreshPromise;
  }
}
