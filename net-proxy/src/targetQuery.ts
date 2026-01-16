function stripOptionalIpv6Brackets(host: string): string {
  const trimmed = host.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

export type ParsedTargetQuery = { host: string; port: number; portRaw: string } | { error: string };

// Defensive caps for attacker-controlled query parameters.
// - Hostnames are normally <= 253 chars, but allow some slack for odd environments.
// - Keep these comfortably below the global request-target cap (~8KB) so parsing stays cheap.
const MAX_TARGET_HOST_LEN = 1024;
const MAX_TARGET_PORT_STR_LEN = 16;
const MAX_TARGET_PARAM_LEN = 2 * 1024;

function hasForbiddenHostChars(host: string): boolean {
  for (let i = 0; i < host.length; i += 1) {
    const c = host.charCodeAt(i);
    // Reject ASCII control/whitespace and a few common “newline-like” Unicode separators.
    if (c <= 0x20 || c === 0x7f || c === 0x85 || c === 0x2028 || c === 0x2029) return true;
  }
  return false;
}

function parsePortStrict(raw: string): number | null {
  if (raw === "") return null;
  let n = 0;
  for (let i = 0; i < raw.length; i += 1) {
    const c = raw.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return null;
    n = n * 10 + (c - 0x30);
    if (n > 65535) return null;
  }
  if (n < 1) return null;
  return n;
}

export function parseTargetQuery(url: URL): ParsedTargetQuery {
  const hostRaw = url.searchParams.get("host");
  const portRaw = url.searchParams.get("port");
  if (hostRaw !== null && portRaw !== null) {
    if (hostRaw.length > MAX_TARGET_HOST_LEN) {
      return { error: "host too long" };
    }
    if (portRaw.length > MAX_TARGET_PORT_STR_LEN) {
      return { error: "port too long" };
    }
    const port = parsePortStrict(portRaw);
    if (hostRaw.trim() === "" || port === null) {
      return { error: "Invalid host or port" };
    }
    const host = stripOptionalIpv6Brackets(hostRaw);
    if (host.length > MAX_TARGET_HOST_LEN) {
      return { error: "host too long" };
    }
    if (hasForbiddenHostChars(host)) {
      return { error: "Invalid host" };
    }
    return { host, port, portRaw: String(port) };
  }

  const target = url.searchParams.get("target");
  if (target === null || target.trim() === "") {
    return { error: "Missing host/port (or target)" };
  }

  const t = target.trim();
  if (t.length > MAX_TARGET_PARAM_LEN) {
    return { error: "target too long" };
  }
  let host = "";
  let portPart = "";
  if (t.startsWith("[")) {
    const closeIdx = t.indexOf("]");
    if (closeIdx === -1) return { error: "Invalid target (missing closing ] for IPv6)" };
    host = t.slice(1, closeIdx);
    const rest = t.slice(closeIdx + 1);
    if (!rest.startsWith(":")) return { error: "Invalid target (missing :port)" };
    portPart = rest.slice(1);
  } else {
    const colonIdx = t.lastIndexOf(":");
    if (colonIdx === -1) return { error: "Invalid target (missing :port)" };
    host = t.slice(0, colonIdx);
    portPart = t.slice(colonIdx + 1);
  }

  if (host.length > MAX_TARGET_HOST_LEN) {
    return { error: "host too long" };
  }
  if (portPart.length > MAX_TARGET_PORT_STR_LEN) {
    return { error: "port too long" };
  }
  const port = parsePortStrict(portPart);
  if (host.trim() === "" || port === null) {
    return { error: "Invalid target host or port" };
  }

  const normalizedHost = stripOptionalIpv6Brackets(host);
  if (normalizedHost.length > MAX_TARGET_HOST_LEN) {
    return { error: "host too long" };
  }
  if (hasForbiddenHostChars(normalizedHost)) {
    return { error: "Invalid host" };
  }
  return { host: normalizedHost, port, portRaw: String(port) };
}

export function normalizeTargetHostForPolicy(rawHost: string): string {
  return stripOptionalIpv6Brackets(rawHost);
}

