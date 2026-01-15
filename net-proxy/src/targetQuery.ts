function stripOptionalIpv6Brackets(host: string): string {
  const trimmed = host.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

export type ParsedTargetQuery = { host: string; port: number; portRaw: string } | { error: string };

export function parseTargetQuery(url: URL): ParsedTargetQuery {
  const hostRaw = url.searchParams.get("host");
  const portRaw = url.searchParams.get("port");
  if (hostRaw !== null && portRaw !== null) {
    const port = Number(portRaw);
    if (hostRaw.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      return { error: "Invalid host or port" };
    }
    return { host: stripOptionalIpv6Brackets(hostRaw), port, portRaw };
  }

  const target = url.searchParams.get("target");
  if (target === null || target.trim() === "") {
    return { error: "Missing host/port (or target)" };
  }

  const t = target.trim();
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

  const port = Number(portPart);
  if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
    return { error: "Invalid target host or port" };
  }

  return { host: stripOptionalIpv6Brackets(host), port, portRaw: portPart };
}

export function normalizeTargetHostForPolicy(rawHost: string): string {
  return stripOptionalIpv6Brackets(rawHost);
}

