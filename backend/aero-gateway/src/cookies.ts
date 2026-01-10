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

  const proto = raw.split(',')[0]?.trim().toLowerCase();
  return proto === 'https';
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

