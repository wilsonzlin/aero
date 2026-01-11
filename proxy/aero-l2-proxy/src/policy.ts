import type http from "node:http";

export const L2_TUNNEL_PATH = "/l2";
export const L2_TUNNEL_SUBPROTOCOL = "aero-l2-tunnel-v1";
export const L2_TOKEN_SUBPROTOCOL_PREFIX = "aero-l2-token.";

function tokenFromQuery(url: URL): string | null {
  // Avoid URLSearchParams here: it applies `application/x-www-form-urlencoded`
  // decoding rules, including treating `+` as a space. Our token values may use
  // `+` and should be interpreted literally unless percent-encoded.
  //
  // Keep this logic aligned with the Rust production proxy:
  // crates/aero-l2-proxy/src/server.rs::token_from_query.
  const query = url.search.startsWith("?") ? url.search.slice(1) : url.search;
  if (!query) return null;

  for (const part of query.split("&")) {
    const [k, v = ""] = part.split("=", 2);
    if (k !== "token") continue;
    if (!v) return null;
    return percentDecode(v);
  }

  return null;
}

function percentDecode(input: string): string {
  const out: number[] = [];
  for (let i = 0; i < input.length; i += 1) {
    if (input.charCodeAt(i) === 0x25 && i + 2 < input.length) {
      const hi = fromHex(input.charCodeAt(i + 1));
      const lo = fromHex(input.charCodeAt(i + 2));
      if (hi !== null && lo !== null) {
        out.push((hi << 4) | lo);
        i += 2;
        continue;
      }
    }
    out.push(input.charCodeAt(i) & 0xff);
  }
  return new TextDecoder().decode(new Uint8Array(out));
}

function fromHex(b: number): number | null {
  // 0-9
  if (b >= 0x30 && b <= 0x39) return b - 0x30;
  // a-f
  if (b >= 0x61 && b <= 0x66) return b - 0x61 + 10;
  // A-F
  if (b >= 0x41 && b <= 0x46) return b - 0x41 + 10;
  return null;
}

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

  const protocols = parseSecWebSocketProtocolHeader(req.headers["sec-websocket-protocol"]);
  if (!protocols.includes(L2_TUNNEL_SUBPROTOCOL)) {
    return { ok: false, status: 400, message: `Missing required subprotocol: ${L2_TUNNEL_SUBPROTOCOL}` };
  }

  if (policy.token !== null) {
    const queryToken = tokenFromQuery(url);
    const ok =
      queryToken === policy.token || protocols.includes(`${L2_TOKEN_SUBPROTOCOL_PREFIX}${policy.token}`);
    if (!ok) return { ok: false, status: 401, message: "Unauthorized" };
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

  return { ok: true };
}

export function chooseL2Subprotocol(offered: string[]): string | null {
  return offered.includes(L2_TUNNEL_SUBPROTOCOL) ? L2_TUNNEL_SUBPROTOCOL : null;
}
