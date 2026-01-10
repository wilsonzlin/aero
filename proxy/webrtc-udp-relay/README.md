# WebRTC UDP Relay (server-side)

This component relays UDP datagrams from a WebRTC client to a UDP destination on the server's network.

## Security model (read before deploying)

A UDP relay is **network egress**. If you run it on an Internet-reachable host without destination controls, it can become an **open proxy / SSRF primitive** that attackers can use to:

- scan internal networks (`10.0.0.0/8`, `192.168.0.0/16`, etc.)
- hit cloud metadata endpoints
- attack link-local services
- abuse your host as a generic UDP reflector

To mitigate this, the relay enforces an outbound destination policy (`internal/policy.DestinationPolicy`) on **every outbound UDP datagram** (and can also drop inbound datagrams from denied sources).

### Safe defaults

By default, the policy is **deny-by-default** and denies common private/special IPv4 ranges unless explicitly enabled.

In other words: if you deploy the relay without any configuration, it should **not** be able to reach arbitrary network targets.

## Configuration

The relay is intended to be configured via environment variables:

- `DESTINATION_POLICY_PRESET`:
  - `production` / `prod` (default): deny by default (requires explicit allow rules)
  - `dev`: allow by default (still applies deny rules)
- `ALLOW_PRIVATE_NETWORKS` (`true`/`false`, default depends on preset): when `false`, the policy denies at minimum:
  - `127.0.0.0/8` (loopback)
  - `169.254.0.0/16` (link-local)
  - `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` (RFC1918)
  - `100.64.0.0/10` (CGNAT)
  - `224.0.0.0/4` (multicast)
  - `0.0.0.0/8`, `240.0.0.0/4` (reserved)
  - `255.255.255.255/32` (broadcast)
- `ALLOW_UDP_CIDRS`: comma-separated CIDRs to allow (e.g. `1.1.1.1/32,8.8.8.0/24`)
- `DENY_UDP_CIDRS`: comma-separated CIDRs to deny (evaluated before allow)
- `ALLOW_UDP_PORTS`: comma-separated ports/ranges to allow (e.g. `53,123,30000-30100`)
- `DENY_UDP_PORTS`: comma-separated ports/ranges to deny (evaluated before allow)

### Examples

Allow only public DNS in production:

```bash
export DESTINATION_POLICY_PRESET=production
export ALLOW_PRIVATE_NETWORKS=false
export ALLOW_UDP_CIDRS="1.1.1.1/32,8.8.8.8/32"
export ALLOW_UDP_PORTS="53"
```

Allow any destination (development only):

```bash
export DESTINATION_POLICY_PRESET=dev
```

