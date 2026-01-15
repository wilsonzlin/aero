export function stripIpv6ZoneIndex(address: string): string {
  const idx = address.indexOf("%");
  if (idx === -1) return address;
  return address.slice(0, idx);
}

