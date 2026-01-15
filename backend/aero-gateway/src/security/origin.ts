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

  const at = trimmed.indexOf('@', start);
  return at !== -1 && at < end;
}

// Conservative cap to avoid spending unbounded CPU on attacker-controlled headers.
// Browser Origin headers are small; anything larger is almost certainly malicious.
const MAX_ORIGIN_HEADER_LEN = 4096;

function asciiStartsWithIgnoreCase(s: string, prefix: string): boolean {
  if (s.length < prefix.length) return false;
  for (let i = 0; i < prefix.length; i++) {
    let c = s.charCodeAt(i);
    // ASCII upper -> lower
    if (c >= 0x41 && c <= 0x5a) c += 0x20;
    if (c !== prefix.charCodeAt(i)) return false;
  }
  return true;
}

function isValidOriginHeaderString(trimmed: string): boolean {
  // Browser Origin is an ASCII serialization (RFC 6454 / WHATWG URL). Be strict:
  // reject any non-printable/non-ASCII characters that URL parsers may otherwise
  // normalize away (e.g. tabs/newlines/zero-width chars).
  //
  // Also reject characters that different URL libraries treat inconsistently or
  // that browsers never emit in Origin headers.
  for (let i = 0; i < trimmed.length; i += 1) {
    const c = trimmed.charCodeAt(i);
    if (c <= 0x20 || c >= 0x7f) return false;
    // Disallow percent-encoding and IPv6 zone identifiers; browsers don't emit
    // these in Origin, and different URL libraries disagree on how to handle them.
    if (c === 0x25 /* '%' */) return false;
    // Reject comma-delimited values. Browsers send a single Origin serialization,
    // but some HTTP stacks may join repeated headers with commas.
    if (c === 0x2c /* ',' */) return false;
    // Some URL libraries (notably Go's net/url) reject additional host codepoints
    // that WHATWG URL parsers accept. Reject them here so Origin validation stays
    // consistent across components.
    if (
      c === 0x7b /* '{' */ ||
      c === 0x7d /* '}' */ ||
      c === 0x5c /* '\\' */ ||
      c === 0x60 /* '`' */
    ) {
      return false;
    }
    // Reject query and fragment delimiters even when empty. WHATWG URL parsers
    // normalize `https://example.com?` or `https://example.com#` to the same origin,
    // but browsers don't emit those in Origin headers.
    if (c === 0x3f /* '?' */ || c === 0x23 /* '#' */) return false;
  }
  return true;
}

export function normalizeOriginString(origin: string): string | null {
  const trimmed = origin.trim();
  if (trimmed === '') return null;
  if (trimmed.length > MAX_ORIGIN_HEADER_LEN) return null;
  if (trimmed === 'null') return 'null';
  if (!isValidOriginHeaderString(trimmed)) return null;
  // Require an explicit scheme://host serialization; WHATWG URL parsers accept
  // weird variants like `https:example.com` and will normalize them to an
  // authority URL, but browsers don't emit those in Origin headers.
  // Avoid allocating a lowercase copy of the full Origin header; we only need a
  // case-insensitive check for the scheme prefix.
  const schemePrefix = asciiStartsWithIgnoreCase(trimmed, 'http://')
    ? 'http://'
    : asciiStartsWithIgnoreCase(trimmed, 'https://')
      ? 'https://'
      : null;
  if (!schemePrefix) return null;
  if (trimmed.charAt(schemePrefix.length) === '/') return null;
  // Allow an optional trailing slash, but reject any other path segments.
  // WHATWG URL parsers normalize dot segments (e.g. "/." or "/..") to "/",
  // which could cause us to accept non-origin strings in allowlist checks.
  const pathStart = trimmed.indexOf('/', schemePrefix.length);
  if (pathStart !== -1 && pathStart !== trimmed.length - 1) return null;
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
