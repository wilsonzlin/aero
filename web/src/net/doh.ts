export interface DohAResult {
  address: string;
  ttl: number;
}

/**
 * Resolve an A record using DNS-over-HTTPS (DoH) JSON.
 *
 * This matches the needs of the Rust net stack: the host turns `Action::DnsResolve { name }` into
 * a DoH query and returns the resolved address + TTL back into the stack.
 */
export async function resolveAOverDohJson(
  name: string,
  endpoint = "https://cloudflare-dns.com/dns-query",
): Promise<DohAResult | null> {
  const url = new URL(endpoint);
  url.searchParams.set("name", name);
  url.searchParams.set("type", "A");

  const resp = await fetch(url.toString(), {
    headers: {
      Accept: "application/dns-json",
    },
  });
  if (!resp.ok) return null;

  const json: unknown = await resp.json();
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

