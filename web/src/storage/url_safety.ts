import { MAX_REMOTE_LEASE_ENDPOINT_LEN, MAX_REMOTE_URL_LEN } from "./url_limits";

const BANNED_SIGNED_URL_PARAM_KEYS_LOWER = new Set([
  // AWS S3 presigned query params.
  "x-amz-algorithm",
  "x-amz-credential",
  "x-amz-date",
  "x-amz-expires",
  "x-amz-security-token",
  "x-amz-signature",
  "x-amz-signedheaders",
  // CloudFront signed URL params (and other common CDNs).
  "expires",
  "key-pair-id",
  "policy",
  "signature",
]);

export function assertNonSecretUrl(url: string | undefined): void {
  if (!url) return;
  if (url.length > MAX_REMOTE_URL_LEN) {
    throw new Error("Refusing to persist a URL that is too long; store a stable shorter URL or use a leaseEndpoint.");
  }
  let parsed: URL;
  try {
    parsed = new URL(url, "https://example.invalid");
  } catch {
    // If URL parsing fails, fall back to best-effort substring checks.
    const lower = url.toLowerCase();
    if (lower.includes("x-amz-signature") || lower.includes("key-pair-id=") || lower.includes("signature=")) {
      throw new Error("Refusing to persist what looks like a signed URL; store a stable URL or a leaseEndpoint instead.");
    }
    return;
  }

  if (parsed.username || parsed.password) {
    throw new Error("Refusing to persist a URL with embedded credentials; store a stable URL or a leaseEndpoint instead.");
  }

  for (const [key] of parsed.searchParams) {
    if (BANNED_SIGNED_URL_PARAM_KEYS_LOWER.has(key.toLowerCase())) {
      throw new Error("Refusing to persist what looks like a signed URL; store a stable URL or a leaseEndpoint instead.");
    }
  }
}

export function assertValidLeaseEndpoint(endpoint: string | undefined): void {
  if (!endpoint) return;
  if (endpoint.length > MAX_REMOTE_LEASE_ENDPOINT_LEN) {
    throw new Error(`leaseEndpoint is too long (max ${MAX_REMOTE_LEASE_ENDPOINT_LEN})`);
  }
  if (!endpoint.startsWith("/")) {
    throw new Error("leaseEndpoint must be a same-origin path starting with '/'");
  }
  // `//example.com` is a protocol-relative URL (cross-origin). Disallow it even though it starts
  // with `/`.
  if (endpoint.startsWith("//")) {
    throw new Error("leaseEndpoint must not start with '//'");
  }
  if (endpoint.includes("\u0000")) {
    throw new Error("leaseEndpoint must not contain NUL bytes");
  }
  // Defensive: disallow embedded absolute URLs in query params (e.g. `/lease?url=https://...`).
  // This value is persisted and must remain stable + non-secret.
  const lower = endpoint.toLowerCase();
  if (lower.includes("http:") || lower.includes("https:")) {
    throw new Error("leaseEndpoint must not contain http:/https:");
  }
}
