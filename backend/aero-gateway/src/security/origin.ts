function authorityHasUserinfo(raw: string): boolean {
  const trimmed = raw.trim();
  const schemeIdx = trimmed.indexOf('://');
  if (schemeIdx === -1) return false;

  const start = schemeIdx + 3;
  let end = trimmed.length;
  for (const sep of ['/', '?', '#'] as const) {
    const idx = trimmed.indexOf(sep, start);
    if (idx !== -1 && idx < end) end = idx;
  }

  return trimmed.slice(start, end).includes('@');
}

export function normalizeOriginString(origin: string): string | null {
  const trimmed = origin.trim();
  if (trimmed === '') return null;
  if (trimmed === 'null') return 'null';
  // Browser Origin is an ASCII serialization (RFC 6454 / WHATWG URL). Be strict:
  // reject any non-printable/non-ASCII characters that URL parsers may otherwise
  // normalize away (e.g. tabs/newlines/zero-width chars).
  if (!/^[\x21-\x7E]+$/.test(trimmed)) return null;
  // Disallow percent-encoding and IPv6 zone identifiers; browsers don't emit
  // these in Origin, and different URL libraries disagree on how to handle them.
  if (trimmed.includes('%')) return null;
  // Reject comma-delimited values. Browsers send a single Origin serialization,
  // but some HTTP stacks may join repeated headers with commas.
  if (trimmed.includes(',')) return null;
  // Require an explicit scheme://host serialization; WHATWG URL parsers accept
  // weird variants like `https:example.com` and will normalize them to an
  // authority URL, but browsers don't emit those in Origin headers.
  const lower = trimmed.toLowerCase();
  const schemePrefix = lower.startsWith('http://') ? 'http://' : lower.startsWith('https://') ? 'https://' : null;
  if (!schemePrefix) return null;
  if (trimmed.charAt(schemePrefix.length) === '/') return null;
  // Allow an optional trailing slash, but reject any other path segments.
  // WHATWG URL parsers normalize dot segments (e.g. "/." or "/..") to "/",
  // which could cause us to accept non-origin strings in allowlist checks.
  const pathStart = trimmed.indexOf('/', schemePrefix.length);
  if (pathStart !== -1 && pathStart !== trimmed.length - 1) return null;
  // Reject backslashes; some URL parsers normalize them to `/`, which can
  // silently change the host/path boundary.
  if (trimmed.includes('\\')) return null;
  // Reject empty port specs like `https://example.com:` or `https://example.com:/`.
  if (trimmed.endsWith(':') || trimmed.endsWith(':/')) return null;

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return null;
  }

  if (!['http:', 'https:'].includes(url.protocol)) return null;
  if (authorityHasUserinfo(trimmed) || url.username !== '' || url.password !== '') return null;
  if (url.search !== '' || url.hash !== '') return null;
  if (url.pathname !== '/' && url.pathname !== '') return null;
  if (!url.hostname) return null;

  if (url.port === '0') return null;

  // `URL.origin` lowercases scheme/host and strips default ports.
  return url.origin;
}

export function normalizeAllowedOriginString(origin: string): string {
  const trimmed = origin.trim();
  if (trimmed === '*' || trimmed === 'null') return trimmed;

  const normalized = normalizeOriginString(trimmed);
  if (!normalized) {
    throw new Error(`Invalid origin "${trimmed}". Expected a full origin like "https://example.com".`);
  }
  return normalized;
}
