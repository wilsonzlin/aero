import type http from "node:http";

import { isOriginAllowed } from "../middleware/originGuard.js";

export type TcpProxyUpgradePolicy = Readonly<{
  /**
   * If the browser supplies an `Origin` header, it must match one of these
   * values or the upgrade is rejected.
   *
   * Note: some non-browser WebSocket clients omit `Origin`. For compatibility,
   * we only enforce the allowlist when `Origin` is present.
   */
  allowedOrigins?: readonly string[];
  /**
   * If provided, reject WebSocket upgrades originating from these client IPs.
   *
   * Exact match only (as reported by `req.socket.remoteAddress`).
   */
  blockedClientIps?: readonly string[];
  /**
   * If provided, outbound TCP dials must match this exact host allowlist.
   * Case-insensitive.
   */
  allowedTargetHosts?: readonly string[];
  /**
   * If provided, outbound TCP dials must match this port allowlist.
   */
  allowedTargetPorts?: readonly number[];
}>;

export type PolicyDecision = { ok: true } | { ok: false; status: number; message: string };

export function validateWsUpgradePolicy(
  req: http.IncomingMessage,
  policy: TcpProxyUpgradePolicy,
): PolicyDecision {
  if (policy.blockedClientIps && policy.blockedClientIps.length > 0) {
    const clientIp = req.socket.remoteAddress;
    if (clientIp && policy.blockedClientIps.includes(clientIp)) {
      return { ok: false, status: 403, message: "Client IP blocked" };
    }
  }

  const originHeader = req.headers.origin;
  const origin = Array.isArray(originHeader) ? originHeader[0] : originHeader;
  if (origin) {
    const allowedOrigins = policy.allowedOrigins;
    if (!allowedOrigins || allowedOrigins.length === 0) {
      return { ok: false, status: 403, message: "Origin not allowed" };
    }
    if (!isOriginAllowed(origin, allowedOrigins)) {
      return { ok: false, status: 403, message: "Origin not allowed" };
    }
  }

  return { ok: true };
}

export function validateTcpTargetPolicy(
  host: string,
  port: number,
  policy: TcpProxyUpgradePolicy,
): PolicyDecision {
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return { ok: false, status: 400, message: "Invalid target port" };
  }

  if (policy.allowedTargetPorts && policy.allowedTargetPorts.length > 0) {
    if (!policy.allowedTargetPorts.includes(port)) {
      return { ok: false, status: 403, message: "Target port not allowed" };
    }
  }

  if (policy.allowedTargetHosts && policy.allowedTargetHosts.length > 0) {
    const normalizedHost = normalizeHost(host);
    const allowed = policy.allowedTargetHosts.some(
      (allowedHost) => normalizeHost(allowedHost).toLowerCase() === normalizedHost.toLowerCase(),
    );
    if (!allowed) {
      return { ok: false, status: 403, message: "Target host not allowed" };
    }
  }

  return { ok: true };
}

function normalizeHost(host: string): string {
  if (host.startsWith("[") && host.endsWith("]") && host.length >= 2) {
    return host.slice(1, -1);
  }
  return host;
}

