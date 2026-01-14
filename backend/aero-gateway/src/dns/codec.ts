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
}

export function decodeDnsHeader(message: Buffer): DnsHeader {
  if (message.length < 12) throw new Error("DNS message too short");
  return {
    id: message.readUInt16BE(0),
    flags: message.readUInt16BE(2),
    qdcount: message.readUInt16BE(4),
    ancount: message.readUInt16BE(6),
    nscount: message.readUInt16BE(8),
    arcount: message.readUInt16BE(10),
  };
}

export function getRcodeFromFlags(flags: number): number {
  return flags & 0x0f;
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
      const pointer = ((length & 0x3f) << 8) | message[offset + 1];
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

export function decodeFirstQuestion(message: Buffer): DnsQuestion {
  const header = decodeDnsHeader(message);
  if (header.qdcount < 1) throw new Error("DNS query has no questions");
  if (header.qdcount !== 1) throw new Error("DNS query must have exactly one question");

  let offset = 12;
  const nameResult = readDnsName(message, offset);
  offset = nameResult.offsetAfter;

  if (offset + 4 > message.length) throw new Error("DNS question out of bounds");
  const type = message.readUInt16BE(offset);
  const cls = message.readUInt16BE(offset + 2);
  return { name: normalizeDnsName(nameResult.name), type, class: cls };
}

export function skipQuestions(message: Buffer, qdcount: number, offset = 12): number {
  let current = offset;
  for (let i = 0; i < qdcount; i++) {
    const nameResult = readDnsName(message, current);
    current = nameResult.offsetAfter;
    if (current + 4 > message.length) throw new Error("DNS question out of bounds");
    current += 4;
  }
  return current;
}

export interface DnsRecordHeader {
  type: number;
  class: number;
  ttl: number;
  rdataOffset: number;
  rdataLength: number;
  offsetAfter: number;
}

export function readDnsRecordHeader(message: Buffer, offset: number): DnsRecordHeader {
  const nameResult = readDnsName(message, offset);
  let current = nameResult.offsetAfter;
  if (current + 10 > message.length) throw new Error("DNS resource record out of bounds");
  const type = message.readUInt16BE(current);
  const cls = message.readUInt16BE(current + 2);
  const ttl = message.readUInt32BE(current + 4);
  const rdlength = message.readUInt16BE(current + 8);
  const rdataOffset = current + 10;
  const offsetAfter = rdataOffset + rdlength;
  if (offsetAfter > message.length) throw new Error("DNS resource record rdata out of bounds");
  return { type, class: cls, ttl, rdataOffset, rdataLength: rdlength, offsetAfter };
}

export function parseSoaMinimumTtl(message: Buffer, record: DnsRecordHeader): number | null {
  // SOA: MNAME, RNAME, SERIAL, REFRESH, RETRY, EXPIRE, MINIMUM
  let offset = record.rdataOffset;
  const mname = readDnsName(message, offset);
  offset = mname.offsetAfter;
  const rname = readDnsName(message, offset);
  offset = rname.offsetAfter;
  if (offset + 20 > record.rdataOffset + record.rdataLength) return null;
  // serial, refresh, retry, expire are present but ignored for caching.
  offset += 16;
  const minimum = message.readUInt32BE(offset);
  return minimum;
}

export interface DnsCacheInfo {
  ttlSeconds: number;
  rcode: number;
  isNegative: boolean;
}

export function extractCacheInfoFromResponse(
  response: Buffer,
  negativeDefaultTtlSeconds: number,
): DnsCacheInfo | null {
  const header = decodeDnsHeader(response);
  const rcode = getRcodeFromFlags(header.flags);

  let offset = skipQuestions(response, header.qdcount);

  let minAnswerTtl = Number.POSITIVE_INFINITY;
  for (let i = 0; i < header.ancount; i++) {
    const record = readDnsRecordHeader(response, offset);
    minAnswerTtl = Math.min(minAnswerTtl, record.ttl);
    offset = record.offsetAfter;
  }

  let soaMinimum: number | null = null;
  let soaTtl: number | null = null;
  for (let i = 0; i < header.nscount; i++) {
    const record = readDnsRecordHeader(response, offset);
    if (record.type === 6) {
      const minimum = parseSoaMinimumTtl(response, record);
      if (minimum !== null) {
        soaMinimum = minimum;
        soaTtl = record.ttl;
      }
    }
    offset = record.offsetAfter;
  }

  // We don't need to parse additionals; skip.

  // Positive caching: NOERROR with at least one answer.
  if (rcode === 0 && header.ancount > 0) {
    const ttlSeconds = Number.isFinite(minAnswerTtl) ? Math.floor(minAnswerTtl) : 0;
    return { ttlSeconds: Math.max(0, ttlSeconds), rcode, isNegative: false };
  }

  // Negative caching: NXDOMAIN or NOERROR/NODATA.
  if (rcode === 3 || (rcode === 0 && header.ancount === 0)) {
    const soaBounded =
      soaMinimum !== null ? Math.min(soaMinimum, soaTtl ?? soaMinimum) : negativeDefaultTtlSeconds;
    return { ttlSeconds: Math.max(0, Math.floor(soaBounded)), rcode, isNegative: true };
  }

  // Do not cache other rcodes by default.
  return null;
}

export interface DnsQueryEncodeOptions {
  id: number;
  flags?: number;
  name: string;
  type: number;
  class?: number;
}

export function encodeDnsName(name: string): Buffer {
  const normalized = normalizeDnsName(name);
  if (normalized === "") return Buffer.from([0x00]);
  const labels = normalized.split(".").filter(Boolean);
  const labelLengths: number[] = [];
  let totalBytes = 1; // terminating 0-length label
  for (const label of labels) {
    const len = Buffer.byteLength(label, "utf8");
    if (len > 63) throw new Error("DNS label too long");
    totalBytes += 1 + len;
    if (totalBytes > 255) throw new Error("DNS name too long");
    labelLengths.push(len);
  }

  const out = Buffer.allocUnsafe(totalBytes);
  let offset = 0;
  for (let i = 0; i < labels.length; i += 1) {
    const len = labelLengths[i]!;
    out[offset] = len;
    offset += 1;
    const written = out.write(labels[i]!, offset, len, "utf8");
    if (written !== len) throw new Error("DNS name encoding error");
    offset += len;
  }
  out[offset] = 0x00;
  return out;
}

export function encodeDnsQuery(options: DnsQueryEncodeOptions): Buffer {
  const qname = encodeDnsName(options.name);
  const out = Buffer.allocUnsafe(12 + qname.length + 4);

  out.writeUInt16BE(options.id & 0xffff, 0);
  out.writeUInt16BE(options.flags ?? 0x0100, 2); // RD by default
  out.writeUInt16BE(1, 4);
  out.writeUInt16BE(0, 6);
  out.writeUInt16BE(0, 8);
  out.writeUInt16BE(0, 10);

  qname.copy(out, 12);
  const tailOffset = 12 + qname.length;
  out.writeUInt16BE(options.type & 0xffff, tailOffset);
  out.writeUInt16BE((options.class ?? 1) & 0xffff, tailOffset + 2);
  return out;
}

export interface DnsAAnswer {
  name: string;
  ttl: number;
  address: string;
}

export interface DnsErrorResponseEncodeOptions {
  id: number;
  queryFlags?: number;
  question?: DnsQuestion;
  rcode: number;
}

export interface DnsResponseEncodeOptions {
  id: number;
  flags?: number;
  question: DnsQuestion;
  answers: DnsAAnswer[];
}

function encodeIpv4(address: string): Buffer {
  const out = Buffer.allocUnsafe(4);
  let octet = 0;
  let value = 0;
  let sawDigit = false;

  for (let i = 0; i <= address.length; i += 1) {
    const c = i === address.length ? 0x2e /* '.' */ : address.charCodeAt(i);
    if (c === 0x2e /* '.' */) {
      if (!sawDigit) throw new Error("Invalid IPv4 address");
      if (octet >= 4) throw new Error("Invalid IPv4 address");
      out[octet] = value;
      octet += 1;
      value = 0;
      sawDigit = false;
      continue;
    }

    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) throw new Error("Invalid IPv4 address");
    sawDigit = true;
    value = value * 10 + (c - 0x30);
    if (value > 255) throw new Error("Invalid IPv4 address");
  }

  if (octet !== 4) throw new Error("Invalid IPv4 address");
  return out;
}

export function encodeDnsResponseA(options: DnsResponseEncodeOptions): Buffer {
  const answers = options.answers;

  const questionName = encodeDnsName(options.question.name);
  const answerParts: Array<{ name: Buffer; ipv4: Buffer; ttl: number }> = [];
  let totalBytes = 12 + questionName.length + 4;
  for (const answer of answers) {
    const name = encodeDnsName(answer.name);
    const ipv4 = encodeIpv4(answer.address);
    answerParts.push({ name, ipv4, ttl: answer.ttl });
    totalBytes += name.length + 10 + ipv4.length;
  }

  const out = Buffer.allocUnsafe(totalBytes);
  out.writeUInt16BE(options.id & 0xffff, 0);
  out.writeUInt16BE(options.flags ?? 0x8180, 2); // Standard response + RA
  out.writeUInt16BE(1, 4);
  out.writeUInt16BE(answers.length, 6);
  out.writeUInt16BE(0, 8);
  out.writeUInt16BE(0, 10);

  let offset = 12;
  questionName.copy(out, offset);
  offset += questionName.length;
  out.writeUInt16BE(options.question.type & 0xffff, offset);
  out.writeUInt16BE(options.question.class & 0xffff, offset + 2);
  offset += 4;

  for (const answer of answerParts) {
    answer.name.copy(out, offset);
    offset += answer.name.length;

    out.writeUInt16BE(1, offset); // A
    out.writeUInt16BE(1, offset + 2); // IN
    out.writeUInt32BE(answer.ttl >>> 0, offset + 4);
    out.writeUInt16BE(answer.ipv4.length, offset + 8);
    offset += 10;

    answer.ipv4.copy(out, offset);
    offset += answer.ipv4.length;
  }

  return out;
}

export function encodeDnsErrorResponse(options: DnsErrorResponseEncodeOptions): Buffer {
  const queryFlags = options.queryFlags ?? 0;
  const flags =
    0x8000 | // QR
    (queryFlags & 0x7800) | // opcode
    (queryFlags & 0x0100) | // RD
    0x0080 | // RA
    (queryFlags & 0x0010) | // CD
    (options.rcode & 0x000f);

  const hasQuestion = Boolean(options.question);
  if (!options.question) {
    const out = Buffer.alloc(12);
    out.writeUInt16BE(options.id & 0xffff, 0);
    out.writeUInt16BE(flags, 2);
    out.writeUInt16BE(0, 4);
    out.writeUInt16BE(0, 6);
    out.writeUInt16BE(0, 8);
    out.writeUInt16BE(0, 10);
    return out;
  }

  const qname = encodeDnsName(options.question.name);
  const out = Buffer.allocUnsafe(12 + qname.length + 4);
  out.writeUInt16BE(options.id & 0xffff, 0);
  out.writeUInt16BE(flags, 2);
  out.writeUInt16BE(hasQuestion ? 1 : 0, 4);
  out.writeUInt16BE(0, 6);
  out.writeUInt16BE(0, 8);
  out.writeUInt16BE(0, 10);
  qname.copy(out, 12);
  const tailOffset = 12 + qname.length;
  out.writeUInt16BE(options.question.type & 0xffff, tailOffset);
  out.writeUInt16BE(options.question.class & 0xffff, tailOffset + 2);
  return out;
}
