import dns from "node:dns/promises";
import type { LookupAddress } from "node:dns";
import net from "node:net";
import ipaddr from "ipaddr.js";

export interface ResolvedTarget {
  requestedHost: string;
  requestedPort: number;
  resolvedAddress: string;
  family: 4 | 6;
  decision: "open" | "allowlist" | "default_public_only";
}

export interface TargetDecisionDenied {
  allowed: false;
  reason: string;
}

export interface TargetDecisionAllowed {
  allowed: true;
  target: ResolvedTarget;
}

export type TargetDecision = TargetDecisionDenied | TargetDecisionAllowed;

type PortMatcher = (port: number) => boolean;

type AllowRule =
  | { kind: "domain"; match: (hostname: string) => boolean; ports: PortMatcher; raw: string }
  | { kind: "cidr"; cidr: [ipaddr.IPv4 | ipaddr.IPv6, number]; ports: PortMatcher; raw: string }
  | { kind: "ip"; addr: ipaddr.IPv4 | ipaddr.IPv6; ports: PortMatcher; raw: string };

function normalizeHostname(hostname: string): string {
  return hostname.trim().toLowerCase().replace(/\.+$/, "");
}

function parsePortMatcher(raw: string): PortMatcher {
  const trimmed = raw.trim();
  if (trimmed === "" || trimmed === "*") return () => true;
  if (trimmed.includes("-")) {
    const [fromRaw, toRaw] = trimmed.split("-", 2);
    const from = Number(fromRaw);
    const to = Number(toRaw);
    if (!Number.isInteger(from) || !Number.isInteger(to) || from < 1 || to > 65535 || from > to) {
      throw new Error(`Invalid port range: ${raw}`);
    }
    return (port) => port >= from && port <= to;
  }
  const port = Number(trimmed);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`Invalid port: ${raw}`);
  }
  return (p) => p === port;
}

function parseAllowRule(entry: string): AllowRule {
  const raw = entry.trim();
  if (raw === "") throw new Error("Empty allowlist entry");

  const colonIdx = raw.lastIndexOf(":");
  const hostPart = colonIdx === -1 ? raw : raw.slice(0, colonIdx);
  const portPart = colonIdx === -1 ? "*" : raw.slice(colonIdx + 1);
  const ports = parsePortMatcher(portPart);

  const host = normalizeHostname(hostPart);

  if (host.includes("/")) {
    const cidr = ipaddr.parseCIDR(host);
    return { kind: "cidr", cidr, ports, raw };
  }

  if (net.isIP(host) !== 0) {
    const addr = ipaddr.parse(host);
    return { kind: "ip", addr, ports, raw };
  }

  if (host.startsWith("*.")) {
    const suffix = normalizeHostname(host.slice(2));
    return {
      kind: "domain",
      raw,
      ports,
      match: (hostname) => {
        const n = normalizeHostname(hostname);
        return n === suffix || n.endsWith(`.${suffix}`);
      }
    };
  }

  return {
    kind: "domain",
    raw,
    ports,
    match: (hostname) => normalizeHostname(hostname) === host
  };
}

function parseAllowlist(rawAllowlist: string): AllowRule[] {
  const trimmed = rawAllowlist.trim();
  if (trimmed === "") return [];

  return trimmed
    .split(",")
    .map((entry) => parseAllowRule(entry));
}

function isPublicUnicast(addr: ipaddr.IPv4 | ipaddr.IPv6): boolean {
  let normalized: ipaddr.IPv4 | ipaddr.IPv6 = addr;
  if (normalized.kind() === "ipv6" && (normalized as ipaddr.IPv6).isIPv4MappedAddress()) {
    normalized = (normalized as ipaddr.IPv6).toIPv4Address();
  }

  return normalized.range() === "unicast";
}

async function lookupAll(hostname: string, timeoutMs: number): Promise<LookupAddress[]> {
  const lookup = dns.lookup(hostname, { all: true, verbatim: true });
  const timeout = new Promise<never>((_resolve, reject) => {
    const handle = setTimeout(() => {
      reject(new Error(`DNS lookup timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    handle.unref();
  });

  return Promise.race([lookup, timeout]);
}

export interface ResolveAndAuthorizeOptions {
  open: boolean;
  allowlist: string;
  dnsTimeoutMs: number;
}

export async function resolveAndAuthorizeTarget(
  host: string,
  port: number,
  opts: ResolveAndAuthorizeOptions
): Promise<TargetDecision> {
  const requestedHost = normalizeHostname(host);

  if (opts.open) {
    const ipKind = net.isIP(requestedHost);
    if (ipKind !== 0) {
      return {
        allowed: true,
        target: {
          requestedHost,
          requestedPort: port,
          resolvedAddress: requestedHost,
          family: ipKind as 4 | 6,
          decision: "open"
        }
      };
    }

    const resolved = await lookupAll(requestedHost, opts.dnsTimeoutMs);
    if (resolved.length === 0) {
      return { allowed: false, reason: "DNS lookup returned no addresses" };
    }

    return {
      allowed: true,
      target: {
        requestedHost,
        requestedPort: port,
        resolvedAddress: resolved[0]!.address,
        family: resolved[0]!.family as 4 | 6,
        decision: "open"
      }
    };
  }

  const allowRules = parseAllowlist(opts.allowlist);

  const ipKind = net.isIP(requestedHost);
  const resolved = ipKind !== 0 ? [{ address: requestedHost, family: ipKind }] : await lookupAll(requestedHost, opts.dnsTimeoutMs);
  if (resolved.length === 0) {
    return { allowed: false, reason: "DNS lookup returned no addresses" };
  }

  const resolvedAddrs = resolved.map((r) => ({
    address: r.address,
    family: r.family as 4 | 6,
    parsed: ipaddr.parse(r.address)
  }));

  if (allowRules.length > 0) {
    for (const addr of resolvedAddrs) {
      const matched = allowRules.some((rule) => {
        if (!rule.ports(port)) return false;
        if (rule.kind === "domain") return rule.match(requestedHost);
        if (rule.kind === "ip") return rule.addr.toString() === addr.parsed.toString();
        return addr.parsed.match(rule.cidr);
      });
      if (matched) {
        return {
          allowed: true,
          target: {
            requestedHost,
            requestedPort: port,
            resolvedAddress: addr.address,
            family: addr.family,
            decision: "allowlist"
          }
        };
      }
    }

    return {
      allowed: false,
      reason: `Target not in allowlist (AERO_PROXY_ALLOW=${opts.allowlist})`
    };
  }

  // Safe-by-default: only allow public unicast targets unless explicitly opened.
  for (const addr of resolvedAddrs) {
    if (isPublicUnicast(addr.parsed)) {
      return {
        allowed: true,
        target: {
          requestedHost,
          requestedPort: port,
          resolvedAddress: addr.address,
          family: addr.family,
          decision: "default_public_only"
        }
      };
    }
  }

  return {
    allowed: false,
    reason: "Target resolves only to non-public IP ranges (private/loopback/link-local/etc)"
  };
}
