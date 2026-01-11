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
  // Reject empty port specs like `https://example.com:` or `https://example.com:/`.
  if (trimmed.endsWith(':') || trimmed.endsWith(':/')) return null;

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return null;
  }

  if (!['http:', 'https:'].includes(url.protocol)) return null;
  if (url.username !== '' || url.password !== '') return null;
  if (url.search !== '' || url.hash !== '') return null;
  if (url.pathname !== '/' && url.pathname !== '') return null;
  if (!url.hostname) return null;

  const scheme = url.protocol.slice(0, -1).toLowerCase();
  const hostname = url.hostname.toLowerCase();
  let port = url.port;
  if (port === '0') return null;
  if (port === '80' && scheme === 'http') port = '';
  if (port === '443' && scheme === 'https') port = '';

  return `${scheme}://${port ? `${hostname}:${port}` : hostname}`;
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
