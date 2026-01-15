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

export class TcpProxyTargetError extends Error {
  readonly kind: "host-policy" | "ip-policy" | "dns";
  readonly statusCode: number;

  constructor(kind: "host-policy" | "ip-policy" | "dns", message: string, statusCode: number) {
    super(message);
    this.kind = kind;
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
    throw new TcpProxyTargetError("host-policy", formatHostPolicyRejection(decision), statusCode);
  }

  if (decision.target.kind === "ip") {
    if (!allowPrivateIps && !isPublicIpAddress(decision.target.ip)) {
      tcpProxyMetrics.blockedByIpPolicy++;
      opts.metrics?.blockedByIpPolicyTotal?.inc();
      throw new TcpProxyTargetError("ip-policy", "Target IP is not allowed by IP egress policy", 403);
    }
    return { ip: decision.target.ip, port };
  }

  // Host policy is enforced before DNS. After that, still enforce IP egress
  // policy on the resolved targets, selecting the first allowed public IP.
  let addresses: { address: string; family: number }[];
  try {
    addresses = await lookup(decision.target.hostname, { all: true, verbatim: true });
  } catch {
    throw new TcpProxyTargetError("dns", `DNS lookup failed for ${decision.target.hostname}`, 502);
  }

  const chosen = selectAllowedDnsAddress(addresses, allowPrivateIps);
  if (chosen) {
    return { ip: chosen.address, port, hostname: decision.target.hostname };
  }

  tcpProxyMetrics.blockedByIpPolicy++;
  opts.metrics?.blockedByIpPolicyTotal?.inc();
  throw new TcpProxyTargetError("ip-policy", "All resolved IPs are blocked by IP egress policy", 403);
}

function formatHostPolicyRejection(decision: Extract<TcpHostPolicyDecision, { allowed: false }>): string {
  return `${decision.reason}: ${decision.message}`;
}

