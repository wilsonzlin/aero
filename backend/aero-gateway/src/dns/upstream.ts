import * as dgram from "node:dgram";

export type DnsUpstream =
  | { kind: "udp"; host: string; port: number; label: string }
  | { kind: "doh"; url: string; label: string };

function parseHostPort(input: string): { host: string; port: number } {
  const trimmed = input.trim();
  if (trimmed.startsWith("[")) {
    const closing = trimmed.indexOf("]");
    if (closing === -1) throw new Error(`Invalid upstream: ${input}`);
    const host = trimmed.slice(1, closing);
    const portPart = trimmed.slice(closing + 1);
    const port = portPart.startsWith(":") ? Number.parseInt(portPart.slice(1), 10) : 53;
    if (!Number.isFinite(port) || port <= 0 || port > 65535) throw new Error(`Invalid upstream port: ${input}`);
    return { host, port };
  }

  const parts = trimmed.split(":");
  if (parts.length === 1) return { host: trimmed, port: 53 };
  if (parts.length === 2) {
    const port = Number.parseInt(parts[1], 10);
    if (!Number.isFinite(port) || port <= 0 || port > 65535) throw new Error(`Invalid upstream port: ${input}`);
    return { host: parts[0], port };
  }

  // Unbracketed IPv6 is ambiguous; require brackets.
  throw new Error(`Invalid upstream address (use [ipv6]:port): ${input}`);
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
    const contentType = response.headers.get('content-type')?.split(';')[0]?.trim().toLowerCase();
    if (contentType !== 'application/dns-message') {
      throw new Error(`DoH upstream returned unexpected Content-Type: ${contentType ?? 'none'}`);
    }

    const body = Buffer.from(await response.arrayBuffer());
    return body;
  } finally {
    clearTimeout(timer);
  }
}
