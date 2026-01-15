export type DecodeBase64UrlOptions = Readonly<{
  // When true, rejects non-canonical base64url-no-pad inputs where "unused bits" are non-zero.
  //
  // Note: This is stricter than Node's decoder and will reject base64url *prefixes* of a longer
  // encoding (useful for auth tokens, but not for best-effort prefix parsing).
  canonical?: boolean;
}>;

export function maxBase64UrlLenForBytes(byteLength: number): number {
  // Treat non-finite values as 0 and clamp huge finite values to Number.MAX_SAFE_INTEGER to avoid
  // float precision issues.
  const nRaw = Number.isFinite(byteLength) ? Math.floor(byteLength) : 0;
  const n = Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, nRaw));
  const fullTriplets = Math.floor(n / 3);
  // Keep the return value a safe integer so comparisons are predictable.
  const maxFullTriplets = Math.floor(Number.MAX_SAFE_INTEGER / 4);
  if (fullTriplets > maxFullTriplets) return Number.MAX_SAFE_INTEGER;

  const rem = n % 3;
  let len = fullTriplets * 4;
  if (rem === 1) len += 2;
  else if (rem === 2) len += 3;
  return len > Number.MAX_SAFE_INTEGER ? Number.MAX_SAFE_INTEGER : len;
}

export function base64UrlPrefixForHeader(base64url: string, maxChars = 16): string {
  let len = Math.min(base64url.length, maxChars);
  if (len % 4 === 1) len -= 1;
  if (len <= 0) return "";
  return base64url.slice(0, len);
}

export function isBase64UrlNoPad(raw: string): boolean {
  if (raw.length === 0) return false;
  for (let i = 0; i < raw.length; i += 1) {
    const c = raw.charCodeAt(i);
    const isUpper = c >= 0x41 /* 'A' */ && c <= 0x5a /* 'Z' */;
    const isLower = c >= 0x61 /* 'a' */ && c <= 0x7a /* 'z' */;
    const isDigit = c >= 0x30 /* '0' */ && c <= 0x39 /* '9' */;
    const isDash = c === 0x2d /* '-' */;
    const isUnderscore = c === 0x5f /* '_' */;
    if (!isUpper && !isLower && !isDigit && !isDash && !isUnderscore) return false;
  }
  return true;
}

function b64urlValue(c: number): number | null {
  if (c >= 0x41 && c <= 0x5a) return c - 0x41;
  if (c >= 0x61 && c <= 0x7a) return c - 0x61 + 26;
  if (c >= 0x30 && c <= 0x39) return c - 0x30 + 52;
  if (c === 0x2d /* '-' */) return 62;
  if (c === 0x5f /* '_' */) return 63;
  return null;
}

export function isCanonicalBase64UrlNoPad(raw: string): boolean {
  if (!isBase64UrlNoPad(raw)) return false;
  const mod = raw.length % 4;
  if (mod === 1) return false;
  if (mod === 0) return true;

  const last = raw.charCodeAt(raw.length - 1);
  const v = b64urlValue(last);
  if (v === null) return false;

  if (mod === 2) return (v & 0x0f) === 0;
  return (v & 0x03) === 0;
}

export function decodeBase64UrlToBuffer(base64url: string, opts: DecodeBase64UrlOptions = {}): Buffer {
  if (!isBase64UrlNoPad(base64url)) throw new Error("Invalid base64url");
  if (base64url.length % 4 === 1) throw new Error("Invalid base64url length");
  if (opts.canonical === true && !isCanonicalBase64UrlNoPad(base64url)) {
    throw new Error("Invalid base64url");
  }
  return Buffer.from(base64url, "base64url");
}
