import type { Duplex } from "node:stream";

import { rejectHttpUpgrade } from "./httpUpgradeReject.js";

// Conservative cap to avoid spending unbounded CPU/memory on attacker-controlled request targets.
// Many HTTP stacks enforce ~8KB request target limits; keep the gateway strict and predictable.
export const MAX_REQUEST_URL_LEN = 8 * 1024;

export function respondUpgradeHttp(socket: Duplex, status: number, message: string): void {
  rejectHttpUpgrade(socket, status, message);
}

export function enforceUpgradeRequestUrlLimit(rawUrl: string, socket: Duplex, parsedUrl?: URL): boolean {
  // Prefer the raw request target length (it includes any invalid bytes/fragments that might be
  // dropped during parsing), but fall back to the parsed URL if callers pass `upgradeUrl` without
  // preserving `req.url` (synthetic request objects).
  let len = rawUrl.length;
  if (len === 0 && parsedUrl) {
    len = parsedUrl.pathname.length + parsedUrl.search.length + parsedUrl.hash.length;
  }
  if (len > MAX_REQUEST_URL_LEN) {
    respondUpgradeHttp(socket, 414, "Request URL too long");
    return false;
  }
  return true;
}

export function parseUpgradeRequestUrl(
  rawUrl: string,
  socket: Duplex,
  opts: Readonly<{ invalidUrlMessage: string }>,
): URL | null {
  try {
    return new URL(rawUrl, "http://localhost");
  } catch {
    respondUpgradeHttp(socket, 400, opts.invalidUrlMessage);
    return null;
  }
}

export function resolveUpgradeRequestUrl(
  rawUrl: string,
  socket: Duplex,
  providedUrl: URL | undefined,
  invalidUrlMessage: string,
): URL | null {
  if (providedUrl) return providedUrl;
  return parseUpgradeRequestUrl(rawUrl, socket, { invalidUrlMessage });
}

