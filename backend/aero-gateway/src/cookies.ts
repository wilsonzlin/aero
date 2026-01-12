import type { IncomingMessage, ServerResponse } from 'node:http';

export interface CookieOptions {
  httpOnly?: boolean;
  maxAgeSeconds?: number;
  path?: string;
  sameSite?: 'Lax' | 'Strict' | 'None';
  secure?: boolean;
}

export function isRequestSecure(req: IncomingMessage, opts: { trustProxy: boolean }): boolean {
  const socketMaybeTls = req.socket as unknown as { encrypted?: boolean };
  if (socketMaybeTls.encrypted) return true;

  if (!opts.trustProxy) return false;

  const header = req.headers['x-forwarded-proto'];
  const raw = Array.isArray(header) ? header[0] : header;
  if (!raw) return false;

  return isForwardedProtoHttps(raw);
}

function isForwardedProtoHttps(raw: string): boolean {
  // Use only the first token in the X-Forwarded-Proto list.
  // RFC 7239 OWS is SP / HTAB, but accept any ASCII <= 0x20 as trimming whitespace.
  let start = 0;
  let end = raw.length;

  while (start < end && raw.charCodeAt(start) <= 0x20) {
    start += 1;
  }

  const comma = raw.indexOf(',', start);
  if (comma !== -1) end = comma;

  while (end > start && raw.charCodeAt(end - 1) <= 0x20) {
    end -= 1;
  }

  if (end - start !== 5) return false;

  // ASCII case-insensitive compare to "https" without allocating a lowercase copy.
  // https
  let c0 = raw.charCodeAt(start);
  let c1 = raw.charCodeAt(start + 1);
  let c2 = raw.charCodeAt(start + 2);
  let c3 = raw.charCodeAt(start + 3);
  let c4 = raw.charCodeAt(start + 4);

  if (c0 >= 0x41 && c0 <= 0x5a) c0 += 0x20;
  if (c1 >= 0x41 && c1 <= 0x5a) c1 += 0x20;
  if (c2 >= 0x41 && c2 <= 0x5a) c2 += 0x20;
  if (c3 >= 0x41 && c3 <= 0x5a) c3 += 0x20;
  if (c4 >= 0x41 && c4 <= 0x5a) c4 += 0x20;

  return c0 === 0x68 /* 'h' */ && c1 === 0x74 /* 't' */ && c2 === 0x74 /* 't' */ && c3 === 0x70 /* 'p' */ &&
    c4 === 0x73 /* 's' */;
}

export function serializeCookie(name: string, value: string, options: CookieOptions = {}): string {
  const parts = [`${name}=${encodeURIComponent(value)}`];

  if (options.maxAgeSeconds !== undefined) {
    parts.push(`Max-Age=${options.maxAgeSeconds}`);
  }
  parts.push(`Path=${options.path ?? '/'}`);

  if (options.httpOnly) {
    parts.push('HttpOnly');
  }
  if (options.sameSite) {
    parts.push(`SameSite=${options.sameSite}`);
  }
  if (options.secure) {
    parts.push('Secure');
  }

  return parts.join('; ');
}

export function appendSetCookieHeader(res: ServerResponse, cookie: string): void {
  const current = res.getHeader('Set-Cookie');
  if (current === undefined) {
    res.setHeader('Set-Cookie', cookie);
    return;
  }

  const cookies = Array.isArray(current) ? current.map(String) : [String(current)];
  cookies.push(cookie);
  res.setHeader('Set-Cookie', cookies);
}

export function getCookieValue(cookieHeader: string | string[] | undefined, name: string): string | undefined {
  if (!cookieHeader) return undefined;
  const raw = Array.isArray(cookieHeader) ? cookieHeader.join(';') : cookieHeader;
  const parts = raw.split(';');
  for (const part of parts) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf('=');
    if (idx <= 0) continue;
    const key = trimmed.slice(0, idx).trim();
    if (key !== name) continue;
    const value = trimmed.slice(idx + 1).trim();
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }
  return undefined;
}
