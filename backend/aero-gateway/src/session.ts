import { createHmac, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import type { IncomingMessage } from 'node:http';

import type { Config } from './config.js';
import { decodeBase64UrlToBuffer, encodeBase64Url } from './base64url.js';
import { getCookieValueFromRequest } from './cookies.js';

export const SESSION_COOKIE_NAME = 'aero_session';

const HMAC_SHA256_SIG_LEN = 32;
// base64url-no-pad encoding length for a 32-byte HMAC:
// - 32 bytes => 44 chars with one '=' padding
// - without padding => 43 chars
const HMAC_SHA256_SIG_B64_LEN = 43;
const MAX_SESSION_TOKEN_PAYLOAD_B64_LEN = 16 * 1024;
const MAX_SESSION_TOKEN_LEN = MAX_SESSION_TOKEN_PAYLOAD_B64_LEN + 1 /* '.' */ + HMAC_SHA256_SIG_B64_LEN;

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
  verifySessionRequest: (req: IncomingMessage) => VerifiedSession | null;
}>;

const utf8DecoderFatal = new TextDecoder('utf-8', { fatal: true });

function sign(payloadBase64Url: string, secret: Buffer): Buffer {
  return createHmac('sha256', secret).update(payloadBase64Url).digest();
}

function constantTimeEqual(a: Buffer, b: Buffer): boolean {
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}

function decodeUtf8Exact(bytes: Buffer): string | null {
  try {
    return utf8DecoderFatal.decode(bytes);
  } catch {
    return null;
  }
}

export function mintSessionToken(payload: SessionTokenPayload, secret: Buffer): string {
  const payloadJson = JSON.stringify(payload);
  const payloadB64 = encodeBase64Url(Buffer.from(payloadJson, 'utf8'));
  if (payloadB64.length > MAX_SESSION_TOKEN_PAYLOAD_B64_LEN) {
    throw new Error('Session token payload too long');
  }
  const sig = sign(payloadB64, secret);
  const sigB64 = encodeBase64Url(sig);
  if (sig.length !== HMAC_SHA256_SIG_LEN || sigB64.length !== HMAC_SHA256_SIG_B64_LEN) {
    throw new Error('Unexpected session token signature format');
  }
  return `${payloadB64}.${sigB64}`;
}

export function verifySessionToken(token: string, secret: Buffer, nowMs = Date.now()): VerifiedSession | null {
  // Quick coarse cap to avoid scanning attacker-controlled strings for delimiters.
  if (token.length > MAX_SESSION_TOKEN_LEN) return null;

  const dot = token.indexOf('.');
  if (dot <= 0 || dot === token.length - 1) return null;
  if (token.indexOf('.', dot + 1) !== -1) return null;
  const payloadB64 = token.slice(0, dot);
  const sigB64 = token.slice(dot + 1);
  if (payloadB64.length > MAX_SESSION_TOKEN_PAYLOAD_B64_LEN) return null;
  if (sigB64.length !== HMAC_SHA256_SIG_B64_LEN) return null;

  let providedSig: Buffer;
  try {
    providedSig = decodeBase64UrlToBuffer(sigB64, { canonical: true });
  } catch {
    return null;
  }
  if (providedSig.length !== HMAC_SHA256_SIG_LEN) return null;

  const expectedSig = sign(payloadB64, secret);
  if (!constantTimeEqual(providedSig, expectedSig)) return null;

  let payloadRaw: Buffer;
  try {
    payloadRaw = decodeBase64UrlToBuffer(payloadB64, { canonical: true });
  } catch {
    return null;
  }

  let payload: SessionTokenPayload;
  try {
    const payloadJson = decodeUtf8Exact(payloadRaw);
    if (payloadJson === null) return null;
    payload = JSON.parse(payloadJson) as SessionTokenPayload;
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
    verifySessionRequest: (req) => {
      const value = getCookieValueFromRequest(req, SESSION_COOKIE_NAME);
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
