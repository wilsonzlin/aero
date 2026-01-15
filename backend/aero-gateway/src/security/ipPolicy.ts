/**
 * TCP proxy IP egress policy helpers.
 *
 * Keep this module dependency-free (node built-ins only) so it can be used in
 * lightweight unit tests without requiring `npm install`.
 */

const MAX_IPV6_LITERAL_LEN = 128;
const MAX_IPV4_DOTTED_DECIMAL_LEN = 15;

export function isPublicIpAddress(ip: string): boolean {
  const v4 = parseIpv4(ip);
  if (v4) return isPublicIpv4(v4);

  const v6 = parseIpv6(ip);
  if (v6) return isPublicIpv6(v6);

  // If we cannot parse it, treat as non-public / disallowed.
  return false;
}

export function normalizeIpv4Literal(ip: string): string | null {
  const v4 = parseIpv4(ip);
  if (!v4) return null;
  return `${v4[0]}.${v4[1]}.${v4[2]}.${v4[3]}`;
}

export function normalizeIpv6Literal(ip: string): string | null {
  const v6 = parseIpv6(ip);
  if (!v6) return null;

  let out = "";
  for (let i = 0; i < 16; i += 2) {
    const n = (v6[i]! << 8) | v6[i + 1]!;
    // Emit a fixed-width representation to keep comparisons stable across
    // different IPv6 textual forms (e.g. "::1" vs "0:0:0:0:0:0:0:1").
    const hex = n.toString(16).padStart(4, "0");
    out = out === "" ? hex : `${out}:${hex}`;
  }
  return out;
}

function parseIpv4(ip: string): Uint8Array | null {
  // Manual parser: avoid allocations and regex overhead in this hot path.
  //
  // We intentionally accept several non-canonical IPv4 forms supported by
  // `dns.lookup()`/`getaddrinfo()` on common platforms:
  // - Hex components: 0x7f.0.0.1
  // - Octal components: 0177.0.0.1
  // - Shorthand forms: 127.1, 1.2.3, 2130706433
  //
  // This avoids surprising false negatives when callers pass host strings that
  // Node can successfully resolve as numeric addresses.
  const ipLen = ip.length;
  if (ipLen === 0) return null;

  // Some platforms accept dotted-quad IPv4 literals with a single trailing dot,
  // but interpret them as *decimal* only (no inet_aton-style octal/hex parsing).
  // Example: "010.0.0.1." -> "10.0.0.1".
  if (ip.charCodeAt(ipLen - 1) === 0x2e /* '.' */) {
    if (ipLen < 2) return null;
    if (ip.charCodeAt(ipLen - 2) === 0x2e /* '.' */) return null;

    const bytes = new Uint8Array(4);
    if (!tryParseIpv4DottedQuadDecimal(ip.slice(0, -1), bytes)) return null;
    return bytes;
  }

  let dots = 0;
  for (let i = 0; i < ipLen; i += 1) {
    if (ip.charCodeAt(i) === 0x2e /* '.' */) dots += 1;
  }
  if (dots > 3) return null;

  if (dots === 3) {
    const bytes = new Uint8Array(4);
    if (tryParseIpv4DottedQuadBase0(ip, bytes)) return bytes;
    if (tryParseIpv4DottedQuadDecimal(ip, bytes)) return bytes;
    return null;
  }

  // 1..3 part inet_aton-style forms.
  const values: number[] = [];
  let start = 0;
  for (let i = 0; i <= ipLen; i += 1) {
    const isDot = i !== ipLen && ip.charCodeAt(i) === 0x2e /* '.' */;
    if (!isDot && i !== ipLen) continue;

    if (i === start) return null; // empty component
    const value = parseIpv4PartBase0(ip, start, i);
    if (value === null) return null;
    values.push(value);
    start = i + 1;
  }

  if (values.length !== dots + 1) return null;

  const bytes = new Uint8Array(4);

  if (values.length === 1) {
    const n = values[0]!;
    if (n > 0xffffffff) return null;
    bytes[0] = (n >>> 24) & 0xff;
    bytes[1] = (n >>> 16) & 0xff;
    bytes[2] = (n >>> 8) & 0xff;
    bytes[3] = n & 0xff;
    return bytes;
  }

  if (values.length === 2) {
    const a = values[0]!;
    const b = values[1]!;
    if (a > 0xff || b > 0xffffff) return null;
    bytes[0] = a;
    bytes[1] = (b >>> 16) & 0xff;
    bytes[2] = (b >>> 8) & 0xff;
    bytes[3] = b & 0xff;
    return bytes;
  }

  if (values.length === 3) {
    const a = values[0]!;
    const b = values[1]!;
    const c = values[2]!;
    if (a > 0xff || b > 0xff || c > 0xffff) return null;
    bytes[0] = a;
    bytes[1] = b;
    bytes[2] = (c >>> 8) & 0xff;
    bytes[3] = c & 0xff;
    return bytes;
  }

  // dots === 3 handled above; any other count is invalid.
  return null;
}

function tryParseIpv4DottedQuadBase0(ip: string, out: Uint8Array): boolean {
  const ipLen = ip.length;
  let part = 0;
  let start = 0;

  for (let i = 0; i <= ipLen; i += 1) {
    const isDot = i !== ipLen && ip.charCodeAt(i) === 0x2e /* '.' */;
    if (!isDot && i !== ipLen) continue;

    if (part >= 4) return false;
    if (i === start) return false; // empty component

    const value = parseIpv4PartBase0(ip, start, i);
    if (value === null || value > 255) return false;
    out[part] = value;
    part += 1;
    start = i + 1;
  }

  return part === 4;
}

function tryParseIpv4DottedQuadDecimal(ip: string, out: Uint8Array): boolean {
  const ipLen = ip.length;
  let part = 0;
  let value = 0;
  let digits = 0;

  for (let i = 0; i <= ipLen; i += 1) {
    if (i === ipLen || ip.charCodeAt(i) === 0x2e /* '.' */) {
      if (digits === 0) return false;
      if (part >= 4) return false;
      out[part] = value;
      part += 1;
      value = 0;
      digits = 0;
      continue;
    }

    const c = ip.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return false;
    value = value * 10 + (c - 0x30);
    if (value > 255) return false;
    digits += 1;
  }

  return part === 4;
}

function parseIpv4PartBase0(ip: string, start: number, end: number): number | null {
  // Base-0 parsing:
  // - "0x" => hex
  // - leading "0" (with more digits) => octal
  // - otherwise => decimal
  if (start >= end) return null;

  let base = 10;
  let i = start;

  if (ip.charCodeAt(i) === 0x30 /* '0' */ && end - start > 1) {
    const next = ip.charCodeAt(i + 1);
    if (next === 0x78 /* 'x' */ || next === 0x58 /* 'X' */) {
      base = 16;
      i += 2;
      if (i >= end) return null;
    } else {
      base = 8;
    }
  }

  let n = 0;
  for (; i < end; i += 1) {
    const c = ip.charCodeAt(i);
    let v: number;

    if (base === 16) {
      if (c >= 0x30 /* '0' */ && c <= 0x39 /* '9' */) v = c - 0x30;
      else if (c >= 0x61 /* 'a' */ && c <= 0x66 /* 'f' */) v = c - 0x61 + 10;
      else if (c >= 0x41 /* 'A' */ && c <= 0x46 /* 'F' */) v = c - 0x41 + 10;
      else return null;
      n = n * 16 + v;
    } else if (base === 8) {
      if (c < 0x30 /* '0' */ || c > 0x37 /* '7' */) return null;
      v = c - 0x30;
      n = n * 8 + v;
    } else {
      if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return null;
      v = c - 0x30;
      n = n * 10 + v;
    }

    // Keep within an unsigned 32-bit range (also avoids loss of precision).
    if (n > 0xffffffff) return null;
  }

  return n;
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
  if (ip.length > MAX_IPV6_LITERAL_LEN) return null;

  // Handle IPv4-mapped IPv6 like ::ffff:192.168.0.1.
  //
  // Note: the embedded IPv4 part must be in canonical dotted-decimal form
  // (e.g. "::ffff:127.0.0.1"). We intentionally do NOT accept non-canonical
  // inet_aton-style variants here (like "::ffff:010.0.0.1"), since Node's
  // resolver does not treat those as numeric IPv6 literals.
  const hasV4Tail = ip.includes(".");
  let v4Tail: Uint8Array | null = null;
  let ipHead = ip;
  if (hasV4Tail) {
    const lastColon = ip.lastIndexOf(":");
    if (lastColon === -1) return null;
    v4Tail = parseIpv4DottedDecimalStrict(ip.slice(lastColon + 1));
    if (!v4Tail) return null;
    // Preserve "::" when the IPv4 tail is preceded by a compression marker.
    // Example: "::192.0.2.1" and "2001:db8::192.0.2.1" should keep the full "::" in the head.
    const keepColon = lastColon > 0 && ip.charCodeAt(lastColon - 1) === 0x3a /* ':' */;
    ipHead = ip.slice(0, keepColon ? lastColon + 1 : lastColon);
  }

  const doubleColon = ipHead.indexOf("::");
  let hasCompression = false;
  let leftStart = 0;
  let leftEnd = ipHead.length;
  let rightStart = ipHead.length;
  let rightEnd = ipHead.length;
  if (doubleColon !== -1) {
    // Only one "::" is allowed.
    if (ipHead.indexOf("::", doubleColon + 2) !== -1) return null;
    hasCompression = true;
    leftEnd = doubleColon;
    rightStart = doubleColon + 2;
  }

  const leftNums = parseIpv6Hextets(ipHead, leftStart, leftEnd);
  if (!leftNums) return null;
  const rightNums = hasCompression ? parseIpv6Hextets(ipHead, rightStart, rightEnd) : [];
  if (!rightNums) return null;

  const v4Groups = v4Tail ? 2 : 0;
  const totalGroups = leftNums.length + rightNums.length + v4Groups;
  if (!hasCompression && totalGroups !== 8) return null;
  if (hasCompression) {
    if (totalGroups > 8) return null;
    // "::" must compress at least one 16-bit group.
    if (totalGroups === 8) return null;
  }

  const zerosToInsert = hasCompression ? 8 - totalGroups : 0;
  const groups = [...leftNums, ...new Array(zerosToInsert).fill(0), ...rightNums];
  if (v4Tail) groups.push(0, 0);
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

function parseIpv6Hextets(s: string, start: number, end: number): number[] | null {
  if (start === end) return [];
  const out: number[] = [];

  let i = start;
  while (i < end) {
    // Reject empty groups (leading ":" / ":::").
    if (s.charCodeAt(i) === 0x3a /* ':' */) return null;

    const groupStart = i;
    while (i < end && s.charCodeAt(i) !== 0x3a /* ':' */) i += 1;
    const groupEnd = i;

    const n = parseHex16Span(s, groupStart, groupEnd);
    if (n === null) return null;
    out.push(n);
    if (out.length > 8) return null;

    if (i < end) {
      // Skip ':'
      i += 1;
      // Reject trailing ":" (e.g. "1:2:") and empty groups (e.g. "1:::"; "::" is handled outside).
      if (i >= end) return null;
      if (s.charCodeAt(i) === 0x3a /* ':' */) return null;
    }
  }

  return out;
}

function parseIpv4DottedDecimalStrict(ip: string): Uint8Array | null {
  // Strict dotted-decimal parser matching node:net's isIP() behavior:
  // - exactly 4 decimal components
  // - no leading zeros (except the single digit "0")
  if (ip.length > MAX_IPV4_DOTTED_DECIMAL_LEN) return null;
  const bytes = new Uint8Array(4);
  let part = 0;
  let value = 0;
  let digits = 0;
  let leadingZero = false;

  for (let i = 0; i <= ip.length; i += 1) {
    if (i === ip.length || ip.charCodeAt(i) === 0x2e /* '.' */) {
      if (digits === 0) return null;
      if (leadingZero && digits > 1) return null;
      if (part >= 4) return null;
      bytes[part] = value;
      part += 1;
      value = 0;
      digits = 0;
      leadingZero = false;
      continue;
    }

    const c = ip.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) return null;
    if (digits === 0) {
      leadingZero = c === 0x30 /* '0' */;
    } else if (leadingZero) {
      // "00", "01", "001", etc are invalid.
      return null;
    }
    value = value * 10 + (c - 0x30);
    if (value > 255) return null;
    digits += 1;
  }

  return part === 4 ? bytes : null;
}

function parseHex16Span(s: string, start: number, end: number): number | null {
  const len = end - start;
  if (len < 1 || len > 4) return null;
  let n = 0;
  for (let i = start; i < end; i += 1) {
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

  // IPv4-compatible ::/96 (deprecated but still accepted by some stacks) => apply IPv4 rules.
  // This prevents bypassing IPv4 private-range checks by spelling an IPv4 address as "::10.0.0.1".
  if (bytes.slice(0, 12).every((b) => b === 0)) {
    return isPublicIpv4(bytes.slice(12, 16));
  }

  // Documentation 2001:db8::/32
  if (bytes[0] === 0x20 && bytes[1] === 0x01 && bytes[2] === 0x0d && bytes[3] === 0xb8) return false;

  return true;
}
