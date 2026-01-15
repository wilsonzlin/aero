export interface DnsHeader {
  id: number;
  flags: number;
  qdcount: number;
  ancount: number;
  nscount: number;
  arcount: number;
}

export interface DnsQuestion {
  name: string;
  type: number;
  class: number;
  /**
   * Raw wire bytes for the question section (QNAME + QTYPE + QCLASS).
   *
   * This is useful when you want to preserve the original query bytes in a
   * response (e.g. to avoid changing casing/encoding when echoing the question).
   */
  wire: Buffer;
}

export function decodeDnsHeader(message: Buffer): DnsHeader {
  if (message.length < 12) throw new Error("DNS message too short");
  return {
    id: message.readUInt16BE(0),
    flags: message.readUInt16BE(2),
    qdcount: message.readUInt16BE(4),
    ancount: message.readUInt16BE(6),
    nscount: message.readUInt16BE(8),
    arcount: message.readUInt16BE(10)
  };
}

export function normalizeDnsName(name: string): string {
  const trimmed = name.trim();
  let end = trimmed.length;
  while (end > 0 && trimmed.charCodeAt(end - 1) === 0x2e /* '.' */) {
    end -= 1;
  }
  const withoutTrailingDot = end === trimmed.length ? trimmed : trimmed.slice(0, end);
  // Avoid allocating in the common case where the name is already lowercase.
  return hasAsciiUppercase(withoutTrailingDot) ? withoutTrailingDot.toLowerCase() : withoutTrailingDot;
}

function hasAsciiUppercase(s: string): boolean {
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charCodeAt(i);
    if (c >= 0x41 /* 'A' */ && c <= 0x5a /* 'Z' */) return true;
  }
  return false;
}

export function readDnsName(message: Buffer, offset: number): { name: string; offsetAfter: number } {
  const labels: string[] = [];
  let jumped = false;
  let offsetAfter = offset;
  // Allocate lazily: most DNS queries (and many records) use uncompressed names.
  let seenPointers: Set<number> | null = null;
  // RFC1035: domain names are limited to 255 bytes in wire format (including length
  // octets and the terminating 0-length label). Enforce this defensively to avoid
  // unbounded allocations on malicious inputs.
  let nameBytes = 0;

  while (true) {
    if (offset >= message.length) throw new Error("DNS name out of bounds");
    const length = message[offset];

    // Compression pointer (RFC 1035 4.1.4)
    if ((length & 0xc0) === 0xc0) {
      if (offset + 1 >= message.length) throw new Error("DNS name pointer out of bounds");
      const pointer = ((length & 0x3f) << 8) | message[offset + 1]!;
      if (!seenPointers) seenPointers = new Set<number>();
      if (seenPointers.has(pointer)) throw new Error("DNS name pointer loop");
      // Protect against malicious pointer chains that bounce between pointers without
      // consuming any label bytes.
      if (seenPointers.size >= 64) throw new Error("DNS name pointer chain too long");
      seenPointers.add(pointer);

      if (!jumped) {
        offsetAfter = offset + 2;
        jumped = true;
      }
      offset = pointer;
      continue;
    }

    if (length === 0) {
      offset += 1;
      nameBytes += 1;
      if (nameBytes > 255) throw new Error("DNS name too long");
      if (!jumped) offsetAfter = offset;
      break;
    }

    if ((length & 0xc0) !== 0x00) throw new Error("DNS name label has invalid prefix bits");
    offset += 1;
    if (offset + length > message.length) throw new Error("DNS name label out of bounds");
    labels.push(message.toString("utf8", offset, offset + length));
    nameBytes += 1 + length;
    if (nameBytes > 255) throw new Error("DNS name too long");
    offset += length;
    if (!jumped) offsetAfter = offset;
  }

  return { name: labels.join("."), offsetAfter };
}

export interface DecodeFirstQuestionOptions {
  maxQnameLength: number;
}

export function decodeFirstQuestion(message: Buffer, opts: DecodeFirstQuestionOptions): DnsQuestion {
  const header = decodeDnsHeader(message);
  if (header.qdcount < 1) throw new Error("DNS query has no questions");
  if (header.qdcount !== 1) throw new Error("DNS query must have exactly one question");

  let offset = 12;
  const nameResult = readDnsName(message, offset);
  offset = nameResult.offsetAfter;

  if (offset + 4 > message.length) throw new Error("DNS question out of bounds");
  const type = message.readUInt16BE(offset);
  const cls = message.readUInt16BE(offset + 2);
  offset += 4;

  // The wire-format limit is 255 bytes (including length octets and terminating 0),
  // but many codebases (and this proxy) also want to enforce a smaller "hostname-like"
  // cap to avoid pathological inputs.
  if (Buffer.byteLength(nameResult.name, "utf8") > opts.maxQnameLength) {
    throw new Error("DNS qname too long");
  }

  return {
    name: nameResult.name,
    type,
    class: cls,
    wire: message.subarray(12, offset)
  };
}

export interface DnsAnswer {
  type: number;
  class: number;
  ttl: number;
  rdata: Buffer;
}

export interface EncodeDnsResponseOptions {
  id: number;
  queryFlags?: number;
  rcode: number;
  question?: Buffer;
  answers?: DnsAnswer[];
}

export function encodeDnsResponse(opts: EncodeDnsResponseOptions): Buffer {
  const question = opts.question ?? null;
  const answers = opts.answers ?? [];
  if (!question && answers.length > 0) {
    throw new Error("DNS response cannot include answers without a question");
  }

  const queryFlags = opts.queryFlags ?? 0;
  const flags =
    0x8000 | // QR
    (queryFlags & 0x7800) | // opcode
    (queryFlags & 0x0100) | // RD
    0x0080 | // RA
    (queryFlags & 0x0010) | // CD
    (opts.rcode & 0x000f);

  const qdcount = question ? 1 : 0;
  const ancount = answers.length;

  let totalBytes = 12;
  if (question) totalBytes += question.length;
  if (question) {
    for (const answer of answers) {
      totalBytes += 2 /* NAME ptr */ + 10 /* TYPE+CLASS+TTL+RDLEN */ + answer.rdata.length;
    }
  }

  const out = Buffer.allocUnsafe(totalBytes);
  out.writeUInt16BE(opts.id & 0xffff, 0);
  out.writeUInt16BE(flags & 0xffff, 2);
  out.writeUInt16BE(qdcount, 4);
  out.writeUInt16BE(ancount, 6);
  out.writeUInt16BE(0, 8); // NSCOUNT
  out.writeUInt16BE(0, 10); // ARCOUNT

  let offset = 12;
  if (question) {
    question.copy(out, offset);
    offset += question.length;
  }

  if (question) {
    for (const answer of answers) {
      // Name: compression pointer to the start of the question name at offset 12 (0xC00C).
      out.writeUInt16BE(0xc00c, offset);
      out.writeUInt16BE(answer.type & 0xffff, offset + 2);
      out.writeUInt16BE(answer.class & 0xffff, offset + 4);
      out.writeUInt32BE(answer.ttl >>> 0, offset + 6);
      out.writeUInt16BE(answer.rdata.length & 0xffff, offset + 10);
      offset += 12;
      answer.rdata.copy(out, offset);
      offset += answer.rdata.length;
    }
  }

  return out;
}

