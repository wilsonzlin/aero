import { ResponseTooLargeError, readJsonResponseWithLimit, readResponseBytesWithLimit } from "../storage/response_json";

export interface DohAResult {
  address: string;
  ttl: number;
}

const MAX_DOH_MESSAGE_BYTES = 1024 * 1024; // 1 MiB
const MAX_DOH_JSON_BYTES = 1024 * 1024; // 1 MiB

async function cancelBody(resp: Response): Promise<void> {
  try {
    await resp.body?.cancel();
  } catch {
    // ignore
  }
}

/**
 * Resolve an A record using DNS-over-HTTPS (DoH, RFC 8484).
 *
 * This matches the needs of the Rust net stack: the host turns `Action::DnsResolve { name }` into
 * a DoH query and returns the resolved address + TTL back into the stack.
 */
export async function resolveAOverDoh(
  name: string,
  endpoint = "/dns-query",
): Promise<DohAResult | null> {
  const trimmed = name.trim().replace(/\.$/, "");
  if (!trimmed) return null;

  const id = randomUint16();
  const query = encodeDnsQueryA(trimmed, id);

  const resp = await fetch(endpoint, {
    method: "POST",
    headers: {
      "Content-Type": "application/dns-message",
      Accept: "application/dns-message",
    },
    body: query,
  });
  if (!resp.ok) {
    await cancelBody(resp);
    return null;
  }

  const contentType = resp.headers.get("content-type")?.split(";")[0]?.trim().toLowerCase();
  if (contentType !== "application/dns-message") {
    await cancelBody(resp);
    return null;
  }

  try {
    const message = await readResponseBytesWithLimit(resp, { maxBytes: MAX_DOH_MESSAGE_BYTES, label: "DoH response" });
    return parseFirstAAnswer(message, id);
  } catch (err) {
    if (err instanceof ResponseTooLargeError) return null;
    throw err;
  }
}

// Legacy helper retained for compatibility with older integrations.
// Prefer `resolveAOverDoh` which targets the gateway's RFC8484 endpoint.
// This helper is Cloudflare-DNS-JSON compatible and defaults to the gateway's
// first-party `/dns-json` endpoint.
export async function resolveAOverDohJson(
  name: string,
  endpoint = "/dns-json",
): Promise<DohAResult | null> {
  const trimmed = name.trim().replace(/\.$/, "");
  if (!trimmed) return null;

  const base = typeof location !== "undefined" ? location.origin : "http://localhost";
  const url = new URL(endpoint, base);
  url.searchParams.set("name", trimmed);
  url.searchParams.set("type", "A");

  const resp = await fetch(url.toString(), {
    headers: {
      Accept: "application/dns-json",
    },
  });
  if (!resp.ok) {
    await cancelBody(resp);
    return null;
  }

  let json: unknown;
  try {
    json = await readJsonResponseWithLimit(resp, { maxBytes: MAX_DOH_JSON_BYTES, label: "DoH JSON response" });
  } catch (err) {
    if (err instanceof ResponseTooLargeError) return null;
    throw err;
  }
  if (!json || typeof json !== "object") return null;

  const answer = (json as any).Answer as Array<any> | undefined;
  if (!answer || !Array.isArray(answer)) return null;

  const firstA = answer.find((a) => a && a.type === 1 && typeof a.data === "string");
  if (!firstA) return null;

  return {
    address: firstA.data,
    ttl: typeof firstA.TTL === "number" ? firstA.TTL : 60,
  };
}

function randomUint16(): number {
  const bytes = new Uint16Array(1);
  crypto.getRandomValues(bytes);
  return bytes[0] ?? 0;
}

function encodeDnsQueryA(name: string, id: number): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const labels = name.split(".").filter(Boolean);

  const nameParts: Uint8Array[] = [];
  let nameLength = 1;
  for (const label of labels) {
    const bytes = encoder.encode(label);
    if (bytes.length === 0 || bytes.length > 63) {
      throw new Error("Invalid DNS label length");
    }
    nameParts.push(Uint8Array.of(bytes.length), bytes);
    nameLength += 1 + bytes.length;
  }
  nameParts.push(Uint8Array.of(0));

  const out = new Uint8Array<ArrayBuffer>(new ArrayBuffer(12 + nameLength + 4));
  const view = new DataView(out.buffer);
  view.setUint16(0, id, false);
  view.setUint16(2, 0x0100, false); // RD
  view.setUint16(4, 1, false); // QDCOUNT
  view.setUint16(6, 0, false); // ANCOUNT
  view.setUint16(8, 0, false); // NSCOUNT
  view.setUint16(10, 0, false); // ARCOUNT

  let offset = 12;
  for (const part of nameParts) {
    out.set(part, offset);
    offset += part.length;
  }
  view.setUint16(offset, 1, false); // QTYPE=A
  view.setUint16(offset + 2, 1, false); // QCLASS=IN
  return out;
}

function parseFirstAAnswer(message: Uint8Array, expectedId: number): DohAResult | null {
  if (message.length < 12) return null;
  const view = new DataView(message.buffer, message.byteOffset, message.byteLength);
  const id = view.getUint16(0, false);
  if (id !== expectedId) return null;

  const flags = view.getUint16(2, false);
  const rcode = flags & 0x0f;
  if (rcode !== 0) return null;

  const qdcount = view.getUint16(4, false);
  const ancount = view.getUint16(6, false);

  let offset = 12;
  for (let i = 0; i < qdcount; i++) {
    const qname = readDnsName(message, offset);
    offset = qname.nextOffset;
    if (offset + 4 > message.length) return null;
    offset += 4;
  }

  for (let i = 0; i < ancount; i++) {
    const nameResult = readDnsName(message, offset);
    offset = nameResult.nextOffset;
    if (offset + 10 > message.length) return null;
    const type = view.getUint16(offset, false);
    const cls = view.getUint16(offset + 2, false);
    const ttl = view.getUint32(offset + 4, false);
    const rdlength = view.getUint16(offset + 8, false);
    offset += 10;
    if (offset + rdlength > message.length) return null;

    if (type === 1 && cls === 1 && rdlength === 4) {
      const addr = message.subarray(offset, offset + 4);
      return {
        address: `${addr[0]}.${addr[1]}.${addr[2]}.${addr[3]}`,
        ttl,
      };
    }

    offset += rdlength;
  }

  return null;
}

function readDnsName(message: Uint8Array, offset: number): { name: string; nextOffset: number } {
  const decoder = new TextDecoder();
  const labels: string[] = [];
  let jumped = false;
  let nextOffset = offset;
  let guard = 0;

  while (true) {
    if (offset >= message.length) throw new Error("DNS name out of bounds");
    if (guard++ > message.length) throw new Error("DNS name pointer loop");
    const len = message[offset] ?? 0;

    // Compression pointer.
    if ((len & 0xc0) === 0xc0) {
      if (offset + 1 >= message.length) throw new Error("DNS name pointer out of bounds");
      const ptr = ((len & 0x3f) << 8) | (message[offset + 1] ?? 0);
      if (!jumped) {
        nextOffset = offset + 2;
        jumped = true;
      }
      offset = ptr;
      continue;
    }

    if (len === 0) {
      offset += 1;
      if (!jumped) nextOffset = offset;
      break;
    }

    if ((len & 0xc0) !== 0) throw new Error("DNS name label has invalid prefix bits");
    offset += 1;
    if (offset + len > message.length) throw new Error("DNS name label out of bounds");
    const label = decoder.decode(message.subarray(offset, offset + len));
    labels.push(label);
    offset += len;
    if (!jumped) nextOffset = offset;
  }

  return { name: labels.join("."), nextOffset };
}
