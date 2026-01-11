// Minimal, dependency-free subset of `ipaddr.js` used by Aero's Node utilities.
//
// Like `ws-shim.mjs`, this exists so `node --test` can run in an offline agent
// environment without `node_modules/`.
//
// It is **not** a full replacement for ipaddr.js; it implements only the APIs
// used in this repo:
// - isValid(str)
// - parse(str) -> IPv4/IPv6 object with:
//     - kind(), toByteArray(), toString(), range(), match([addr, prefixLen])
// - fromByteArray(bytes)
// - parseCIDR("addr/prefix") -> [addrObj, prefixLen]
//
// For IPv6, we support basic `::` compression and IPv4-mapped tails.

import net from "node:net";

function stripOptionalBrackets(address) {
  const trimmed = String(address).trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) return trimmed.slice(1, -1);
  return trimmed;
}

function parseIpv4ToBytes(address) {
  const parts = stripOptionalBrackets(address).split(".");
  if (parts.length !== 4) throw new Error(`Invalid IPv4 address: ${address}`);
  const bytes = new Uint8Array(4);
  for (let i = 0; i < 4; i += 1) {
    const n = Number(parts[i]);
    if (!Number.isInteger(n) || n < 0 || n > 255) {
      throw new Error(`Invalid IPv4 address: ${address}`);
    }
    bytes[i] = n;
  }
  return bytes;
}

function ipv4BytesToString(bytes) {
  return `${bytes[0]}.${bytes[1]}.${bytes[2]}.${bytes[3]}`;
}

function parseIpv6ToBytes(address) {
  let ip = stripOptionalBrackets(address);
  const zoneIdx = ip.indexOf("%");
  if (zoneIdx !== -1) ip = ip.slice(0, zoneIdx);
  ip = ip.toLowerCase();

  const pieces = ip.split("::");
  if (pieces.length > 2) throw new Error(`Invalid IPv6 address: ${address}`);

  const left = pieces[0];
  const right = pieces.length === 2 ? pieces[1] : null;

  const leftParts = left.length > 0 ? left.split(":") : [];
  const rightParts = right && right.length > 0 ? right.split(":") : [];

  const parseParts = (parts) => {
    /** @type {number[]} */
    const out = [];
    for (const part of parts) {
      if (part === "") continue;
      if (part.includes(".")) {
        const v4 = parseIpv4ToBytes(part);
        // eslint-disable-next-line no-bitwise
        out.push(((v4[0] << 8) | v4[1]) >>> 0);
        // eslint-disable-next-line no-bitwise
        out.push(((v4[2] << 8) | v4[3]) >>> 0);
        continue;
      }
      const n = Number.parseInt(part, 16);
      if (!Number.isFinite(n) || n < 0 || n > 0xffff) throw new Error(`Invalid IPv6 hextet: ${part}`);
      out.push(n);
    }
    return out;
  };

  const leftHextets = parseParts(leftParts);
  const rightHextets = parseParts(rightParts);

  /** @type {number[]} */
  let hextets;
  if (right !== null) {
    const missing = 8 - (leftHextets.length + rightHextets.length);
    if (missing < 0) throw new Error(`Invalid IPv6 address: ${address}`);
    hextets = [...leftHextets, ...Array(missing).fill(0), ...rightHextets];
  } else {
    if (leftHextets.length !== 8) throw new Error(`Invalid IPv6 address: ${address}`);
    hextets = leftHextets;
  }

  if (hextets.length !== 8) throw new Error(`Invalid IPv6 address: ${address}`);

  const bytes = new Uint8Array(16);
  for (let i = 0; i < 8; i += 1) {
    const v = hextets[i];
    // eslint-disable-next-line no-bitwise
    bytes[i * 2] = (v >> 8) & 0xff;
    // eslint-disable-next-line no-bitwise
    bytes[i * 2 + 1] = v & 0xff;
  }
  return bytes;
}

function ipv6BytesToString(bytes) {
  /** @type {number[]} */
  const hextets = [];
  for (let i = 0; i < 16; i += 2) {
    // eslint-disable-next-line no-bitwise
    hextets.push(((bytes[i] << 8) | bytes[i + 1]) >>> 0);
  }

  // Find longest run of zeros (length >= 2) for :: compression.
  let bestStart = -1;
  let bestLen = 0;
  let curStart = -1;
  let curLen = 0;
  for (let i = 0; i <= 8; i += 1) {
    const isZero = i < 8 && hextets[i] === 0;
    if (isZero) {
      if (curStart === -1) curStart = i;
      curLen += 1;
      continue;
    }
    if (curStart !== -1 && curLen >= 2 && curLen > bestLen) {
      bestStart = curStart;
      bestLen = curLen;
    }
    curStart = -1;
    curLen = 0;
  }

  /** @type {string[]} */
  const parts = [];
  for (let i = 0; i < 8; i += 1) {
    if (bestStart !== -1 && i === bestStart) {
      // Ensure that a compressed prefix renders as `::` rather than `:`.
      // `["", "1"].join(":")` => `:1`, but `["", "", "1"].join(":")` => `::1`.
      parts.push("");
      if (i === 0) parts.push("");
      i += bestLen - 1;
      if (i === 7) parts.push("");
      continue;
    }
    parts.push(hextets[i].toString(16));
  }

  const joined = parts.join(":");
  // Collapse any ":::"
  return joined.replace(/:{3,}/g, "::");
}

function matchPrefixBytes(aBytes, bBytes, prefixLenBits) {
  const fullBytes = Math.floor(prefixLenBits / 8);
  const remBits = prefixLenBits % 8;
  for (let i = 0; i < fullBytes; i += 1) {
    if (aBytes[i] !== bBytes[i]) return false;
  }
  if (remBits === 0) return true;
  // eslint-disable-next-line no-bitwise
  const mask = (0xff << (8 - remBits)) & 0xff;
  // eslint-disable-next-line no-bitwise
  return (aBytes[fullBytes] & mask) === (bBytes[fullBytes] & mask);
}

class IPv4 {
  /** @param {Uint8Array} bytes */
  constructor(bytes) {
    this._bytes = bytes;
  }

  kind() {
    return "ipv4";
  }

  toByteArray() {
    return Array.from(this._bytes);
  }

  toString() {
    return ipv4BytesToString(this._bytes);
  }

  match(cidr) {
    const [addr, prefixLen] = cidr;
    if (!addr || typeof addr.kind !== "function" || addr.kind() !== "ipv4") return false;
    if (!Number.isInteger(prefixLen) || prefixLen < 0 || prefixLen > 32) return false;
    return matchPrefixBytes(this._bytes, addr._bytes ?? Uint8Array.from(addr.toByteArray()), prefixLen);
  }

  range() {
    const a = this._bytes[0];
    const b = this._bytes[1];
    const c = this._bytes[2];
    const d = this._bytes[3];

    if (a === 0 && b === 0 && c === 0 && d === 0) return "unspecified";
    if (a === 127) return "loopback";
    if (a === 10) return "private";
    if (a === 172 && b >= 16 && b <= 31) return "private";
    if (a === 192 && b === 168) return "private";
    if (a === 169 && b === 254) return "linkLocal";
    if (a === 100 && b >= 64 && b <= 127) return "carrierGradeNat";

    // 198.18.0.0/15 (benchmarking)
    if (a === 198 && (b === 18 || b === 19)) return "benchmarking";

    // Documentation ranges (TEST-NET) are not public unicast.
    if (a === 192 && b === 0 && c === 2) return "reserved";
    if (a === 198 && b === 51 && c === 100) return "reserved";
    if (a === 203 && b === 0 && c === 113) return "reserved";

    if (a >= 224 && a <= 239) return "multicast";
    if (a === 255 && b === 255 && c === 255 && d === 255) return "broadcast";
    if (a >= 240) return "reserved";
    return "unicast";
  }
}

class IPv6 {
  /** @param {Uint8Array} bytes */
  constructor(bytes) {
    this._bytes = bytes;
  }

  kind() {
    return "ipv6";
  }

  toByteArray() {
    return Array.from(this._bytes);
  }

  toString() {
    return ipv6BytesToString(this._bytes);
  }

  isIPv4MappedAddress() {
    for (let i = 0; i < 10; i += 1) {
      if (this._bytes[i] !== 0) return false;
    }
    return this._bytes[10] === 0xff && this._bytes[11] === 0xff;
  }

  toIPv4Address() {
    if (!this.isIPv4MappedAddress()) throw new Error("not an IPv4-mapped address");
    return new IPv4(this._bytes.subarray(12, 16));
  }

  match(cidr) {
    const [addr, prefixLen] = cidr;
    if (!addr || typeof addr.kind !== "function" || addr.kind() !== "ipv6") return false;
    if (!Number.isInteger(prefixLen) || prefixLen < 0 || prefixLen > 128) return false;
    return matchPrefixBytes(this._bytes, addr._bytes ?? Uint8Array.from(addr.toByteArray()), prefixLen);
  }

  range() {
    // ::/128
    if (this._bytes.every((b) => b === 0)) return "unspecified";
    // ::1/128
    let loopback = true;
    for (let i = 0; i < 15; i += 1) loopback = loopback && this._bytes[i] === 0;
    if (loopback && this._bytes[15] === 1) return "loopback";
    // ff00::/8
    if (this._bytes[0] === 0xff) return "multicast";
    // fc00::/7
    // eslint-disable-next-line no-bitwise
    if ((this._bytes[0] & 0xfe) === 0xfc) return "uniqueLocal";
    // fe80::/10
    // eslint-disable-next-line no-bitwise
    if (this._bytes[0] === 0xfe && (this._bytes[1] & 0xc0) === 0x80) return "linkLocal";
    if (this.isIPv4MappedAddress()) return "ipv4Mapped";
    return "unicast";
  }
}

function parse(address) {
  const cleaned = stripOptionalBrackets(address);
  const kind = net.isIP(cleaned);
  if (kind === 4) return new IPv4(parseIpv4ToBytes(cleaned));
  if (kind === 6) return new IPv6(parseIpv6ToBytes(cleaned));
  throw new Error(`Invalid IP address: ${address}`);
}

function fromByteArray(bytes) {
  const arr = Uint8Array.from(bytes);
  if (arr.length === 4) return new IPv4(arr);
  if (arr.length === 16) return new IPv6(arr);
  throw new Error(`Invalid byte array length: ${arr.length}`);
}

function parseCIDR(cidr) {
  const trimmed = String(cidr).trim();
  const idx = trimmed.lastIndexOf("/");
  if (idx === -1) throw new Error(`Invalid CIDR: ${cidr}`);
  const addrPart = trimmed.slice(0, idx);
  const prefixPart = trimmed.slice(idx + 1);
  const addr = parse(addrPart);
  const prefixLen = Number(prefixPart);
  if (!Number.isInteger(prefixLen)) throw new Error(`Invalid CIDR prefix length: ${cidr}`);
  const max = addr.kind() === "ipv4" ? 32 : 128;
  if (prefixLen < 0 || prefixLen > max) throw new Error(`Invalid CIDR prefix length: ${cidr}`);
  return [addr, prefixLen];
}

function isValid(address) {
  const cleaned = stripOptionalBrackets(address);
  return net.isIP(cleaned) !== 0;
}

const ipaddr = {
  IPv4,
  IPv6,
  isValid,
  parse,
  fromByteArray,
  parseCIDR,
};

export default ipaddr;
