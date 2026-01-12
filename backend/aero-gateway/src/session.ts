import { createHmac, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';

import type { Config } from './config.js';
import { getCookieValue } from './cookies.js';

export const SESSION_COOKIE_NAME = 'aero_session';

type LoggerLike = {
  warn: (obj: unknown, msg?: string) => void;
};

type SessionTokenPayload = {
  v: 1;
  sid: string;
  exp: number;
};

export type VerifiedSession = {
  id: string;
  expiresAtMs: number;
};

export type SessionManager = Readonly<{
  ttlSeconds: number;
  cookieSameSite: Config['SESSION_COOKIE_SAMESITE'];
  issueSession: (existing: VerifiedSession | null) => { token: string; session: VerifiedSession };
  verifySessionToken: (token: string) => VerifiedSession | null;
  verifySessionCookie: (cookieHeader: string | string[] | undefined) => VerifiedSession | null;
}>;

function encodeBase64Url(buf: Buffer): string {
  // Node supports the "base64url" encoding, which omits padding and uses the URL-safe alphabet.
  return buf.toString('base64url');
}

function decodeBase64Url(raw: string): Buffer {
  if (!isBase64Url(raw)) throw new Error('Invalid base64url');
  // Base64url inputs are unpadded; only lengths mod 4 of 0, 2, or 3 are valid.
  // (mod 4 of 1 cannot be produced by base64 encoding.)
  const mod = raw.length % 4;
  if (mod === 1) throw new Error('Invalid base64url length');
  return Buffer.from(raw, 'base64url');
}

function isBase64Url(raw: string): boolean {
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

function sign(payloadBase64Url: string, secret: Buffer): Buffer {
  return createHmac('sha256', secret).update(payloadBase64Url).digest();
}

function constantTimeEqual(a: Buffer, b: Buffer): boolean {
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}

export function mintSessionToken(payload: SessionTokenPayload, secret: Buffer): string {
  const payloadJson = JSON.stringify(payload);
  const payloadB64 = encodeBase64Url(Buffer.from(payloadJson, 'utf8'));
  const sig = sign(payloadB64, secret);
  const sigB64 = encodeBase64Url(sig);
  return `${payloadB64}.${sigB64}`;
}

export function verifySessionToken(token: string, secret: Buffer, nowMs = Date.now()): VerifiedSession | null {
  const dot = token.indexOf('.');
  if (dot <= 0 || dot === token.length - 1) return null;
  if (token.indexOf('.', dot + 1) !== -1) return null;
  const payloadB64 = token.slice(0, dot);
  const sigB64 = token.slice(dot + 1);

  let providedSig: Buffer;
  try {
    providedSig = decodeBase64Url(sigB64);
  } catch {
    return null;
  }

  const expectedSig = sign(payloadB64, secret);
  if (!constantTimeEqual(providedSig, expectedSig)) return null;

  let payloadRaw: Buffer;
  try {
    payloadRaw = decodeBase64Url(payloadB64);
  } catch {
    return null;
  }

  let payload: SessionTokenPayload;
  try {
    payload = JSON.parse(payloadRaw.toString('utf8')) as SessionTokenPayload;
  } catch {
    return null;
  }

  if (!payload || payload.v !== 1) return null;
  if (typeof payload.sid !== 'string' || payload.sid.length === 0) return null;
  if (typeof payload.exp !== 'number' || !Number.isFinite(payload.exp)) return null;

  const expiresAtMs = payload.exp * 1000;
  if (expiresAtMs <= nowMs) return null;

  return { id: payload.sid, expiresAtMs };
}

export function createSessionManager(config: Config, logger: LoggerLike): SessionManager {
  const secretRaw = config.SESSION_SECRET.trim();
  const secret =
    secretRaw.length > 0
      ? Buffer.from(secretRaw, 'utf8')
      : (() => {
          const generated = randomBytes(32);
          logger.warn(
            {
              env: ['AERO_GATEWAY_SESSION_SECRET', 'SESSION_SECRET'],
            },
            'Session secret not configured; generated a temporary secret (sessions will not survive restarts)',
          );
          return generated;
        })();

  const ttlSeconds = config.SESSION_TTL_SECONDS;

  return {
    ttlSeconds,
    cookieSameSite: config.SESSION_COOKIE_SAMESITE,
    issueSession: (existing) => {
      const nowMs = Date.now();
      const expiresAtMs = nowMs + ttlSeconds * 1000;
      const id = existing?.id ?? randomUUID();

      const payload: SessionTokenPayload = {
        v: 1,
        sid: id,
        exp: Math.floor(expiresAtMs / 1000),
      };
      const token = mintSessionToken(payload, secret);
      return { token, session: { id, expiresAtMs } };
    },
    verifySessionToken: (token) => verifySessionToken(token, secret),
    verifySessionCookie: (cookieHeader) => {
      const value = getCookieValue(cookieHeader, SESSION_COOKIE_NAME);
      if (!value) return null;
      return verifySessionToken(value, secret);
    },
  };
}

export class SessionConnectionTracker {
  private readonly maxConnections: number;
  private readonly active = new Map<string, number>();

  constructor(maxConnections: number) {
    if (!Number.isInteger(maxConnections) || maxConnections < 1) {
      throw new Error(`Invalid maxConnections: ${maxConnections}`);
    }
    this.maxConnections = maxConnections;
  }

  getMaxConnections(): number {
    return this.maxConnections;
  }

  getActiveConnections(sessionId: string): number {
    return this.active.get(sessionId) ?? 0;
  }

  tryAcquire(sessionId: string, count = 1): boolean {
    if (!Number.isInteger(count) || count < 1) return false;
    const current = this.getActiveConnections(sessionId);
    if (current + count > this.maxConnections) return false;
    this.active.set(sessionId, current + count);
    return true;
  }

  release(sessionId: string, count = 1): void {
    if (!Number.isInteger(count) || count < 1) return;
    const current = this.getActiveConnections(sessionId);
    const next = Math.max(0, current - count);
    if (next === 0) this.active.delete(sessionId);
    else this.active.set(sessionId, next);
  }
}
