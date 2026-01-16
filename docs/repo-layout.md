# Repo layout (canonical vs legacy/prototypes)

This repo contains multiple generations of frontend/backend work. The goal is to make it obvious where **new changes should go** and to keep CI/dev tooling from accidentally targeting the wrong thing.

For project-wide layout decisions, see: [`docs/adr/0001-repo-layout.md`](./adr/0001-repo-layout.md).

## Canonical / production paths

### Browser host app (canonical): repo root (Vite)

The **canonical** browser host app lives in:

- Repo root `index.html` + `src/`
- Vite config: `vite.harness.config.ts`

Recommended dev workflow from the repo root:

```bash
just setup
just dev
```

To run the legacy `web/` Vite app explicitly:

```bash
npm run dev:web
# or:
npm -w web run dev
```

### Rust emulator workspace: root `Cargo.toml` + `crates/`

The Rust codebase is a workspace rooted at:

- `Cargo.toml` (workspace)
- `crates/` (workspace members)

For VM/machine wiring specifically, there is exactly one canonical integration layer:
`crates/aero-machine` (`aero_machine::Machine`). See:

- [`docs/vm-crate-map.md`](./vm-crate-map.md) (what is canonical vs legacy)
- [`docs/21-emulator-crate-migration.md`](./21-emulator-crate-migration.md) (`crates/emulator` → canonical stack plan + deletion targets)

QEMU-based reference boot tests live under the workspace root `tests/` directory and are registered
under `crates/aero-boot-tests` (see [`docs/TESTING.md`](./TESTING.md)).

#### Crate naming convention (important)

Crates should use `aero-foo` **lowercase kebab-case** package names and matching `crates/aero-foo/`
directories. Note that Rust `use` paths still normalize `-` → `_` (e.g. `aero-cpu-core` is
imported as `aero_cpu_core`).

This repo still contains some older crates that are not `aero-*` prefixed (e.g. `crates/emulator`,
`crates/memory`). These remain in the workspace for now, but **new crates should follow the
convention**.

See [`docs/adr/0007-rust-crate-naming.md`](./adr/0007-rust-crate-naming.md).

#### Graphics ABI note (AeroGPU)

There are multiple GPU “protocols” in-tree from different phases of bring-up. New work intended
for the Windows 7 WDDM graphics path should target the canonical AeroGPU ABI in:

 - `drivers/aerogpu/protocol/*` (C headers, source of truth)
 - `emulator/protocol` (Rust/TypeScript mirror)
 - `crates/aero-machine/src/aerogpu.rs` (canonical machine MVP wiring for `A3A0:0001`; BAR0 regs + BAR1 VRAM/legacy decode)
 - `crates/aero-devices-gpu/` (shared device-side implementation: regs/ring/executor + portable PCI wrapper)
 - `crates/emulator/src/devices/pci/aerogpu.rs` (legacy/sandbox emulator integration surface)

Some legacy/prototype GPU ABIs have existed during bring-up and are **not** the Win7/WDDM driver
contract.

See `docs/graphics/aerogpu-protocols.md` for the full mapping.

For a repo-backed “what’s implemented vs what’s missing” checklist for the overall graphics stack,
see [`docs/graphics/status.md`](./graphics/status.md).

#### USB note (browser runtime)

The repo contains multiple generations of USB host controller work. The **canonical browser runtime** USB
stack is defined by [ADR 0015](./adr/0015-canonical-usb-stack.md):

- Rust USB device models + host controllers (UHCI/EHCI/xHCI): `crates/aero-usb`
- Host integration + passthrough broker/executor: `web/src/usb/*`

Controller design notes:

- EHCI (USB 2.0) emulation contracts: [`docs/usb-ehci.md`](./usb-ehci.md)
- xHCI (USB 3.x) emulation contracts: [`docs/usb-xhci.md`](./usb-xhci.md)

Legacy/non-canonical USB implementations (do not extend for new browser runtime work):

- Native emulator USB integration (PCI/PortIO wiring + re-exports around `crates/aero-usb`):
  `crates/emulator` (`emulator::io::usb` module)
- Legacy repo-root WebUSB demo RPC (direct `navigator.usb` operations; removed): previously lived under `src/platform/legacy/webusb_*`

### Backend services (production)

Most maintained backend work lives under:

- `backend/` (e.g. `backend/aero-gateway`)
- `services/` (deployment-oriented services)
- `proxy/` (networking relays used in production deployments, e.g. `proxy/webrtc-udp-relay`)
- `net-proxy/` (local-dev WebSocket TCP/UDP relay + DNS-over-HTTPS endpoints; run alongside `vite dev`)

### Protocol golden vectors (canonical): `protocol-vectors/`

Bytes-on-the-wire protocols that have multiple independent implementations (Go,
TypeScript, JavaScript) use **shared canonical golden vectors** under:

- `protocol-vectors/`

These vectors are consumed by conformance tests across implementations to
prevent protocol drift.

## Non-canonical / quarantined paths

### Legacy/experimental Vite app: `web/`

The `web/` directory is primarily shared runtime code + WASM build tooling, but it also contains a
Vite entrypoint (`web/index.html`) that is **not** the canonical host app.

CI and Playwright use the repo-root app, and the repo-root build serves `web/index.html` under `/web/`
for compatibility when needed.

### Legacy backend: `server/`

`server/` is a legacy Node backend (static hosting + early TCP proxy). New work should target `backend/aero-gateway`.

See: `server/LEGACY.md`.

### Legacy gateway prototype (Rust): `tools/aero-gateway-rs`

`tools/aero-gateway-rs` is a legacy Rust/Axum gateway prototype (historical `/tcp?target=...`).
It is kept for diagnostics only and is intentionally excluded from the default Rust workspace build
surface. The canonical, CI-tested gateway implementation is `backend/aero-gateway` (Node/TypeScript).

### Prototypes / PoCs

These directories are intentionally **not** production code:

- `poc/` – small proof-of-concepts (usually referenced from docs)
- `prototype/` – larger prototypes / RFC companions
  - `prototype/legacy-win7-aerogpu-1ae0/` – legacy Win7 AeroGPU prototype stack (deprecated; archived)
- `guest/` – legacy prototype tombstones
  - `windows/` – Win7 AeroGPU prototype pointer (tombstone; stub files only: README + redirecting install doc + stub INF).
    Archived sources live under `prototype/legacy-win7-aerogpu-1ae0/`.

If you add new experiments, keep them under one of these (or a clearly named `legacy/` directory) and document them with a small `README.md`.
