import { createHmac } from 'node:crypto';
import type { Config, UdpRelayAuthMode } from './config.js';

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

function encodeBase64Url(buf: Buffer): string {
  return buf.toString('base64').replaceAll('=', '').replaceAll('+', '-').replaceAll('/', '_');
}

function encodeJwtHS256(payload: Record<string, unknown>, secret: string): string {
  const header = { alg: 'HS256', typ: 'JWT' };
  const headerPart = encodeBase64Url(Buffer.from(JSON.stringify(header), 'utf8'));
  const payloadPart = encodeBase64Url(Buffer.from(JSON.stringify(payload), 'utf8'));
  const signingInput = `${headerPart}.${payloadPart}`;
  const signaturePart = encodeBase64Url(createHmac('sha256', secret).update(signingInput).digest());
  return `${signingInput}.${signaturePart}`;
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

