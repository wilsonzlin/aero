import { decodeDnsHeader, getRcodeFromFlags, normalizeDnsName, readDnsName, skipQuestions } from './codec.js';
import { DNS_RECORD_TYPES } from './recordTypes.js';

export interface DnsJsonQuestion {
  name: string;
  type: number;
}

export interface DnsJsonAnswer {
  name: string;
  type: number;
  TTL: number;
  data: string;
}

export interface DnsJsonResponse {
  Status: number;
  TC: boolean;
  RD: boolean;
  RA: boolean;
  AD: boolean;
  CD: boolean;
  Question: DnsJsonQuestion[];
  Answer?: DnsJsonAnswer[];
}

function isFlagSet(flags: number, mask: number): boolean {
  return (flags & mask) !== 0;
}

function formatIpv4(bytes: Uint8Array): string {
  if (bytes.length !== 4) throw new Error('Invalid IPv4 length');
  return `${bytes[0]}.${bytes[1]}.${bytes[2]}.${bytes[3]}`;
}

function formatIpv6(bytes: Uint8Array): string {
  if (bytes.length !== 16) throw new Error('Invalid IPv6 length');

  const hextets: number[] = [];
  for (let i = 0; i < 16; i += 2) {
    hextets.push((bytes[i] << 8) | bytes[i + 1]);
  }

  let bestStart = -1;
  let bestLen = 0;
  let currentStart = -1;
  let currentLen = 0;

  for (let i = 0; i < hextets.length; i += 1) {
    if (hextets[i] === 0) {
      if (currentStart === -1) {
        currentStart = i;
        currentLen = 1;
      } else {
        currentLen += 1;
      }
    } else if (currentStart !== -1) {
      if (currentLen > bestLen) {
        bestStart = currentStart;
        bestLen = currentLen;
      }
      currentStart = -1;
      currentLen = 0;
    }
  }

  if (currentStart !== -1 && currentLen > bestLen) {
    bestStart = currentStart;
    bestLen = currentLen;
  }

  if (bestLen < 2) {
    bestStart = -1;
    bestLen = 0;
  }

  let out = '';
  for (let i = 0; i < hextets.length; i += 1) {
    if (bestStart !== -1 && i === bestStart) {
      out += '::';
      i += bestLen - 1;
      continue;
    }

    if (out.length > 0 && !out.endsWith(':')) {
      out += ':';
    }
    out += hextets[i].toString(16);
  }

  return out.length === 0 ? '::' : out;
}

export function dnsResponseToJson(response: Buffer, question: DnsJsonQuestion): DnsJsonResponse {
  const header = decodeDnsHeader(response);
  const flags = header.flags;

  let offset = skipQuestions(response, header.qdcount);
  const answers: DnsJsonAnswer[] = [];

  for (let idx = 0; idx < header.ancount; idx += 1) {
    const nameResult = readDnsName(response, offset);
    let current = nameResult.offsetAfter;

    if (current + 10 > response.length) throw new Error('DNS resource record out of bounds');
    const type = response.readUInt16BE(current);
    const ttl = response.readUInt32BE(current + 4);
    const rdlength = response.readUInt16BE(current + 8);
    const rdataOffset = current + 10;
    const offsetAfter = rdataOffset + rdlength;
    if (offsetAfter > response.length) throw new Error('DNS resource record rdata out of bounds');

    const name = normalizeDnsName(nameResult.name);

    if (type === DNS_RECORD_TYPES.A) {
      answers.push({
        name,
        type,
        TTL: ttl,
        data: formatIpv4(response.subarray(rdataOffset, rdataOffset + rdlength)),
      });
    } else if (type === DNS_RECORD_TYPES.AAAA) {
      answers.push({
        name,
        type,
        TTL: ttl,
        data: formatIpv6(response.subarray(rdataOffset, rdataOffset + rdlength)),
      });
    } else if (type === DNS_RECORD_TYPES.CNAME) {
      const target = readDnsName(response, rdataOffset).name;
      answers.push({
        name,
        type,
        TTL: ttl,
        data: normalizeDnsName(target),
      });
    }

    offset = offsetAfter;
  }

  const json: DnsJsonResponse = {
    Status: getRcodeFromFlags(flags),
    TC: isFlagSet(flags, 0x0200),
    RD: isFlagSet(flags, 0x0100),
    RA: isFlagSet(flags, 0x0080),
    AD: isFlagSet(flags, 0x0020),
    CD: isFlagSet(flags, 0x0010),
    Question: [question],
  };

  if (answers.length > 0) {
    json.Answer = answers;
  }

  return json;
}

