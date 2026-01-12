export function assertNonSecretUrl(url: string | undefined): void {
  if (!url) return;
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

  const banned = new Set([
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

  for (const [key] of parsed.searchParams) {
    if (banned.has(key.toLowerCase())) {
      throw new Error("Refusing to persist what looks like a signed URL; store a stable URL or a leaseEndpoint instead.");
    }
  }
}

export function assertValidLeaseEndpoint(endpoint: string | undefined): void {
  if (!endpoint) return;
  if (!endpoint.startsWith("/")) {
    throw new Error("leaseEndpoint must be a same-origin path starting with '/'");
  }
}

