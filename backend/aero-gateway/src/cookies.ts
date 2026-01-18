import type { IncomingMessage, ServerResponse } from 'node:http';

import { iterRawHeaderValues } from './rawHeaders.js';

// Cookie values are attacker-controlled. Keep any decoding/allocation bounded.
const MAX_COOKIE_VALUE_LEN = 4 * 1024;
const MAX_COOKIE_HEADER_VALUE_LEN = 16 * 1024;

export interface CookieOptions {
  httpOnly?: boolean;
  maxAgeSeconds?: number;
  path?: string;
  sameSite?: 'Lax' | 'Strict' | 'None';
  secure?: boolean;
}

function tryGetHeader(res: ServerResponse, name: string): unknown {
  try {
    return res.getHeader(name);
  } catch {
    return undefined;
  }
}

function trySetHeader(res: ServerResponse, name: string, value: string | string[]): boolean {
  try {
    res.setHeader(name, value);
    return true;
  } catch {
    return false;
  }
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
  const current = tryGetHeader(res, 'Set-Cookie');
  if (current === undefined) {
    trySetHeader(res, 'Set-Cookie', cookie);
    return;
  }

  if (typeof current === 'string') {
    trySetHeader(res, 'Set-Cookie', [current, cookie]);
    return;
  }
  if (Array.isArray(current)) {
    const cookies = current.filter((v): v is string => typeof v === 'string');
    cookies.push(cookie);
    trySetHeader(res, 'Set-Cookie', cookies);
    return;
  }

  // Should not happen for Set-Cookie, but don't stringify unexpected header types.
  trySetHeader(res, 'Set-Cookie', cookie);
}

function getCookieValueFromHeaderString(raw: string, name: string): string | undefined {
  if (raw.length === 0 || name.length === 0) return undefined;

  // Scan cookie header without allocating `raw.split(';')`.
  //
  // Cookie header grammar (RFC 6265-ish): `cookie-pair *( ";" SP cookie-pair )`.
  // We accept any ASCII whitespace <= 0x20 as "SP" for robustness.
  let i = 0;
  while (i < raw.length) {
    // Skip separators/whitespace.
    while (i < raw.length) {
      const c = raw.charCodeAt(i);
      if (c !== 0x3b /* ';' */ && c > 0x20) break;
      i += 1;
    }
    if (i >= raw.length) break;

    // Key: scan until '=' or ';'.
    const keyStart = i;
    while (i < raw.length) {
      const c = raw.charCodeAt(i);
      if (c === 0x3d /* '=' */ || c === 0x3b /* ';' */) break;
      i += 1;
    }
    if (i >= raw.length || raw.charCodeAt(i) !== 0x3d /* '=' */) {
      // Malformed segment; skip to next ';'.
      while (i < raw.length && raw.charCodeAt(i) !== 0x3b /* ';' */) i += 1;
      continue;
    }

    // Trim trailing whitespace from the key.
    let keyEnd = i;
    while (keyEnd > keyStart && raw.charCodeAt(keyEnd - 1) <= 0x20) {
      keyEnd -= 1;
    }

    const keyLen = keyEnd - keyStart;
    let keyMatches = keyLen === name.length;
    if (keyMatches) {
      for (let j = 0; j < keyLen; j += 1) {
        if (raw.charCodeAt(keyStart + j) !== name.charCodeAt(j)) {
          keyMatches = false;
          break;
        }
      }
    }

    i += 1; // skip '='

    // Value: skip leading whitespace, then scan to ';' or end.
    while (i < raw.length && raw.charCodeAt(i) <= 0x20) i += 1;
    const valueStart = i;
    while (i < raw.length && raw.charCodeAt(i) !== 0x3b /* ';' */) i += 1;
    let valueEnd = i;
    while (valueEnd > valueStart && raw.charCodeAt(valueEnd - 1) <= 0x20) valueEnd -= 1;

    if (!keyMatches) {
      continue;
    }

    const rawValueLen = valueEnd - valueStart;
    const boundedEnd = rawValueLen > MAX_COOKIE_VALUE_LEN ? valueStart + MAX_COOKIE_VALUE_LEN : valueEnd;
    const value = raw.slice(valueStart, boundedEnd);
    if (value.indexOf('%') === -1) {
      return value;
    }
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }

  return undefined;
}

export function getCookieValue(cookieHeader: string | string[] | undefined, name: string): string | undefined {
  if (!cookieHeader) return undefined;

  if (Array.isArray(cookieHeader)) {
    for (const header of cookieHeader) {
      if (typeof header !== 'string') return undefined;
      const value = getCookieValueFromHeaderString(header, name);
      // Preserve "first cookie wins" semantics, even when the value is an empty string.
      if (value !== undefined) return value;
    }
    return undefined;
  }

  return getCookieValueFromHeaderString(cookieHeader, name);
}

export function getCookieValueFromRequest(req: IncomingMessage, name: string): string | undefined {
  const rawHeaders = (req as unknown as { rawHeaders?: unknown }).rawHeaders;
  for (const header of iterRawHeaderValues(rawHeaders, 'cookie')) {
    // Preserve "first cookie wins" semantics even when the header itself is oversized:
    // treat an oversized Cookie header as invalid (blocks bypass via later headers).
    if (header.length > MAX_COOKIE_HEADER_VALUE_LEN) return '';
    const value = getCookieValueFromHeaderString(header, name);
    // Preserve "first cookie wins" semantics, even when the value is an empty string.
    if (value !== undefined) return value;
  }

  const cookieHeader = (req.headers as Record<string, unknown>)['cookie'];
  if (typeof cookieHeader === 'string') {
    if (cookieHeader.length > MAX_COOKIE_HEADER_VALUE_LEN) return '';
    return getCookieValueFromHeaderString(cookieHeader, name);
  }
  if (Array.isArray(cookieHeader)) {
    for (const header of cookieHeader) {
      if (typeof header !== 'string') return '';
      if (header.length > MAX_COOKIE_HEADER_VALUE_LEN) return '';
    }
    for (const header of cookieHeader) {
      const value = getCookieValueFromHeaderString(header, name);
      if (value !== undefined) return value;
    }
    return undefined;
  }

  return undefined;
}
