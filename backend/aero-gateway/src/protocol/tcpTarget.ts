export type TcpProxyProtocolVersion = 1;

export interface TcpTarget {
  host: string;
  port: number;
  version: TcpProxyProtocolVersion;
}

export type TcpTargetParseErrorCode =
  | "ERR_TCP_UNSUPPORTED_VERSION"
  | "ERR_TCP_MISSING_HOST"
  | "ERR_TCP_MISSING_PORT"
  | "ERR_TCP_INVALID_HOST"
  | "ERR_TCP_INVALID_PORT"
  | "ERR_TCP_INVALID_TARGET";

export class TcpTargetParseError extends Error {
  override name = "TcpTargetParseError";
  readonly code: TcpTargetParseErrorCode;

  constructor(code: TcpTargetParseErrorCode, message: string) {
    super(message);
    this.code = code;
  }
}

// Defensive caps: query params are attacker-controlled.
const MAX_VERSION_LEN = 16;
const MAX_HOST_LEN = 1024;
const MAX_PORT_LEN = 16;
const MAX_TARGET_LEN = MAX_HOST_LEN + MAX_PORT_LEN + 16;

export function parseTcpTargetFromUrl(url: URL): TcpTarget {
  return parseTcpTarget(url.searchParams);
}

/**
 * Parses `/tcp` query parameters.
 *
 * Supported forms:
 *  - `target=<host>:<port>`
 *  - `host=<host>&port=<port>`
 *
 * If both forms are provided, `target` takes precedence.
 *
 * IPv6 must be supplied in RFC3986 bracket form in `target`, e.g.:
 *  - `target=[2001:db8::1]:443`
 *
 * Bracketed IPv6 is accepted for `host` as well (e.g. `host=[::1]`).
 *
 * Versioning:
 *  - `v=1` (default if omitted).
 */
export function parseTcpTarget(searchParams: URLSearchParams): TcpTarget {
  const version = parseVersion(searchParams.get("v"));

  const target = searchParams.get("target");
  if (target !== null) {
    if (target.length > MAX_TARGET_LEN) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_TARGET",
        "Invalid target: too long",
      );
    }
    const { host, port } = parseTargetParam(target);
    return { host, port, version };
  }

  const rawHost = searchParams.get("host");
  const rawPort = searchParams.get("port");
  if (rawHost === null) {
    throw new TcpTargetParseError(
      "ERR_TCP_MISSING_HOST",
      "Missing required query parameter: host",
    );
  }
  if (rawPort === null) {
    throw new TcpTargetParseError(
      "ERR_TCP_MISSING_PORT",
      "Missing required query parameter: port",
    );
  }

  if (rawHost.length > MAX_HOST_LEN) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_HOST",
      "Invalid host: too long",
    );
  }
  if (rawPort.length > MAX_PORT_LEN) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_PORT",
      "Invalid port",
    );
  }

  const host = normalizeHost(rawHost);
  const port = parsePort(rawPort);
  return { host, port, version };
}

function parseVersion(raw: string | null): TcpProxyProtocolVersion {
  if (raw === null || raw === "") {
    return 1;
  }
  if (raw.length > MAX_VERSION_LEN) {
    throw new TcpTargetParseError(
      "ERR_TCP_UNSUPPORTED_VERSION",
      "Unsupported TCP proxy protocol version",
    );
  }
  if (raw === "1") {
    return 1;
  }
  throw new TcpTargetParseError(
    "ERR_TCP_UNSUPPORTED_VERSION",
    "Unsupported TCP proxy protocol version",
  );
}

function parseTargetParam(target: string): { host: string; port: number } {
  if (target === "") {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_TARGET",
      "Query parameter target must not be empty",
    );
  }

  // RFC3986 bracket form for IPv6 literals.
  if (target.startsWith("[")) {
    const close = target.indexOf("]");
    if (close === -1) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_TARGET",
        "Invalid target: missing closing bracket for IPv6 literal",
      );
    }
    const host = target.slice(1, close);
    if (host === "") {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_TARGET",
        "Invalid target: empty IPv6 literal",
      );
    }
    const rest = target.slice(close + 1);
    if (!rest.startsWith(":")) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_TARGET",
        "Invalid target: missing port separator",
      );
    }
    const port = parsePort(rest.slice(1));
    return { host, port };
  }

  const colon = target.lastIndexOf(":");
  if (colon === -1) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_TARGET",
      "Invalid target: missing port separator",
    );
  }

  const rawHost = target.slice(0, colon);
  const rawPort = target.slice(colon + 1);
  if (rawHost === "" || rawPort === "") {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_TARGET",
      "Invalid target: expected <host>:<port>",
    );
  }
  // IPv6 literals must use RFC3986 bracket form (`[::1]:443`), otherwise they
  // are ambiguous (e.g. `2001:db8::1` could be interpreted as host+port).
  if (rawHost.includes(":")) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_TARGET",
      "Invalid target: IPv6 literals must use bracket form, e.g. [::1]:443",
    );
  }
  if (rawHost.includes("[") || rawHost.includes("]")) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_TARGET",
      "Invalid target: unexpected bracket in host",
    );
  }

  const host = normalizeHost(rawHost);
  const port = parsePort(rawPort);
  return { host, port };
}

function normalizeHost(host: string): string {
  if (host === "") {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_HOST",
      "Host must not be empty",
    );
  }
  if (host.startsWith("[") || host.endsWith("]")) {
    if (!(host.startsWith("[") && host.endsWith("]"))) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_HOST",
        "Invalid host: mismatched brackets",
      );
    }
    const inner = host.slice(1, -1);
    if (inner === "") {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_HOST",
        "Invalid host: empty bracketed host",
      );
    }
    if (inner.length > MAX_HOST_LEN) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_HOST",
        "Invalid host: too long",
      );
    }
    return inner;
  }
  if (host.length > MAX_HOST_LEN) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_HOST",
      "Invalid host: too long",
    );
  }
  return host;
}

function parsePort(port: string): number {
  if (port === "") {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_PORT",
      "Invalid port",
    );
  }
  if (port.length > MAX_PORT_LEN) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_PORT",
      "Invalid port",
    );
  }

  let n = 0;
  for (let i = 0; i < port.length; i += 1) {
    const c = port.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_PORT",
        "Invalid port",
      );
    }
    n = n * 10 + (c - 0x30);
    if (n > 65535) {
      throw new TcpTargetParseError(
        "ERR_TCP_INVALID_PORT",
        "Invalid port",
      );
    }
  }

  if (n < 1) {
    throw new TcpTargetParseError(
      "ERR_TCP_INVALID_PORT",
      "Invalid port",
    );
  }

  return n;
}
