import { createHmac } from 'node:crypto';
import type { Config, UdpRelayAuthMode } from './config.js';
import { encodeBase64Url } from './base64url.js';

const HMAC_SHA256_SIG_LEN = 32;
// base64url-no-pad encoding length for a 32-byte HMAC:
// - 32 bytes => 44 chars with one '=' padding
// - without padding => 43 chars
const HMAC_SHA256_SIG_B64_LEN = 43;
const MAX_JWT_HEADER_B64_LEN = 4 * 1024;
const MAX_JWT_PAYLOAD_B64_LEN = 16 * 1024;
const MAX_JWT_LEN = MAX_JWT_HEADER_B64_LEN + 1 /* '.' */ + MAX_JWT_PAYLOAD_B64_LEN + 1 /* '.' */ + HMAC_SHA256_SIG_B64_LEN;

export const UDP_RELAY_ENDPOINT_PATHS = {
  webrtcSignal: '/webrtc/signal',
  webrtcOffer: '/webrtc/offer',
  udp: '/udp',
  webrtcIce: '/webrtc/ice',
} as const;

export type UdpRelayEndpoints = typeof UDP_RELAY_ENDPOINT_PATHS;

export type UdpRelaySessionInfo = Readonly<{
  baseUrl: string;
  authMode: UdpRelayAuthMode;
  endpoints: UdpRelayEndpoints;
  token?: string;
  expiresAt?: string;
}>;

export type UdpRelayTokenInfo = Readonly<{
  authMode: UdpRelayAuthMode;
  token?: string;
  expiresAt?: string;
}>;

function encodeJwtHS256(payload: Record<string, unknown>, secret: string): string {
  const header = { alg: 'HS256', typ: 'JWT' };
  const headerPart = encodeBase64Url(Buffer.from(JSON.stringify(header), 'utf8'));
  if (headerPart.length > MAX_JWT_HEADER_B64_LEN) {
    throw new Error('UDP relay JWT header too long');
  }
  const payloadPart = encodeBase64Url(Buffer.from(JSON.stringify(payload), 'utf8'));
  if (payloadPart.length > MAX_JWT_PAYLOAD_B64_LEN) {
    throw new Error('UDP relay JWT payload too long');
  }
  const signingInput = `${headerPart}.${payloadPart}`;
  const signature = createHmac('sha256', secret).update(signingInput).digest();
  const signaturePart = encodeBase64Url(signature);
  if (signature.length !== HMAC_SHA256_SIG_LEN || signaturePart.length !== HMAC_SHA256_SIG_B64_LEN) {
    throw new Error('Unexpected UDP relay JWT signature format');
  }
  const token = `${signingInput}.${signaturePart}`;
  if (token.length > MAX_JWT_LEN) {
    throw new Error('UDP relay JWT too long');
  }
  return token;
}

export function mintUdpRelayToken(
  config: Config,
  opts: { sessionId: string; origin?: string; nowMs?: number },
): UdpRelayTokenInfo | undefined {
  if (!config.UDP_RELAY_BASE_URL) return undefined;

  if (config.UDP_RELAY_AUTH_MODE === 'none') return { authMode: 'none' };

  const nowMs = opts.nowMs ?? Date.now();
  const ttlSeconds = config.UDP_RELAY_TOKEN_TTL_SECONDS;
  const expiresAt = new Date(nowMs + ttlSeconds * 1000).toISOString();

  if (config.UDP_RELAY_AUTH_MODE === 'api_key') {
    return { authMode: 'api_key', token: config.UDP_RELAY_API_KEY, expiresAt };
  }

  const iat = Math.floor(nowMs / 1000);
  const exp = iat + ttlSeconds;
  const payload: Record<string, unknown> = {
    iat,
    exp,
    sid: opts.sessionId,
  };

  if (opts.origin) payload.origin = opts.origin;
  if (config.UDP_RELAY_AUDIENCE) payload.aud = config.UDP_RELAY_AUDIENCE;
  if (config.UDP_RELAY_ISSUER) payload.iss = config.UDP_RELAY_ISSUER;

  return {
    authMode: 'jwt',
    token: encodeJwtHS256(payload, config.UDP_RELAY_JWT_SECRET),
    expiresAt,
  };
}

export function buildUdpRelaySessionInfo(
  config: Config,
  opts: { sessionId: string; origin?: string; nowMs?: number },
): UdpRelaySessionInfo | undefined {
  if (!config.UDP_RELAY_BASE_URL) return undefined;

  const tokenInfo = mintUdpRelayToken(config, opts);
  if (!tokenInfo) return undefined;

  return {
    baseUrl: config.UDP_RELAY_BASE_URL,
    authMode: config.UDP_RELAY_AUTH_MODE,
    endpoints: UDP_RELAY_ENDPOINT_PATHS,
    ...(tokenInfo.token ? { token: tokenInfo.token } : {}),
    ...(tokenInfo.expiresAt ? { expiresAt: tokenInfo.expiresAt } : {}),
  };
}

