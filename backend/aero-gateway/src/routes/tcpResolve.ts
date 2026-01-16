import { lookup } from "node:dns/promises";

import {
  evaluateTcpHostPolicy,
  parseTcpHostnameEgressPolicyFromEnv,
  type TcpHostPolicyDecision,
} from "../security/egressPolicy.js";
import { isPublicIpAddress } from "../security/ipPolicy.js";
import { selectAllowedDnsAddress } from "./tcpDns.js";
import type { TcpProxyEgressMetricSink } from "./tcpEgressMetrics.js";

export const tcpProxyMetrics = {
  blockedByHostPolicy: 0,
  blockedByIpPolicy: 0,
};

export type TcpProxyTargetErrorKind = "host-policy" | "ip-policy" | "dns";
export type TcpProxyTargetErrorReason =
  | "invalid-host"
  | "ip-literal-disallowed"
  | "target-blocked"
  | "target-not-allowed"
  | "ip-egress-policy"
  | "dns-lookup-failed";

export class TcpProxyTargetError extends Error {
  readonly kind: TcpProxyTargetErrorKind;
  readonly reason: TcpProxyTargetErrorReason;
  readonly statusCode: number;

  constructor(kind: TcpProxyTargetErrorKind, reason: TcpProxyTargetErrorReason, statusCode: number) {
    super(formatTcpProxyTargetErrorMessage(reason));
    this.kind = kind;
    this.reason = reason;
    this.statusCode = statusCode;
  }
}

export async function resolveTcpProxyTarget(
  rawHost: string,
  port: number,
  opts: Readonly<{
    allowPrivateIps?: boolean;
    env?: Record<string, string | undefined>;
    metrics?: TcpProxyEgressMetricSink;
  }> = {},
): Promise<{ ip: string; port: number; hostname?: string }> {
  const env = opts.env ?? process.env;
  // By default we block private/reserved IPs to prevent SSRF / local-network
  // probing via the browser-facing TCP proxy.
  //
  // For local development + E2E testing we allow opting out so the proxy can
  // reach loopback test servers (e.g. Playwright).
  const allowPrivateIps = opts.allowPrivateIps ?? env.TCP_ALLOW_PRIVATE_IPS === "1";

  const hostPolicy = parseTcpHostnameEgressPolicyFromEnv(env);
  const decision = evaluateTcpHostPolicy(rawHost, hostPolicy);
  if (!decision.allowed) {
    tcpProxyMetrics.blockedByHostPolicy++;
    opts.metrics?.blockedByHostPolicyTotal?.inc();
    const statusCode = decision.reason === "invalid-hostname" ? 400 : 403;
    throw new TcpProxyTargetError("host-policy", hostPolicyRejectionReason(decision), statusCode);
  }

  if (decision.target.kind === "ip") {
    if (!allowPrivateIps && !isPublicIpAddress(decision.target.ip)) {
      tcpProxyMetrics.blockedByIpPolicy++;
      opts.metrics?.blockedByIpPolicyTotal?.inc();
      throw new TcpProxyTargetError("ip-policy", "ip-egress-policy", 403);
    }
    return { ip: decision.target.ip, port };
  }

  // Host policy is enforced before DNS. After that, still enforce IP egress
  // policy on the resolved targets, selecting the first allowed public IP.
  let addresses: { address: string; family: number }[];
  try {
    addresses = await lookup(decision.target.hostname, { all: true, verbatim: true });
  } catch {
    throw new TcpProxyTargetError("dns", "dns-lookup-failed", 502);
  }

  const chosen = selectAllowedDnsAddress(addresses, allowPrivateIps);
  if (chosen) {
    return { ip: chosen.address, port, hostname: decision.target.hostname };
  }

  tcpProxyMetrics.blockedByIpPolicy++;
  opts.metrics?.blockedByIpPolicyTotal?.inc();
  throw new TcpProxyTargetError("ip-policy", "ip-egress-policy", 403);
}

function formatTcpProxyTargetErrorMessage(reason: TcpProxyTargetErrorReason): string {
  switch (reason) {
    case "invalid-host":
      return "Invalid host";
    case "ip-literal-disallowed":
      return "IP-literal targets are not allowed";
    case "target-blocked":
      return "Target is blocked";
    case "target-not-allowed":
      return "Target is not allowed";
    case "ip-egress-policy":
      return "Target IP is not allowed by IP egress policy";
    case "dns-lookup-failed":
      return "DNS lookup failed";
    default:
      return "Target is not allowed";
  }
}

function hostPolicyRejectionReason(
  decision: Extract<TcpHostPolicyDecision, { allowed: false }>,
): TcpProxyTargetErrorReason {
  // Keep client-visible rejection strings stable and avoid leaking internal config knobs.
  switch (decision.reason) {
    case "invalid-hostname":
      return "invalid-host";
    case "ip-literal-disallowed":
      return "ip-literal-disallowed";
    case "blocked-by-host-policy":
      return "target-blocked";
    case "not-allowed-by-host-policy":
      return "target-not-allowed";
    default:
      return "target-not-allowed";
  }
}

