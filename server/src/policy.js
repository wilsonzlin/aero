import dns from "node:dns/promises";
import ipaddr from "ipaddr.js";

export class PolicyError extends Error {
  constructor(message) {
    super(message);
    this.name = "PolicyError";
  }
}

export function isPortAllowed(port, allowPorts) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return false;
  if (!allowPorts || allowPorts.length === 0) return false;
  return allowPorts.some((r) => port >= r.start && port <= r.end);
}

export function isHostAllowed(host, allowHosts) {
  if (!host) return false;
  if (!allowHosts || allowHosts.length === 0) return false;
  const needle = host.toLowerCase();

  const isIp = ipaddr.isValid(needle);
  const ip = isIp ? ipaddr.parse(needle) : null;

  for (const pattern of allowHosts) {
    if (pattern.kind === "wildcard") return true;
    if (pattern.kind === "exact" && needle === pattern.value) return true;
    if (pattern.kind === "suffix" && needle.endsWith(pattern.suffix) && needle.length > pattern.suffix.length) return true;
    if (pattern.kind === "cidr" && ip && ip.match([pattern.addr, pattern.prefixLen])) return true;
  }
  return false;
}

function isAllowedNonPublicRange(range) {
  // ipaddr.js range values:
  // IPv4: unicast, private, loopback, linkLocal, carrierGradeNat, multicast, broadcast, reserved, unspecified, benchmarking
  // IPv6: unicast, uniqueLocal, loopback, linkLocal, multicast, unspecified, ipv4Mapped, rfc6145, rfc6052, 6to4, teredo, ...
  return ["private", "loopback", "linkLocal", "carrierGradeNat", "uniqueLocal"].includes(range);
}

export function isIpAllowed(ipString, allowPrivateRanges) {
  if (!ipaddr.isValid(ipString)) return false;
  const ip = ipaddr.parse(ipString);
  const range = ip.range();
  if (range === "unicast") return true;
  return allowPrivateRanges ? isAllowedNonPublicRange(range) : false;
}

export async function resolveAndValidateTarget({ host, port }, config) {
  if (!isPortAllowed(port, config.allowPorts)) {
    throw new PolicyError("Port is not allowlisted");
  }
  if (!isHostAllowed(host, config.allowHosts)) {
    throw new PolicyError("Host is not allowlisted");
  }

  const candidates = [];
  if (ipaddr.isValid(host)) {
    candidates.push({ address: host, family: ipaddr.parse(host).kind() === "ipv6" ? 6 : 4 });
  } else {
    const answers = await dns.lookup(host, { all: true });
    for (const a of answers) candidates.push(a);
  }

  for (const candidate of candidates) {
    if (isIpAllowed(candidate.address, config.allowPrivateRanges)) {
      return candidate;
    }
  }
  throw new PolicyError("Target resolved to blocked address range");
}

