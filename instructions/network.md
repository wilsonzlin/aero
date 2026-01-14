# Workstream E: Network & Proxy

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns **network emulation**: the E1000 NIC device model, virtio-net, and the external proxies that bridge guest network traffic to the real internet (TCP over WebSocket, UDP over WebRTC).

Network connectivity is important for Windows activation, updates, and general usability.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-net-e1000/` | Intel E1000 NIC emulation |
| `crates/aero-net-pump/` | Bounded per-tick NIC↔backend frame pumping glue (shared integration logic) |
| `crates/aero-net-backend/` | Minimal host-side network backend trait + L2 tunnel backends (queue + NET_TX/NET_RX ring) |
| `crates/aero-net-stack/` | User-space TCP/IP stack |
| `crates/aero-l2-protocol/` | L2 frame protocol |
| `crates/aero-l2-proxy/` | L2 tunnel proxy |
| `proxy/` | Go-based network proxies |
| `proxy/webrtc-udp-relay/` | WebRTC UDP relay |
| `net-proxy/` | Local development proxy (TCP/UDP relay + DoH) |
| `backend/aero-gateway/` | Production gateway service |

---

## Essential Documentation

**Must read:**

- [`docs/07-networking.md`](../docs/07-networking.md) — Network architecture
- [`docs/backend/01-aero-gateway-api.md`](../docs/backend/01-aero-gateway-api.md) — Gateway API spec

**Reference:**

- [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md) — UDP relay protocol
- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture

---

## Tasks

### Network Device Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| NT-001 | E1000 NIC emulation | P0 | None | Very High |
| NT-002 | Packet receive/transmit | P0 | NT-001 | Medium |
| NT-003 | User-space network stack | P0 | None | High |
| NT-004 | DHCP client | P0 | NT-003 | Medium |
| NT-005 | DNS resolution (DoH) | P1 | None | Medium |
| NT-006 | WebSocket TCP proxy | P0 | None | Medium |
| NT-007 | WebRTC UDP proxy | P1 | None | High |
| NT-008 | Virtio-net device model | P1 | VTP-001..VTP-003 | High |
| NT-009 | Network test suite | P0 | NT-001 | Medium |

---

## Network Architecture

### L2 Tunneling (Recommended)

The emulator forwards raw Ethernet frames to an external proxy that runs the TCP/IP stack:

```
┌─────────────────────────────────────────────┐
│            Windows 7 Guest                   │
│                 │                            │
│         E1000 / Virtio-net Driver           │
├─────────────────┼───────────────────────────┤
│                 ▼                            │
│        E1000 Device Model                   │
│        Virtio-net Device Model              │
│                 │                            │
│                 ▼                            │
│         L2 Frame Protocol                   │
│                 │                            │
└─────────────────┼───────────────────────────┘
                  │ WebSocket
                  ▼
┌─────────────────────────────────────────────┐
│           Aero Gateway                       │
│                 │                            │
│         L2 Proxy + TCP/IP Stack             │
│                 │                            │
│         ┌───────┴───────┐                   │
│         ▼               ▼                   │
│    TCP Proxy      UDP Relay                 │
│    (WebSocket)    (WebRTC)                  │
└─────────────────────────────────────────────┘
```

### Why L2 Tunneling?

- **Browser limitations**: Browsers cannot create raw sockets
- **Proxy handles stack**: TCP/IP stack runs in the proxy, not the browser
- **Security**: Proxy can enforce policies (rate limits, blocked destinations)

---

## E1000 Implementation Notes

Intel E1000 is a well-documented Gigabit Ethernet controller. Key components:

1. **PCI Configuration** — Standard PCI device
2. **Register Set** — MMIO BAR0
3. **TX Descriptor Ring** — Transmit packets
4. **RX Descriptor Ring** — Receive packets
5. **Interrupts** — MSI or INTx

Windows 7 has an inbox E1000 driver (`e1000325.sys`).

Reference: Intel 8254x PRM (publicly available).

---

## Proxy Architecture

### TCP Proxy (WebSocket)

```
Browser                              Gateway
   │                                    │
   │ WebSocket CONNECT                  │
   │ ──────────────────────────────────▶│
   │                                    │
   │ TCP handshake                      │──▶ Real TCP connection
   │ ◀──────────────────────────────────│
   │                                    │
   │ Bidirectional data                 │
   │ ◀─────────────────────────────────▶│
```

### UDP Relay (WebRTC)

```
Browser                              Gateway
   │                                    │
   │ WebRTC DataChannel                 │
   │ ──────────────────────────────────▶│
   │                                    │
   │ UDP frames (encapsulated)          │──▶ Real UDP packets
   │ ◀─────────────────────────────────▶│
```

Inbound filtering note: `proxy/webrtc-udp-relay` defaults to `UDP_INBOUND_FILTER_MODE=address_and_port`
(only accept inbound UDP from remote address+port tuples the guest previously sent to). You can switch
to full-cone behavior with `UDP_INBOUND_FILTER_MODE=any` (**less safe**; see the relay README).

WebRTC DataChannel DoS hardening note: the relay configures pion/SCTP limits to prevent malicious peers
from sending extremely large WebRTC DataChannel messages that would otherwise be buffered/allocated
before `DataChannel.OnMessage` runs. Relevant knobs:

- `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (SDP `a=max-message-size` hint; 0 = auto)
- `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (hard receive-side cap; 0 = auto; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
- `WEBRTC_SESSION_CONNECT_TIMEOUT` (close sessions that never reach a connected state; prevents PeerConnection leaks; default `30s`)

---

## Aero Gateway

The production gateway (`backend/aero-gateway/`) provides:

- TCP proxy endpoint (`/tcp`)
- UDP relay (WebRTC signaling)
- DoH endpoint for DNS
- Health/readiness endpoints
- Rate limiting and access control

See [`docs/backend/01-aero-gateway-api.md`](../docs/backend/01-aero-gateway-api.md) for the API spec.

---

## Local Development

For local development, use `net-proxy/`:

```bash
# From the repo root (npm workspaces)
npm ci

# Start the proxy (safe-by-default: only allows public/unicast targets)
npm -w net-proxy run dev

# Or: trusted local dev mode (allows localhost/private ranges for /tcp, /tcp-mux, /udp)
# AERO_PROXY_OPEN=1 npm -w net-proxy run dev
```

This provides a local proxy that the emulator (and the browser networking clients under `web/src/net`) can connect to for testing:

- `GET /healthz`
- `GET|POST /dns-query` and `GET /dns-json` (DNS-over-HTTPS)
- `WS /tcp`, `WS /tcp-mux`, `WS /udp`

Notes:

- DoH endpoints are normal `fetch()` calls, so browser clients generally need them to be **same-origin** (or served with
  permissive CORS). The easiest local-dev approach is proxying `/dns-query` + `/dns-json` through Vite; alternatively,
  `net-proxy` supports an explicit CORS allowlist via `AERO_PROXY_DOH_CORS_ALLOW_ORIGINS` (see `net-proxy/README.md`).
- `net-proxy` DoH endpoints are intentionally lightweight and are **unauthenticated** (no session cookie) and not
  policy-filtered by `AERO_PROXY_OPEN` / `AERO_PROXY_ALLOW` (the policy applies to `/tcp`, `/tcp-mux`, `/udp`).

See [`net-proxy/README.md`](../net-proxy/README.md) for full details (allowlist policy, URL formats, and DoH examples).

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: NIC registers accessed via `CpuBus`
- **Integration (H)**: NIC must be wired into PCI bus

### What Other Workstreams Need From You

- Working network for Windows activation/updates
- Virtio-net device model for driver development (C)

---

## Testing

```bash
# Run network tests
bash ./scripts/safe-run.sh cargo test -p aero-net-e1000 --locked
bash ./scripts/safe-run.sh cargo test -p aero-net-stack --locked
bash ./scripts/safe-run.sh cargo test -p aero-l2-proxy --locked

# Note: `aero-l2-proxy` can be slow to compile in shared/contended sandboxes.
# If `safe-run` times out, retry with a larger timeout and/or isolate Cargo state:
#   AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-l2-proxy --locked
#   AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh cargo test -p aero-l2-proxy --locked

# Run gateway tests
cd backend/aero-gateway
npm test
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/07-networking.md`](../docs/07-networking.md)
4. ☐ Explore `crates/aero-net-e1000/src/` and `proxy/`
5. ☐ Run local proxy to test connectivity
6. ☐ Pick a task from the tables above and begin

---

*Network makes the emulator useful. Without it, Windows 7 is an island.*
