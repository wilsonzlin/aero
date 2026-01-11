import type http from "node:http";

export const L2_TUNNEL_PATH = "/l2";
export const L2_TUNNEL_SUBPROTOCOL = "aero-l2-tunnel-v1";
export const L2_TOKEN_SUBPROTOCOL_PREFIX = "aero-l2-token.";

function normalizeOriginHeader(origin: string): string | null {
  const trimmed = origin.trim();
  if (trimmed === "null") return "null";

  try {
    return new URL(trimmed).origin;
  } catch {
    return null;
  }
}

export function isOriginAllowed(originHeader: string, allowedOrigins: readonly string[]): boolean {
  if (allowedOrigins.includes("*")) return true;

  const normalized = normalizeOriginHeader(originHeader);
  if (!normalized) return false;

  return allowedOrigins.includes(normalized);
}

function parseSecWebSocketProtocolHeader(header: unknown): string[] {
  const raw = Array.isArray(header) ? header.join(",") : typeof header === "string" ? header : "";
  if (!raw) return [];
  return raw
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
}

export type L2UpgradePolicy = Readonly<{
  open: boolean;
  allowedOrigins: readonly string[];
  token: string | null;
  maxConnections: number;
}>;

export type PolicyDecision = { ok: true } | { ok: false; status: number; message: string };

export function validateL2WsUpgrade(
  req: http.IncomingMessage,
  url: URL,
  policy: L2UpgradePolicy,
  activeConnections: number,
): PolicyDecision {
  if (policy.maxConnections > 0 && activeConnections >= policy.maxConnections) {
    return { ok: false, status: 429, message: "Too many connections" };
  }

  if (!policy.open) {
    const originHeader = req.headers.origin;
    const origin = Array.isArray(originHeader) ? originHeader[0] : originHeader;
    if (!origin) {
      return { ok: false, status: 403, message: "Missing required Origin header" };
    }

    if (policy.allowedOrigins.length === 0) {
      return { ok: false, status: 403, message: "Origin not allowed" };
    }

    if (!isOriginAllowed(origin, policy.allowedOrigins)) {
      return { ok: false, status: 403, message: "Origin not allowed" };
    }
  }

  const protocols = parseSecWebSocketProtocolHeader(req.headers["sec-websocket-protocol"]);
  if (!protocols.includes(L2_TUNNEL_SUBPROTOCOL)) {
    return { ok: false, status: 400, message: `Missing required subprotocol: ${L2_TUNNEL_SUBPROTOCOL}` };
  }

  if (policy.token !== null) {
    const tokenParam = url.searchParams.get("token");
    if (tokenParam !== null) {
      if (tokenParam !== policy.token) return { ok: false, status: 401, message: "Unauthorized" };
    } else {
      const ok =
        protocols.includes(policy.token) || protocols.includes(`${L2_TOKEN_SUBPROTOCOL_PREFIX}${policy.token}`);
      if (!ok) return { ok: false, status: 401, message: "Unauthorized" };
    }
  }

  return { ok: true };
}

export function chooseL2Subprotocol(offered: string[]): string | null {
  return offered.includes(L2_TUNNEL_SUBPROTOCOL) ? L2_TUNNEL_SUBPROTOCOL : null;
}
