# ADR 0005: Networking via L2 tunnel (Option C) to an unprivileged proxy

## Context

Aero needs guest networking (Windows 7 TCP/IP stack → emulated NIC) to reach the public internet (DNS, TCP, UDP).

Browsers cannot open raw sockets or arbitrary TCP/UDP connections, so any “real” guest networking requires a server-side proxy component.

The project evaluated three high-level approaches in [`docs/networking-architecture-rfc.md`](../networking-architecture-rfc.md):

- **Option A:** implement slirp/NAT in the browser (WASM) and use a “dumb” relay proxy
- **Option B:** tunnel **IP packets** (L3) to a proxy that runs DHCP/DNS/NAT
- **Option C:** tunnel **Ethernet frames** (L2) to a proxy that runs a user-space stack or bridges to the host

The RFC recommended **Option C** but left open several production-design questions around service boundaries, transports, reliability, and security posture. This ADR finalizes those decisions.

## Decision

### Default target architecture

**Option C (L2 tunnel) is the default target architecture for production guest networking.**

The browser runtime is a **frame forwarder**:

```
guest NIC (e1000/virtio-net)  ⇄  L2 tunnel  ⇄  proxy-side user-space stack  ⇄  host sockets (TCP/UDP)
```

This keeps client CPU usage low and avoids implementing a full TCP/IP stack in WASM.

### Proxy implementation model (unprivileged)

The proxy runs a **user-space network stack** (server-side “slirp/NAT”) and **does not require privileged kernel networking**:

- **No TUN/TAP**
- **No `CAP_NET_ADMIN`**
- No bridging to a real L2 segment

Outbound connectivity is performed via normal host sockets (connect/sendto) with NAT/policy enforced in-process.

### Where the L2 proxy lives (repo + deployment topology)

The L2 proxy is a distinct *data-plane* service, separate from the HTTP/control-plane gateway:

- **In-repo target location:** `crates/aero-l2-proxy/` (Rust service).
  - It will reuse the existing Rust packet/stack crates (e.g. `crates/aero-net-stack`, `crates/nt-packetlib`) rather than re-implementing packet parsing/state machines in Node.
- **Deployment topology:** deploy `aero-l2-proxy` alongside `backend/aero-gateway` and route both behind the same edge proxy/Ingress.
  - The edge proxy provides TLS/WSS termination, COOP/COEP headers, and path-based routing (e.g. `/tcp`, `/dns-query` → `aero-gateway`; `/l2` → `aero-l2-proxy`).

### Relationship to `backend/aero-gateway` (Node vs Rust)

`backend/aero-gateway` remains the public-facing *control-plane* service (currently implemented in Node in production deployments). The L2 tunnel proxy is **not** implemented inside the Node gateway process.

Responsibilities are split as:

- `aero-gateway` (control plane):
  - session bootstrap / auth primitives
  - origin allowlist + global request rate limiting
  - other HTTP APIs (e.g. DoH) as needed
- `aero-l2-proxy` (data plane):
  - per-VM L2 tunnel termination
  - DHCP/DNS/NAT in the proxy-side user-space stack
  - egress policy enforcement at the point where real sockets are opened

This avoids coupling the gateway’s HTTP concerns to a high-throughput packet-forwarding loop and keeps the L2 implementation in Rust (where the packet/stack code already lives).

### Transport choices (WS first; WebRTC optional)

- **Baseline transport:** **WebSocket (WSS)** for the L2 tunnel (simplest to deploy and debug).
- **Optional optimization:** **WebRTC DataChannel** may be added later to reduce head-of-line blocking and improve latency under loss.

### Reliability requirement for WebRTC L2

If/when a WebRTC transport is used for L2 tunneling:

- The DataChannel **MUST be reliable** (no frame loss).
    - (I.e. do **not** use `maxRetransmits`/`maxPacketLifeTime` / “partial reliability”.)
- `ordered: false` is recommended to reduce head-of-line blocking.

Rationale: an L2 tunnel carries TCP/UDP/IP/ARP/DHCP frames. Dropping frames breaks correctness. In
particular, when the proxy runs a user-space NAT/TCP stack (slirp-style), it may acknowledge upstream
TCP data before the guest has received it, so allowing tunnel message loss (partial reliability) can
break TCP correctness.

### Security posture (auth, origin, egress policy)

The L2 proxy is security-critical: it is a powerful egress primitive and must be treated as hostile-input-facing.

Minimum requirements for production deployments:

1. **Authentication required**
   - Establishing a tunnel must require a gateway-issued session (cookie) or an explicit token conveyed via a WebSocket-compatible mechanism (e.g. subprotocol).
2. **Origin enforcement**
   - Reject WebSocket upgrades / WebRTC signaling requests unless `Origin` is an allowed frontend origin.
3. **Egress policy enforced at the proxy**
   - Deny private / loopback / link-local / multicast / otherwise special-use destination ranges by default (IPv4 + IPv6).
   - Apply policy *after* DNS resolution to mitigate DNS rebinding.
   - Apply port policy (deny-by-default or allowlist) suitable for public deployments.
4. **Abuse controls**
   - Per-session/IP rate limits, connection limits, and bandwidth quotas (bytes in/out).
   - Observability (structured logs + metrics) sufficient to detect scanning / abuse.

## Alternatives considered

1. **Option A — in-browser slirp/NAT**
   - Pros: less WAN packet overhead; proxy can be “dumber”.
   - Cons: large and correctness-sensitive implementation in WASM; competes with emulator CPU budget; higher client attack surface.

2. **Option B — L3 tunnel (IP packets)**
   - Pros: less overhead than L2; proxy can still centralize policy.
   - Cons: still requires handling L2 impedance mismatch at the browser boundary (ARP/DHCP), or a custom guest driver; more edge cases than pure frame forwarding.

3. **Privileged proxy designs (TUN/TAP + kernel NAT/bridge)**
   - Pros: leverage mature kernel networking.
   - Cons: requires `CAP_NET_ADMIN`/TUN/TAP and careful host configuration; increases operational risk (accidental L2 exposure) and limits deployment environments.

4. **Implement the L2 proxy inside the Node `aero-gateway` process**
   - Pros: fewer services to deploy; shared auth/session code.
   - Cons: Node is not the right place for a full packet-oriented user-space network stack; we already have Rust networking crates; separating data plane vs control plane allows independent scaling and more predictable performance.

## Consequences

- **Higher bandwidth overhead:** L2 tunneling carries Ethernet/IP/TCP “chatter” (ACKs, retransmits, ARP/DHCP/broadcast) over the WAN. Expect higher baseline bandwidth than socket-level relaying.
- **WebRTC must be reliable for L2:** if WebRTC is introduced, it cannot use
  lossy/partially reliable settings.
- **Operational complexity:** running a dedicated `aero-l2-proxy` adds:
  - another service to deploy/monitor/scale
  - stateful per-VM session management (timeouts, cleanup, quotas)
  - additional security review surface (egress policy, abuse prevention)

For background and the original tradeoff analysis, see [`docs/networking-architecture-rfc.md`](../networking-architecture-rfc.md).
