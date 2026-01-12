/**
 * TCP proxy IP egress policy helpers.
 *
 * Keep this module dependency-free (node built-ins only) so it can be used in
 * lightweight unit tests without requiring `npm install`.
 */

export function isPublicIpAddress(ip: string): boolean {
  const v4 = parseIpv4(ip);
  if (v4) return isPublicIpv4(v4);

  const v6 = parseIpv6(ip);
  if (v6) return isPublicIpv6(v6);

  // If we cannot parse it, treat as non-public / disallowed.
  return false;
}

function parseIpv4(ip: string): Uint8Array | null {
  // Manual parser: avoid allocations and regex overhead in this hot path.
  const bytes = new Uint8Array(4);
  let part = 0;
  let value = 0;
  let digits = 0;

  for (let i = 0; i < ip.length; i++) {
    const c = ip.charCodeAt(i);

    if (c === 0x2e /* '.' */) {
      if (digits === 0) return null;
      if (part >= 4) return null;
      bytes[part] = value;
      part += 1;
      value = 0;
      digits = 0;
      continue;
    }

    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return null;
    if (digits >= 3) return null;
    value = value * 10 + (c - 0x30);
    if (value > 255) return null;
    digits += 1;
  }

  if (part !== 3 || digits === 0) return null;
  bytes[part] = value;
  return bytes;
}

function isPublicIpv4(bytes: Uint8Array): boolean {
  const a = bytes[0]!;
  const b = bytes[1]!;

  // 0.0.0.0/8 (this network)
  if (a === 0) return false;
  // 10.0.0.0/8 (RFC1918)
  if (a === 10) return false;
  // 100.64.0.0/10 (CGNAT)
  if (a === 100 && b >= 64 && b <= 127) return false;
  // 127.0.0.0/8 (loopback)
  if (a === 127) return false;
  // 169.254.0.0/16 (link-local)
  if (a === 169 && b === 254) return false;
  // 172.16.0.0/12 (RFC1918)
  if (a === 172 && b >= 16 && b <= 31) return false;
  // 192.168.0.0/16 (RFC1918)
  if (a === 192 && b === 168) return false;

  // 192.0.0.0/24 (IETF Protocol Assignments)
  if (a === 192 && b === 0 && bytes[2] === 0) return false;
  // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 (TEST-NET)
  if (a === 192 && b === 0 && bytes[2] === 2) return false;
  if (a === 198 && b === 51 && bytes[2] === 100) return false;
  if (a === 203 && b === 0 && bytes[2] === 113) return false;
  // 198.18.0.0/15 (benchmarking)
  if (a === 198 && (b === 18 || b === 19)) return false;

  // Multicast 224.0.0.0/4 and reserved 240.0.0.0/4
  if (a >= 224) return false;

  return true;
}

function parseIpv6(ip: string): Uint8Array | null {
  // Handle IPv4-mapped IPv6 like ::ffff:192.168.0.1
  const hasV4Tail = ip.includes(".");
  let v4Tail: Uint8Array | null = null;
  let ipHead = ip;
  if (hasV4Tail) {
    const lastColon = ip.lastIndexOf(":");
    if (lastColon === -1) return null;
    v4Tail = parseIpv4(ip.slice(lastColon + 1));
    if (!v4Tail) return null;
    ipHead = ip.slice(0, lastColon) + ":0:0"; // placeholder for 2 groups
  }

  const pieces = ipHead.split("::");
  if (pieces.length > 2) return null;

  const left = pieces[0] ? pieces[0].split(":").filter(Boolean) : [];
  const right = pieces[1] ? pieces[1].split(":").filter(Boolean) : [];

  const leftNums: number[] = [];
  const rightNums: number[] = [];

  for (const part of left) {
    const n = parseHex16(part);
    if (n === null) return null;
    leftNums.push(n);
  }
  for (const part of right) {
    const n = parseHex16(part);
    if (n === null) return null;
    rightNums.push(n);
  }

  const totalGroups = leftNums.length + rightNums.length;
  const needsCompression = pieces.length === 2;
  if (!needsCompression && totalGroups !== 8) return null;
  if (needsCompression && totalGroups > 8) return null;

  const zerosToInsert = needsCompression ? 8 - totalGroups : 0;
  const groups = [...leftNums, ...new Array(zerosToInsert).fill(0), ...rightNums];
  if (groups.length !== 8) return null;

  const bytes = new Uint8Array(16);
  for (let i = 0; i < 8; i++) {
    const n = groups[i]!;
    bytes[i * 2] = (n >> 8) & 0xff;
    bytes[i * 2 + 1] = n & 0xff;
  }

  if (v4Tail) {
    // Replace the last 32 bits with the IPv4 tail (we used a placeholder above).
    bytes[12] = v4Tail[0]!;
    bytes[13] = v4Tail[1]!;
    bytes[14] = v4Tail[2]!;
    bytes[15] = v4Tail[3]!;
  }

  return bytes;
}

function parseHex16(s: string): number | null {
  if (s.length < 1 || s.length > 4) return null;
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    let v: number;
    if (c >= 0x30 /* '0' */ && c <= 0x39 /* '9' */) {
      v = c - 0x30;
    } else if (c >= 0x61 /* 'a' */ && c <= 0x66 /* 'f' */) {
      v = c - 0x61 + 10;
    } else if (c >= 0x41 /* 'A' */ && c <= 0x46 /* 'F' */) {
      v = c - 0x41 + 10;
    } else {
      return null;
    }
    n = (n << 4) | v;
  }
  return n;
}

function isPublicIpv6(bytes: Uint8Array): boolean {
  // Unspecified ::/128
  if (bytes.every((b) => b === 0)) return false;
  // Loopback ::1/128
  if (bytes.slice(0, 15).every((b) => b === 0) && bytes[15] === 1) return false;

  // Multicast ff00::/8
  if (bytes[0] === 0xff) return false;

  // Link-local fe80::/10
  if (bytes[0] === 0xfe && (bytes[1]! & 0xc0) === 0x80) return false;

  // Unique local fc00::/7
  if ((bytes[0]! & 0xfe) === 0xfc) return false;

  // IPv4-mapped ::ffff:0:0/96 => apply IPv4 rules
  const isV4Mapped =
    bytes.slice(0, 10).every((b) => b === 0) && bytes[10] === 0xff && bytes[11] === 0xff;
  if (isV4Mapped) {
    return isPublicIpv4(bytes.slice(12, 16));
  }

  // Documentation 2001:db8::/32
  if (bytes[0] === 0x20 && bytes[1] === 0x01 && bytes[2] === 0x0d && bytes[3] === 0xb8) return false;

  return true;
}
