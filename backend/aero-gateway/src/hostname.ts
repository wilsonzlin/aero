import { domainToASCII } from 'node:url';

export function normalizeHostname(input: string): string | null {
  const trimmed = input.trim();
  if (trimmed.length === 0) return null;

  const withoutTrailingDot = trimmed.endsWith('.') ? trimmed.slice(0, -1) : trimmed;
  if (withoutTrailingDot.length === 0) return null;

  const lower = withoutTrailingDot.toLowerCase();
  const ascii = domainToASCII(lower);
  if (ascii.length === 0) return null;
  if (ascii.length > 253) return null;

  const labels = ascii.split('.');
  if (labels.some((l) => l.length === 0 || l.length > 63)) return null;

  if (labels.some((l) => !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(l))) return null;

  return ascii;
}

export function matchHostnamePattern(hostname: string, pattern: string): boolean {
  const normHost = normalizeHostname(hostname);
  if (!normHost) return false;

  const trimmedPattern = pattern.trim().toLowerCase();
  if (trimmedPattern.startsWith('*.')) {
    const normBase = normalizeHostname(trimmedPattern.slice(2));
    if (!normBase) return false;
    return normHost !== normBase && normHost.endsWith(`.${normBase}`);
  }

  const normPattern = normalizeHostname(trimmedPattern);
  if (!normPattern) return false;
  return normHost === normPattern;
}
