import dns from "node:dns/promises";
import type { LookupAddress } from "node:dns";
import net from "node:net";
import ipaddr from "ipaddr.js";
import { splitCommaSeparatedList } from "./csv";
import { formatOneLineUtf8 } from "./text";

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

function formatForError(value: string, maxLen = 256): string {
  const out = formatOneLineUtf8(value, maxLen);
  if (out.length === value.length) return out;
  return `${out}…(${value.length} chars)`;
}

function normalizeHostname(hostname: string): string {
  return hostname.trim().toLowerCase().replace(/\.+$/, "");
}

function splitHostAndPortSpec(raw: string): { host: string; portSpec: string } {
  const trimmed = raw.trim();

  // Bracketed host form supports IPv6-with-port unambiguously, e.g.:
  //   [2606:4700:4700::1111]:443
  // Also accepted for consistency in other cases.
  if (trimmed.startsWith("[")) {
    const closeIdx = trimmed.indexOf("]");
    if (closeIdx === -1) throw new Error(`Invalid allowlist entry (missing ]): ${formatForError(raw)}`);
    const host = trimmed.slice(1, closeIdx);
    const rest = trimmed.slice(closeIdx + 1);
    if (rest === "") return { host, portSpec: "*" };
    if (!rest.startsWith(":")) {
      throw new Error(`Invalid allowlist entry (expected :port after ]): ${formatForError(raw)}`);
    }
    return { host, portSpec: rest.slice(1) || "*" };
  }

  // A bare IPv6 address contains ":" but has no unambiguous way to include a port without brackets.
  // Treat it as "any port" rather than mis-parsing the final hextet as a port.
  if (net.isIP(trimmed) === 6) {
    return { host: trimmed, portSpec: "*" };
  }

  const colonIdx = trimmed.lastIndexOf(":");
  if (colonIdx === -1) {
    return { host: trimmed, portSpec: "*" };
  }

  const hostPart = trimmed.slice(0, colonIdx);
  const maybePort = trimmed.slice(colonIdx + 1);
  if (maybePort === "") {
    throw new Error(`Invalid allowlist entry (empty port): ${formatForError(raw)}`);
  }

  // For IPv6 CIDRs like `2001:db8::/32`, `maybePort` will be `/32` which is not a port matcher.
  // In that case, interpret the entire string as the host/cidr and default port to "*".
  try {
    parsePortMatcher(maybePort);
    return { host: hostPart, portSpec: maybePort };
  } catch {
    return { host: trimmed, portSpec: "*" };
  }
}

function parsePortMatcher(raw: string): PortMatcher {
  const trimmed = raw.trim();
  if (trimmed === "" || trimmed === "*") return () => true;
  if (trimmed.includes("-")) {
    const [fromRaw, toRaw] = trimmed.split("-", 2);
    const from = Number(fromRaw);
    const to = Number(toRaw);
    if (!Number.isInteger(from) || !Number.isInteger(to) || from < 1 || to > 65535 || from > to) {
      throw new Error(`Invalid port range: ${formatForError(raw)}`);
    }
    return (port) => port >= from && port <= to;
  }
  const port = Number(trimmed);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`Invalid port: ${formatForError(raw)}`);
  }
  return (p) => p === port;
}

function parseAllowRule(entry: string): AllowRule {
  const raw = entry.trim();
  if (raw === "") throw new Error("Empty allowlist entry");

  const { host: hostPartRaw, portSpec } = splitHostAndPortSpec(raw);
  const ports = parsePortMatcher(portSpec);

  const host = normalizeHostname(hostPartRaw);

  if (host === "*") {
    return { kind: "domain", raw, ports, match: () => true };
  }

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

const allowlistCache = new Map<string, AllowRule[]>();

function parseAllowlist(rawAllowlist: string): AllowRule[] {
  const trimmed = rawAllowlist.trim();
  if (trimmed === "") return [];

  const cached = allowlistCache.get(trimmed);
  if (cached) return cached;

  let entries: string[];
  try {
    entries = splitCommaSeparatedList(trimmed, { maxLen: 64 * 1024, maxItems: 4096 });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Invalid allowlist: ${formatForError(msg, 128)}`);
  }

  const parsed = entries.map((entry) => parseAllowRule(entry));
  allowlistCache.set(trimmed, parsed);
  // Avoid unbounded memory growth if callers provide lots of distinct allowlists.
  if (allowlistCache.size > 100) {
    allowlistCache.clear();
    allowlistCache.set(trimmed, parsed);
  }
  return parsed;
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
  let handle: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_resolve, reject) => {
    handle = setTimeout(() => {
      reject(new Error(`DNS lookup timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    handle.unref();
  });

  try {
    return await Promise.race([lookup, timeout]);
  } finally {
    if (handle) clearTimeout(handle);
  }
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
        // Domain allowlist entries are intentionally limited to *public* targets to prevent
        // DNS rebinding from turning a "public hostname" allowlist into private network access.
        // To allow private/loopback ranges, use an explicit IP/CIDR allowlist entry.
        if (rule.kind === "domain") return rule.match(requestedHost) && isPublicUnicast(addr.parsed);
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

    const domainRuleMatchedHostAndPort = allowRules.some(
      (rule) => rule.kind === "domain" && rule.ports(port) && rule.match(requestedHost)
    );
    if (domainRuleMatchedHostAndPort && resolvedAddrs.every((addr) => !isPublicUnicast(addr.parsed))) {
      return {
        allowed: false,
        reason: "Target resolves to non-public IPs; domain allowlist rules only allow public targets (use IP/CIDR allowlist)"
      };
    }

    return {
      allowed: false,
      reason: "Target not in allowlist"
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
