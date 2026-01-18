import type http from "node:http";

import { isOriginAllowed } from "../middleware/originGuard.js";
import { tryGetProp, tryGetStringProp } from "../../../../src/safe_props.js";
import {
  classifyTargetHost,
  parseHostnamePattern,
  targetMatchesPattern,
  type HostnamePattern,
  type TargetHost,
} from "../security/egressPolicy.js";

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
   * If provided, outbound TCP dials must match this host allowlist.
   *
   * Supports exact matches and wildcard subdomain matches (`*.example.com`).
   */
  allowedTargetHosts?: readonly string[];
  /**
   * If provided, outbound TCP dials must match this port allowlist.
   */
  allowedTargetPorts?: readonly number[];
}>;

export type PolicyDecision = { ok: true } | { ok: false; status: number; message: string };

type CompiledPolicy = Readonly<{
  allowedTargetPorts?: ReadonlySet<number>;
  allowedTargetHostPatterns?: readonly HostnamePattern[];
}>;

const compiledPolicyCache = new WeakMap<TcpProxyUpgradePolicy, CompiledPolicy>();

function compilePolicy(policy: TcpProxyUpgradePolicy): CompiledPolicy {
  const cached = compiledPolicyCache.get(policy);
  if (cached) return cached;

  const compiled: CompiledPolicy = {
    allowedTargetPorts:
      policy.allowedTargetPorts && policy.allowedTargetPorts.length > 0 ? new Set(policy.allowedTargetPorts) : undefined,
    allowedTargetHostPatterns:
      policy.allowedTargetHosts && policy.allowedTargetHosts.length > 0
        ? policy.allowedTargetHosts.flatMap((allowedHost) => {
            try {
              return [parseHostnamePattern(allowedHost)];
            } catch {
              return [];
            }
          })
        : undefined,
  };

  compiledPolicyCache.set(policy, compiled);
  return compiled;
}

function originFromHeaders(headers: unknown): { ok: true; origin?: string } | { ok: false } {
  const originHeader = (headers as { origin?: unknown } | undefined)?.origin;
  if (originHeader === undefined) return { ok: true, origin: undefined };
  if (typeof originHeader === "string") return { ok: true, origin: originHeader };

  if (Array.isArray(originHeader)) {
    if (originHeader.length === 0) return { ok: true, origin: undefined };
    if (originHeader.length === 1) {
      const v = originHeader[0];
      return typeof v === "string" ? { ok: true, origin: v } : { ok: false };
    }
    return { ok: false };
  }

  return { ok: false };
}

export function validateWsUpgradePolicy(
  req: http.IncomingMessage,
  policy: TcpProxyUpgradePolicy,
): PolicyDecision {
  if (policy.blockedClientIps && policy.blockedClientIps.length > 0) {
    // Be defensive: some unit/property tests use minimal `IncomingMessage` shapes.
    const clientIp = tryGetStringProp(tryGetProp(req, "socket"), "remoteAddress");
    if (clientIp && policy.blockedClientIps.includes(clientIp)) {
      return { ok: false, status: 403, message: "Client IP blocked" };
    }
  }

  const originResult = originFromHeaders((req as unknown as { headers?: unknown }).headers);
  if (!originResult.ok) {
    return { ok: false, status: 403, message: "Origin not allowed" };
  }
  const origin = originResult.origin?.trim();
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

  const compiled = compilePolicy(policy);

  if (compiled.allowedTargetPorts) {
    if (!compiled.allowedTargetPorts.has(port)) {
      return { ok: false, status: 403, message: "Target port not allowed" };
    }
  }

  const rawAllowedHosts = policy.allowedTargetHosts;
  if (rawAllowedHosts && rawAllowedHosts.length > 0) {
    let target: TargetHost;
    try {
      target = classifyTargetHost(host);
    } catch {
      return { ok: false, status: 400, message: "Invalid target host" };
    }

    const patterns = compiled.allowedTargetHostPatterns ?? [];
    const allowed = patterns.some((pattern) => targetMatchesPattern(target, pattern));
    if (!allowed) {
      return { ok: false, status: 403, message: "Target host not allowed" };
    }
  }

  return { ok: true };
}
