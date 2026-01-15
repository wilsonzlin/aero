import { isPublicIpAddress } from "../security/ipPolicy.js";

export type DnsLookupAddress = Readonly<{
  address: string;
  family?: number;
}>;

export function selectAllowedDnsAddress(
  addresses: readonly DnsLookupAddress[],
  allowPrivateIps: boolean,
): DnsLookupAddress | null {
  if (addresses.length === 0) return null;
  if (allowPrivateIps) return addresses[0] ?? null;

  for (const addr of addresses) {
    if (isPublicIpAddress(addr.address)) return addr;
  }
  return null;
}

