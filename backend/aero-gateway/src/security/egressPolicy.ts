import { isIP } from "node:net";
import { domainToASCII } from "node:url";

export type HostnamePattern =
  | { kind: "exact"; hostname: string }
  | { kind: "wildcard"; suffix: string }
  | { kind: "ip"; ip: string; version: 4 | 6 };

export type TargetHost =
  | { kind: "hostname"; hostname: string }
  | { kind: "ip"; ip: string; version: 4 | 6 };

export interface TcpHostnameEgressPolicy {
  /**
   * If non-empty, the target hostname must match at least one pattern.
   */
  allowed: HostnamePattern[];
  /**
   * Always applied; deny overrides allow.
   */
  blocked: HostnamePattern[];
  /**
   * If true, disallow IP-literal targets entirely.
   */
  requireDnsName: boolean;
}

export type TcpHostPolicyDecision =
  | { allowed: true; target: TargetHost }
  | {
      allowed: false;
      reason:
        | "invalid-hostname"
        | "ip-literal-disallowed"
        | "blocked-by-host-policy"
        | "not-allowed-by-host-policy";
      message: string;
    };

export function parseTcpHostnameEgressPolicyFromEnv(env: Record<string, string | undefined>): TcpHostnameEgressPolicy {
  return {
    allowed: parseHostnamePatterns(env.TCP_ALLOWED_HOSTS),
    blocked: parseHostnamePatterns(env.TCP_BLOCKED_HOSTS),
    requireDnsName: env.TCP_REQUIRE_DNS_NAME === "1",
  };
}

export function parseHostnamePatterns(csv: string | undefined): HostnamePattern[] {
  if (!csv) return [];
  const parts = csv
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  return parts.map(parseHostnamePattern);
}

export function parseHostnamePattern(rawPattern: string): HostnamePattern {
  const pattern = rawPattern.trim();
  if (!pattern) throw new Error("Empty host pattern");

  if (pattern.startsWith("*.")) {
    const suffixRaw = pattern.slice(2);
    if (!suffixRaw) throw new Error(`Invalid wildcard host pattern "${rawPattern}"`);
    if (suffixRaw.includes("*")) {
      throw new Error(`Invalid wildcard host pattern "${rawPattern}": "*" is only supported as a "*." prefix`);
    }
    return {
      kind: "wildcard",
      suffix: normalizeHostname(suffixRaw),
    };
  }

  if (pattern.includes("*")) {
    throw new Error(`Invalid host pattern "${rawPattern}": "*" is only supported as a "*." prefix`);
  }

  const classified = classifyTargetHost(pattern);
  if (classified.kind === "ip") {
    return { kind: "ip", ip: classified.ip, version: classified.version };
  }
  return { kind: "exact", hostname: classified.hostname };
}

export function hostnameMatchesPattern(hostname: string, pattern: HostnamePattern): boolean {
  if (pattern.kind === "ip") return false;
  if (pattern.kind === "exact") return hostname === pattern.hostname;
  // "*.example.com" matches "a.example.com" and "a.b.example.com" but not
  // "example.com" itself.
  const suffix = pattern.suffix;
  if (hostname.length <= suffix.length) return false;
  if (!hostname.endsWith(suffix)) return false;
  // Avoid allocating `.${suffix}` for every match check.
  return hostname.charCodeAt(hostname.length - suffix.length - 1) === 0x2e /* '.' */;
}

export function targetMatchesPattern(target: TargetHost, pattern: HostnamePattern): boolean {
  if (target.kind === "ip") {
    return pattern.kind === "ip" && pattern.ip === target.ip;
  }
  return hostnameMatchesPattern(target.hostname, pattern);
}

export function normalizeHostname(rawHost: string): string {
  const trimmed = rawHost.trim();
  let end = trimmed.length;
  while (end > 0 && trimmed.charCodeAt(end - 1) === 0x2e /* '.' */) {
    end -= 1;
  }
  const withoutTrailingDot = end === trimmed.length ? trimmed : trimmed.slice(0, end);
  if (!withoutTrailingDot) throw new Error("Invalid hostname");

  const ascii = domainToASCII(withoutTrailingDot);
  if (!ascii) throw new Error("Invalid hostname");

  // Avoid allocating when the IDNA-normalized hostname is already lowercase.
  const normalized = /[A-Z]/.test(ascii) ? ascii.toLowerCase() : ascii;
  assertValidAsciiHostname(normalized);
  return normalized;
}

function assertValidAsciiHostname(hostname: string): void {
  // Based on the usual hostname constraints for DNS names:
  // - max 253 chars (excluding a trailing dot)
  // - labels separated by ".", each 1..63 chars
  // - labels: a-z0-9-, no leading/trailing "-"
  if (hostname.length < 1 || hostname.length > 253) throw new Error("Invalid hostname");

  // Avoid allocating intermediate arrays/regex state: validate in a single scan.
  let labelLen = 0;
  let prev = 0;

  for (let i = 0; i < hostname.length; i += 1) {
    const c = hostname.charCodeAt(i);
    if (c === 0x2e /* '.' */) {
      if (labelLen < 1 || labelLen > 63) throw new Error("Invalid hostname");
      if (prev === 0x2d /* '-' */) throw new Error("Invalid hostname");
      labelLen = 0;
      continue;
    }

    const isLower = c >= 0x61 /* 'a' */ && c <= 0x7a /* 'z' */;
    const isDigit = c >= 0x30 /* '0' */ && c <= 0x39 /* '9' */;
    const isHyphen = c === 0x2d /* '-' */;
    if (!isLower && !isDigit && !isHyphen) throw new Error("Invalid hostname");
    if (labelLen === 0 && isHyphen) throw new Error("Invalid hostname");

    labelLen += 1;
    if (labelLen > 63) throw new Error("Invalid hostname");
    prev = c;
  }

  if (labelLen < 1 || labelLen > 63) throw new Error("Invalid hostname");
  if (prev === 0x2d /* '-' */) throw new Error("Invalid hostname");
}

export function classifyTargetHost(rawHost: string): TargetHost {
  const host = rawHost.trim();
  if (!host) throw new Error("Invalid hostname");

  const maybeBracketedV6 =
    host.startsWith("[") && host.endsWith("]") && host.length > 2 ? host.slice(1, -1) : host;

  const version = isIP(maybeBracketedV6);
  if (version === 4 || version === 6) {
    return {
      kind: "ip",
      ip: /[A-F]/.test(maybeBracketedV6) ? maybeBracketedV6.toLowerCase() : maybeBracketedV6,
      version,
    };
  }

  return {
    kind: "hostname",
    hostname: normalizeHostname(host),
  };
}

export function evaluateTcpHostPolicy(rawHost: string, policy: TcpHostnameEgressPolicy): TcpHostPolicyDecision {
  let target: TargetHost;
  try {
    target = classifyTargetHost(rawHost);
  } catch {
    return {
      allowed: false,
      reason: "invalid-hostname",
      message: "Target hostname is invalid",
    };
  }

  if (target.kind === "ip") {
    if (policy.requireDnsName) {
      return {
        allowed: false,
        reason: "ip-literal-disallowed",
        message: "IP-literal targets are disallowed by TCP_REQUIRE_DNS_NAME",
      };
    }

    if (policy.blocked.some((pattern) => targetMatchesPattern(target, pattern))) {
      return {
        allowed: false,
        reason: "blocked-by-host-policy",
        message: "Target is blocked by TCP_BLOCKED_HOSTS",
      };
    }

    if (policy.allowed.length > 0 && !policy.allowed.some((pattern) => targetMatchesPattern(target, pattern))) {
      return {
        allowed: false,
        reason: "not-allowed-by-host-policy",
        message: "Target does not match TCP_ALLOWED_HOSTS",
      };
    }

    return { allowed: true, target };
  }

  if (policy.blocked.some((pattern) => targetMatchesPattern(target, pattern))) {
    return {
      allowed: false,
      reason: "blocked-by-host-policy",
      message: "Target is blocked by TCP_BLOCKED_HOSTS",
    };
  }

  if (policy.allowed.length > 0 && !policy.allowed.some((pattern) => targetMatchesPattern(target, pattern))) {
    return {
      allowed: false,
      reason: "not-allowed-by-host-policy",
      message: "Target does not match TCP_ALLOWED_HOSTS",
    };
  }

  return { allowed: true, target };
}
