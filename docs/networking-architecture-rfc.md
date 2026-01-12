# Networking Architecture RFC: slirp-in-browser vs L3/L2 tunneling

> **Final decision:** [ADR 0013: Networking via L2 tunnel (Option C) to an unprivileged proxy](./adr/0013-networking-l2-tunnel.md).  
> This RFC is retained for background, tradeoff analysis, and prototype references.

## Context / goal

Aero needs guest networking (Windows 7 TCP/IP stack → emulated NIC) to reach:

- DNS (name resolution)
- TCP (web browsing, Windows Update, etc.)
- UDP (DNS, NTP, many games/VoIP apps; eventually required)

Browser constraints:

- No raw sockets, no direct TCP/UDP to arbitrary hosts.
- Only browser transports (WebSocket, WebRTC DataChannel) and HTTP(S) are available.
- Therefore **some form of proxy** is required for real networking.

This RFC resolves where the “host-side network stack” lives:

- **A)** in the browser (WASM slirp/NAT) with a “dumb” relay proxy
- **B)** tunnel **IP packets** (L3) to a proxy that does DHCP/DNS/NAT
- **C)** tunnel **Ethernet frames** (L2) to a proxy that does bridging or a user-space stack

---

## Option A — In-browser slirp/NAT (current doc direction)

### Summary

Implement a user-mode network stack in the browser (WASM):

- ARP + DHCP server/client behavior
- IPv4 routing
- UDP and **TCP termination** (slirp-style)
- NAT + port mapping
- DNS forwarding (DoH or proxy-assisted)

Then translate guest sockets to browser-available transports:

- TCP → one WebSocket per connection (or multiplexed)
- UDP → WebRTC DataChannel (or WebSocket with framing)

### Pros

- **No per-packet tunneling over the WAN**: can translate guest TCP segments into a stream early,
  reducing ACK chatter and head-of-line effects compared to packet tunneling.
- **Unprivileged server**: proxy can be “just” a TCP/UDP relay (no TUN/TAP).
- **Potentially better guest-perceived connect latency**: slirp can SYN-ACK locally before the
  upstream connect finishes (at the cost of buffering/edge cases).

### Cons

- **Very large implementation surface in the browser**:
  implementing TCP correctly (retransmits, window scaling, SACK, PMTU discovery, corner cases)
  is a multi-month project and hard to debug.
- **CPU budget conflict**: Aero’s critical constraint is browser CPU time (emulation/JIT/WebGPU).
  A TCP/IP stack in the same process competes directly with emulation performance.
- **Feature completeness pressure**: Windows will exercise “weird” network behaviors
  (ICMP, fragmentation, DHCP renew, DNS retries, etc.).
- **Harder security story client-side**: filtering/quotas can be enforced in the proxy, but the
  browser still has to parse and synthesize complex protocol state robustly.

### Security implications

- Proxy still effectively becomes an **open egress** endpoint unless targets are restricted.
- The browser stack can be used for exfiltration (expected), but also increases attack surface
  for memory/CPU exhaustion in the emulator runtime.

---

## Option B — L3 tunnel (IP packets) to proxy; proxy does NAT + DHCP/DNS

### Summary

Browser forwards **IP packets** to a proxy server; proxy injects packets into a network stack
and returns IP packets back.

Typical implementation variants:

1) **Kernel stack via TUN** (privileged): create a TUN device per VM session and use iptables/NAT.
2) **User-space stack** (unprivileged): run a TCP/IP stack (gVisor netstack, smoltcp-based, etc.)
   and perform NAT in process.

The browser still needs to deal with the fact the guest NIC is L2:

- Either emulate a point-to-point link (custom guest driver), **or**
- Handle ARP/DHCP at the browser boundary and tunnel only IPv4 payloads.

### Pros

- **No TCP implementation in the browser**: the guest’s stack speaks TCP; the proxy handles the
  “host side”.
- Proxy can centralize **policy enforcement** (egress allowlist/denylist, rate limits, logging).
- Potentially smaller bandwidth than L2 tunneling (no Ethernet header/broadcast frames).

### Cons

- If using kernel via TUN: **requires CAP_NET_ADMIN/root-ish privileges** and host networking
  configuration (routing, NAT) that many PaaS/serverless environments disallow.
- If using user-space stack: still non-trivial implementation (but server-side is easier to iterate).
- Browser boundary still has **L2 impedance mismatch** unless a custom guest driver is introduced.

### Security implications

- Same core SSRF/open-proxy risk as Option A: the proxy can be induced to connect to internal IPs.
  Mitigations belong on the proxy side (deny RFC1918/ULA/link-local by default, per-tenant policy).

---

## Option C — L2 tunnel (Ethernet frames) to proxy; proxy provides bridge/TAP or user-space stack

### Summary

Browser forwards **raw Ethernet frames** from the emulated NIC to the proxy, and receives frames
back from the proxy.

**Wire protocol:** see [`docs/l2-tunnel-protocol.md`](./l2-tunnel-protocol.md) (versioned framing +
PING/PONG + size limits).

Proxy implementation variants:

1) **Kernel bridge via TAP** (privileged): create TAP per VM session, bridge/NAT in the host.
2) **User-space “slirp on the server”** (unprivileged): run a user-mode stack that speaks Ethernet
   and uses normal host sockets for outbound (libslirp-style).

### Pros

- **Simplest browser boundary**: the browser does not need to understand ARP/DHCP/IPv6.
  It is a pure frame forwarder: `virtio-net/e1000 ↔ tunnel ↔ proxy`.
- **Most protocol-complete** at the boundary: any L2/L3 protocol Windows emits can be carried
  without client-side special cases.
- **Proxy-side policy** remains centralized (egress controls, quotas, auditing).
- With a user-space stack, the proxy can run **unprivileged** (no TUN/TAP), which is important
  for hosted multi-tenant deployments.

### Cons

- **More bytes over the tunnel** than socket-level relaying:
  TCP ACKs, retransmits, and broadcast traffic traverse the WAN.
- **Transport choice matters**:
  - WebSocket is reliable but suffers head-of-line blocking (HOL).
  - WebRTC DataChannel can be tuned (ordering + reliability). Unordered delivery can reduce HOL, but
    Aero’s current L2 tunnel requires an **ordered** reliable DataChannel for correctness; see
    [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).
  - If using TAP/bridge: requires CAP_NET_ADMIN and increases risk of exposing a VM to the proxy’s
    L2 environment if misconfigured.

### Security implications

- A TAP/bridge design can accidentally expose the VM to a real L2 segment (ARP spoofing, scanning).
  Strong isolation is required; for hosted SaaS this is a major operational footgun.
- User-space stack avoids bridging to the host LAN entirely; VM sees only a synthetic LAN.

---

## Performance / latency expectations (qualitative)

| Axis | A) Browser slirp | B) L3 tunnel | C) L2 tunnel |
|------|------------------|--------------|--------------|
| Browser CPU | High (TCP/IP stack) | Low–Medium | Low |
| Proxy CPU | Low | Medium | Medium |
| Bandwidth overhead | Low–Medium | Medium | Highest |
| Latency sensitivity | Lower (can ACK locally) | Higher (packet RTT adds) | Higher (packet RTT adds) |
| Correctness surface | Large in browser | Large on proxy | Large on proxy |
| Operational privilege | None | Often needs TUN | Often needs TAP |

Important note: with WebRTC (unordered / partial reliability options), the practical latency
difference between packet tunneling and socket relaying is often dominated by the proxy’s geographic
distance and congestion, not protocol details.

---

## Recommendation

**Recommend Option C: L2 tunnel (Ethernet frames) to a proxy that runs an unprivileged user-space
network stack (server-side slirp/NAT).**

For the concrete wire protocol and deployment notes, see:

- [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md)
- [`l2-tunnel-runbook.md`](./l2-tunnel-runbook.md)

### Rationale

1) **Browser simplicity + emulator performance**: Aero’s performance bottleneck is the client.
   Keeping the browser as a frame forwarder avoids dedicating substantial CPU to TCP/IP.
2) **Protocol completeness at the boundary**: Windows networking is complicated. L2 tunneling
   avoids a long tail of “why does Windows send this?” client-side bugs.
3) **Avoid privileged networking in the proxy**: requiring TUN/TAP/CAP_NET_ADMIN limits deployment
   options and increases operational risk. A user-space stack can run in standard containers.
4) **Centralized security controls**: the proxy can enforce egress policy (deny RFC1918 by default,
   rate limits, per-session caps) regardless of what Windows attempts.

### Implications for follow-on work (NT-STACK / NT-WS-PROXY / NT-WRTC-PROXY)

- The browser networking implementation becomes a **transport + framing layer**:
  it forwards frames between the emulated NIC and the tunnel (WebSocket initially; WebRTC later).
- The proxy implements:
  - DHCP + DNS services for the VM subnet
  - NAT (TCP/UDP) to the public internet
  - Policy controls (allow/deny, quotas) and observability

---

## Implementation plan (repo components)

The Option C implementation is split into a small set of concrete components:

- `web/src/net/l2Tunnel.ts`
  - Browser-side tunnel client.
  - Owns the WebSocket (default) or WebRTC DataChannel (optional) and forwards raw Ethernet frames
    between the emulator and the proxy.
- `crates/aero-net-backend`
  - Emulator-side L2 tunnel backends that the NIC device model calls into, instead of running a full
    TCP/IP stack in WASM.
  - Includes:
    - queue-backed `L2TunnelBackend` (in-memory FIFO queues; useful for native test harnesses and
      non-browser hosts), and
    - ring-buffer-backed `L2TunnelRingBackend` for the browser runtime (`NET_TX`/`NET_RX` AIPC queues
      in `ioIpcSab`).
  - (Re-exported via `crates/emulator/src/io/net/{tunnel_backend.rs,l2_ring_backend.rs}` for compatibility.)
- `crates/aero-l2-proxy`
  - Unprivileged proxy service implementing a user-space Ethernet/IP stack + NAT + policy.
  - Terminates the L2 tunnel and returns frames (ARP/DHCP/DNS/etc.) back to the browser.
- `proxy/webrtc-udp-relay`
  - Optional WebRTC transport for the L2 tunnel (DataChannel carrying the L2 tunnel framing).
  - Also remains useful for standalone UDP relay use-cases during migration.

---

## Prototype in this repo

This RFC is accompanied by a minimal prototype that demonstrates the Option C shape:

**Security note:** this prototype is for experimentation only and is not hardened for production use.
For a maintained, policy-driven L2 tunnel proxy implementation, use `crates/aero-l2-proxy`
(see [`docs/l2-tunnel-runbook.md`](./l2-tunnel-runbook.md)) and treat it as a security-critical egress
surface (enforce strict policy and quotas).
For the legacy socket-level relays (Phase 0 `/tcp`, DoH, UDP datagrams), use:

- `backend/aero-gateway` for TCP + DNS (`/tcp`, `/tcp-mux`, `/dns-query`, `/dns-json`; see `docs/backend/openapi.yaml`)
- `proxy/webrtc-udp-relay` for UDP datagrams (`/webrtc/*`, `/udp`; see `proxy/webrtc-udp-relay/PROTOCOL.md`)
- `net-proxy/` can be used as a local dev relay.
- `tools/aero-gateway-rs` is an older Rust/Axum gateway prototype kept only for
  **legacy/diagnostic** purposes (historical `/tcp?target=<host>:<port>`). It is not
  production-hardened; the canonical gateway is `backend/aero-gateway`.

**Protocol note:** the prototype now uses the versioned L2 tunnel framing (including basic
PING/PONG handling) over WebSocket. Production implementations should use the maintained codec and
enforce all limits described in
[`docs/l2-tunnel-protocol.md`](./l2-tunnel-protocol.md).

- Client (“browser side”) sends:
  - ARP request (to discover gateway MAC)
  - DNS query (UDP/53)
  - TCP SYN + data to an echo server
- Proxy responds with ARP/DNS/TCP frames and forwards TCP payload to a real TCP socket.

See:

- `prototype/nt-arch-rfc/proxy-server.js`
- `prototype/nt-arch-rfc/client.js`
- `tests/networking-architecture-rfc.test.js`

---

## Current implementation status

What exists today (in repo):

- **Option C (L2 tunnel):**
  - Wire protocol: `docs/l2-tunnel-protocol.md` (`aero-l2-tunnel-v1`)
  - Browser client: `web/src/net/l2Tunnel.ts`
  - Proxy: `crates/aero-l2-proxy` (`GET /l2` WebSocket, user-space stack + NAT)
  - Optional WebRTC transport bridge: `proxy/webrtc-udp-relay` (`l2` DataChannel ↔ backend WS `/l2`, per `PROTOCOL.md`)
- **Phase 0 / migration (socket-level relays):**
  - `backend/aero-gateway` (`POST /session`, `/tcp`, `/tcp-mux`, `/dns-query`, `/dns-json`, `/udp-relay/token`)
  - `proxy/webrtc-udp-relay` UDP relay (`udp` DataChannel framing v1/v2 + WebSocket `/udp` fallback)

What Option C still requires to fully replace Phase 0:

- Making the L2 tunnel the default path in production builds (with clear fallbacks for debugging).
- Continued hardening of the proxy egress policy, observability, and resource accounting under real workloads.
