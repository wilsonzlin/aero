import * as dgram from "node:dgram";

import { headerHasMimeType } from "../contentType.js";

export type DnsUpstream =
  | { kind: "udp"; host: string; port: number; label: string }
  | { kind: "doh"; url: string; label: string };

const MAX_UPSTREAM_ENTRY_LEN = 4096;

function parsePortNumber(rawPort: string, input: string): number {
  const portStr = rawPort.trim();
  if (portStr.length < 1 || portStr.length > 5) throw new Error(`Invalid upstream port: ${input}`);
  for (let i = 0; i < portStr.length; i += 1) {
    const c = portStr.charCodeAt(i);
    if (c < 0x30 || c > 0x39) throw new Error(`Invalid upstream port: ${input}`);
  }
  const port = Number(portStr);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error(`Invalid upstream port: ${input}`);
  return port;
}

function parseHostPort(input: string): { host: string; port: number } {
  const trimmed = input.trim();
  if (trimmed.length > MAX_UPSTREAM_ENTRY_LEN) throw new Error(`Invalid upstream: ${input}`);

  if (trimmed.startsWith("[")) {
    const closing = trimmed.indexOf("]");
    if (closing === -1) throw new Error(`Invalid upstream: ${input}`);
    if (closing === 1) throw new Error(`Invalid upstream: ${input}`);
    const host = trimmed.slice(1, closing).trim();
    if (!host) throw new Error(`Invalid upstream: ${input}`);

    const rest = trimmed.slice(closing + 1).trim();
    const port = rest === "" ? 53 : rest.startsWith(":") ? parsePortNumber(rest.slice(1), input) : NaN;
    if (!Number.isInteger(port)) throw new Error(`Invalid upstream: ${input}`);
    return { host, port };
  }

  const firstColon = trimmed.indexOf(":");
  if (firstColon === -1) return { host: trimmed, port: 53 };

  const lastColon = trimmed.lastIndexOf(":");
  if (firstColon !== lastColon) {
    // Unbracketed IPv6 is ambiguous; require brackets.
    throw new Error(`Invalid upstream address (use [ipv6]:port): ${input}`);
  }

  const host = trimmed.slice(0, lastColon).trim();
  const portPart = trimmed.slice(lastColon + 1);
  if (!host) throw new Error(`Invalid upstream: ${input}`);
  const port = parsePortNumber(portPart, input);
  return { host, port };

  // unreachable
}

export function parseUpstreams(rawUpstreams: readonly string[]): DnsUpstream[] {
  const upstreams: DnsUpstream[] = [];
  for (const raw of rawUpstreams) {
    const trimmed = raw.trim();
    if (!trimmed) continue;

    if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) {
      upstreams.push({ kind: "doh", url: trimmed, label: trimmed });
      continue;
    }

    const udp = trimmed.startsWith("udp://") ? trimmed.slice("udp://".length) : trimmed;
    const { host, port } = parseHostPort(udp);
    upstreams.push({ kind: "udp", host, port, label: `${host}:${port}` });
  }

  return upstreams;
}

export async function queryUdpUpstream(
  upstream: Extract<DnsUpstream, { kind: "udp" }>,
  query: Buffer,
  timeoutMs: number,
): Promise<Buffer> {
  const socket = dgram.createSocket(upstream.host.includes(":") ? "udp6" : "udp4");

  return await new Promise<Buffer>((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error(`UDP upstream timeout after ${timeoutMs}ms`));
    }, timeoutMs);
    timer.unref?.();

    socket.once("error", (err) => {
      clearTimeout(timer);
      socket.close();
      reject(err);
    });

    socket.once("message", (msg) => {
      clearTimeout(timer);
      socket.close();
      resolve(Buffer.from(msg));
    });

    socket.send(query, upstream.port, upstream.host, (err) => {
      if (!err) return;
      clearTimeout(timer);
      socket.close();
      reject(err);
    });
  });
}

export async function queryDohUpstream(
  upstream: Extract<DnsUpstream, { kind: 'doh' }>,
  query: Buffer,
  timeoutMs: number,
): Promise<Buffer> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  timer.unref?.();

  try {
    const response = await fetch(upstream.url, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/dns-message',
        Accept: 'application/dns-message',
      },
      // Keep this as `ArrayBuffer` to satisfy TypeScript's `fetch()` types under NodeNext.
      body: query.buffer.slice(query.byteOffset, query.byteOffset + query.byteLength) as ArrayBuffer,
      signal: controller.signal,
    });

    if (!response.ok) throw new Error(`DoH upstream HTTP ${response.status}`);
    const contentType = response.headers.get("content-type");
    if (!headerHasMimeType(contentType, "application/dns-message", 256)) {
      const shown = typeof contentType === "string" ? contentType.slice(0, 256) : "none";
      throw new Error(`DoH upstream returned unexpected Content-Type: ${shown}`);
    }

    const body = Buffer.from(await response.arrayBuffer());
    return body;
  } finally {
    clearTimeout(timer);
  }
}
