import net from 'node:net';

function parseIpv4ToBytes(ip: string): [number, number, number, number] | null {
  if (net.isIP(ip) !== 4) return null;
  const parts = ip.split('.');
  if (parts.length !== 4) return null;
  const nums = parts.map((p) => Number(p));
  if (nums.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return null;
  return [nums[0], nums[1], nums[2], nums[3]];
}

function parseIpv6ToHextets(ip: string): number[] | null {
  if (ip.includes('%')) return null;
  if (net.isIP(ip) !== 6) return null;

  const [headRaw, tailRaw] = ip.split('::');
  const hasCompression = ip.includes('::');
  if (ip.split('::').length > 2) return null;

  const parseParts = (raw: string): number[] => {
    if (raw.length === 0) return [];
    return raw.split(':').flatMap((p) => {
      if (p.includes('.')) {
        const bytes = parseIpv4ToBytes(p);
        if (!bytes) return [NaN];
        return [(bytes[0] << 8) | bytes[1], (bytes[2] << 8) | bytes[3]];
      }
      const n = Number.parseInt(p, 16);
      return [n];
    });
  };

  const head = parseParts(headRaw ?? '');
  const tail = hasCompression ? parseParts(tailRaw ?? '') : [];
  if (head.some((n) => !Number.isInteger(n) || n < 0 || n > 0xffff)) return null;
  if (tail.some((n) => !Number.isInteger(n) || n < 0 || n > 0xffff)) return null;

  const total = head.length + tail.length;
  if (!hasCompression && total !== 8) return null;
  if (hasCompression && total > 8) return null;

  const zeros = hasCompression ? 8 - total : 0;
  return [...head, ...new Array(zeros).fill(0), ...tail];
}

export function isPrivateIp(ip: string): boolean {
  const v4 = parseIpv4ToBytes(ip);
  if (v4) {
    const [a, b, c, d] = v4;
    if (a === 10) return true;
    if (a === 127) return true;
    if (a === 0) return true;
    if (a === 169 && b === 254) return true;
    if (a === 172 && b >= 16 && b <= 31) return true;
    if (a === 192 && b === 168) return true;
    if (a === 100 && b >= 64 && b <= 127) return true;
    if (a >= 224) return true; // multicast/reserved
    if (a === 255 && b === 255 && c === 255 && d === 255) return true;
    return false;
  }

  const hextets = parseIpv6ToHextets(ip);
  if (!hextets) return false;

  const [h0, h1] = hextets;

  const isLoopback = hextets.slice(0, 7).every((h) => h === 0) && hextets[7] === 1;
  if (isLoopback) return true;

  const isUnspecified = hextets.every((h) => h === 0);
  if (isUnspecified) return true;

  if ((h0 & 0xfe00) === 0xfc00) return true;

  if ((h0 & 0xffc0) === 0xfe80) return true;

  const isV4Mapped = hextets.slice(0, 5).every((h) => h === 0) && hextets[5] === 0xffff;
  if (isV4Mapped) {
    const v4FromMapped = `${hextets[6] >> 8}.${hextets[6] & 0xff}.${hextets[7] >> 8}.${
      hextets[7] & 0xff
    }`;
    return isPrivateIp(v4FromMapped);
  }

  return false;
}
