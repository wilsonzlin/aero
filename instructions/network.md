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
| `crates/aero-net-stack/` | User-space TCP/IP stack |
| `crates/aero-l2-protocol/` | L2 frame protocol |
| `crates/aero-l2-proxy/` | L2 tunnel proxy |
| `proxy/` | Go-based network proxies |
| `proxy/webrtc-udp-relay/` | WebRTC UDP relay |
| `net-proxy/` | Local development proxy |
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
cd net-proxy
npm install
npm start
```

This provides a local proxy that the emulator can connect to for testing.

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
