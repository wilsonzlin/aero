# Workstream F: USB & Input

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

This workstream owns **input emulation** and **USB passthrough**: PS/2 keyboard/mouse, USB HID devices, UHCI/EHCI controllers, and browser-to-guest input event forwarding.

Input is essential for usability. Without keyboard/mouse, the emulator is unusable.

### Two runtime shapes (important)

Aero supports input through two different integration styles:

- **Canonical full-system VM (`aero_machine::Machine`, exported to JS as `crates/aero-wasm::Machine`)**
  - One object owns CPU + devices.
  - Used by native tests and by the JS/WASM “single machine” API.
  - Input is injected directly via `Machine.inject_*`:
    - PS/2 (i8042): `inject_browser_key`, `inject_mouse_motion`, etc.
    - virtio-input (optional): `inject_virtio_key/rel/button/wheel` once enabled.
    - synthetic USB HID devices (keyboard/mouse/gamepad/consumer-control): `inject_usb_hid_*` (enabled by default for `new api.Machine(...)` in the WASM wrapper; native config is opt-in via `MachineConfig.enable_synthetic_usb_hid`).
  - Virtio-input is opt-in:
    - Native: `MachineConfig.enable_virtio_input = true` (requires `enable_pc_platform = true`).
    - JS/WASM: `api.Machine.new_with_options(..., { enable_virtio_input: true })`.
    - Canonical BDFs: `00:0A.0` (keyboard) and `00:0A.1` (mouse).
- **Browser worker runtime (production)**
  - Main thread captures browser events and batches them in `web/src/input/*`.
  - The worker that injects input depends on `vmRuntime`:
    - `vmRuntime=legacy`: the **I/O worker** (`web/src/workers/io.worker.ts`) owns guest device models and routes input to:
      - **virtio-input** (fast path, once the guest driver sets `DRIVER_OK`)
      - **synthetic USB HID devices behind the guest-visible USB controller** (when enabled/available; UHCI by default, with EHCI/xHCI fallbacks in some WASM builds)
      - **PS/2 i8042** fallback (via the `aero-devices-input` model / equivalents)
    - `vmRuntime=machine`: the **machine CPU worker** (`web/src/workers/machine_cpu.worker.ts`) owns the canonical `api.Machine` instance and injects input directly (including backend selection/routing). The I/O worker runs in host-only stub mode and does not own guest input devices.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-machine/` | Canonical full-system VM (`aero_machine::Machine`) |
| `crates/aero-wasm/` | WASM exports (`Machine`, virtio-input core, device bridges) |
| `crates/aero-usb/` | Canonical USB stack (ADR 0015) |
| `crates/aero-devices-input/` | PS/2 controller (i8042), keyboard, mouse |
| `web/src/workers/io.worker.ts` | I/O worker routing (PS/2 vs USB HID vs virtio-input) |
| `web/src/workers/machine_cpu.worker.ts` | Machine runtime input injection/routing (PS/2 vs USB HID vs virtio-input) |
| `web/src/io/devices/` | Browser-side device models (i8042, UHCI, virtio-input, …) |
| `web/src/usb/` | TypeScript USB broker and passthrough |
| `web/src/input/` | Browser input event capture |

---

## Essential Documentation

**Must read:**

- [`docs/08-input-devices.md`](../docs/08-input-devices.md) — Input architecture
- [`docs/usb-hid.md`](../docs/usb-hid.md) — USB HID usages and reports
- [`docs/usb-ehci.md`](../docs/usb-ehci.md) — EHCI (USB 2.0) emulation contracts (regs, root hub, IRQ, snapshot plan)
- [`docs/adr/0015-canonical-usb-stack.md`](../docs/adr/0015-canonical-usb-stack.md) — USB stack design

**Reference:**

- [`docs/webhid-webusb-passthrough.md`](../docs/webhid-webusb-passthrough.md) — Passthrough architecture
- [`docs/webhid-hid-report-descriptor-synthesis.md`](../docs/webhid-hid-report-descriptor-synthesis.md) — HID descriptor synthesis
- [`docs/webusb-passthrough.md`](../docs/webusb-passthrough.md) — WebUSB passthrough
- [`docs/usb-xhci.md`](../docs/usb-xhci.md) — xHCI (USB 3.x) controller emulation (in progress)
- [`docs/usb-hid-gamepad.md`](../docs/usb-hid-gamepad.md) — Gamepad support

---

## Tasks

### Input Device Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| IN-001 | PS/2 controller (i8042) | P0 | None | Medium |
| IN-002 | PS/2 keyboard | P0 | IN-001 | Medium |
| IN-003 | PS/2 mouse | P0 | IN-001 | Medium |
| IN-004 | Scancode translation | P0 | None | Medium |
| IN-005 | Browser event capture | P0 | None | Medium |
| IN-006 | Pointer Lock integration | P0 | IN-005 | Low |
| IN-007 | USB HID (keyboard) | P2 | None | Medium |
| IN-008 | USB HID (mouse) | P2 | None | Medium |
| IN-009 | Gamepad support | P2 | None | Medium |
| IN-010 | Input test suite | P0 | IN-001..IN-003 | Medium |
| IN-011 | Virtio-input device model | P1 | DM-008, VTP-002 | High |
| IN-012 | Windows 7 virtio-input driver | P1 | VIO-001..VIO-003 | Very High |
| IN-013 | HID report descriptor + mapping | P1 | IN-012 | High |
| IN-014 | Driver packaging/signing | P1 | IN-012, IN-013 | Medium |
| IN-015 | Browser → virtio-input events | P1 | IN-005, IN-011 | Medium |
| IN-016 | Virtio-input test plan | P1 | IN-011..IN-015 | Medium |

---

## Input Architecture

### Canonical browser runtime input pipeline (main thread → input injector worker)

```
Browser DOM events
  → `web/src/input/*` capture + batching
  → `postMessage({ type: "in:input-batch", buffer })`
  → input injector worker:
     - `vmRuntime=legacy`: `web/src/workers/io.worker.ts`
     - `vmRuntime=machine`: `web/src/workers/machine_cpu.worker.ts`
     (route + inject)
     → virtio-input (fast path) OR USB HID (guest-visible USB controller; UHCI by default) OR PS/2 i8042 (fallback)
  → Windows 7 guest input stacks
```

Routing policy (high level):

- **Keyboard:** virtio-input (when `DRIVER_OK`) → synthetic USB keyboard (once configured) → PS/2 i8042
- **Mouse:** virtio-input (when `DRIVER_OK`) → PS/2 until the synthetic USB mouse is configured → synthetic USB mouse (once configured; or if PS/2 is unavailable)
- **Gamepad:** synthetic USB gamepad (no PS/2 fallback)

### USB HID devices behind the external hub (synthetic + passthrough)

The browser runtime can expose input as guest-visible USB HID devices in two ways:

- **Synthetic HID devices** (keyboard/mouse/gamepad/consumer-control) attached behind the external hub on root port 0 (see `web/src/usb/uhci_external_hub.ts` and the attachment logic in `web/src/workers/io.worker.ts`).
- **Physical device passthrough** via WebHID/WebUSB, bridged into the guest-visible USB controller topology (see `docs/webhid-webusb-passthrough.md`).

Guest-visible topology (external hub on root port 0):

- root port 0: external hub (synthetic HID devices + WebHID passthrough)
- root port 1: reserved for WebUSB passthrough
- External hub ports:
  - ports 1..4 reserved for synthetic keyboard/mouse/gamepad/consumer-control
  - dynamic passthrough ports start at 5 (`UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT`)
- Note: when the external hub is hosted behind xHCI, hub port numbers must be <= **15** (xHCI Slot
  Context Route String encodes downstream hub ports as 4-bit values). The web runtime clamps hub port
  counts accordingly so the topology remains representable under xHCI.

Note: the canonical `aero_machine::Machine` only auto-attaches the external hub + synthetic HID
devices when `MachineConfig.enable_synthetic_usb_hid = true` (or via the WASM wrapper helper
`Machine.new_with_input_backends(..., enableSyntheticUsbHid=true)`). Enabling
`MachineConfig.enable_uhci` by itself only attaches the UHCI controller; hosts can still attach
device models explicitly via:
 - UHCI: `Machine.usb_attach_*` (when `MachineConfig.enable_uhci` is enabled)
 - EHCI: `Machine.usb_ehci_attach_*` (when `MachineConfig.enable_ehci` is enabled)
 - xHCI: `Machine.usb_xhci_attach_*` (when `MachineConfig.enable_xhci` is enabled)

---

## Scancode Translation

DOM `KeyboardEvent.code` is mapped to PS/2 **Set 2** scancode bytes via a single source-of-truth table:

- `tools/gen_scancodes/scancodes.json`

Generated outputs:

- `scancodes.ts` in `web/src/input/` (browser capture)
- `crates/aero-devices-input/src/scancodes_generated.rs` (Rust/WASM)

Native Rust harnesses consume the mapping via `aero-devices-input` (there is no longer a separate generated harness copy).

In the web runtime, capture uses `ps2Set2ScancodeForCode` from `web/src/input/scancode.ts`. On the Rust side, the canonical helper is `aero_devices_input::scancode::browser_code_to_set2_bytes`.

---

## USB Stack (aero-usb)

Per ADR 0015, `crates/aero-usb` is the **canonical USB stack**. It provides:

- UHCI controller emulation (USB 1.1, full/low-speed)
- EHCI bring-up (USB 2.0, high-speed; regs + root hub + minimal async/periodic schedule engines)
- xHCI bring-up (USB 3.x; in progress)
- USB device enumeration
- HID class driver
- Passthrough bridge for WebUSB/WebHID

```rust
// Simplified USB stack interface
pub trait UsbController {
    fn attach_device(&mut self, device: Box<dyn UsbDevice>);
    fn detach_device(&mut self, port: u8);
    fn process_frame(&mut self);
}

pub trait UsbDevice {
    fn handle_control(&mut self, setup: SetupPacket) -> ControlResult;
    fn handle_bulk_in(&mut self, endpoint: u8) -> BulkResult;
    fn handle_bulk_out(&mut self, endpoint: u8, data: &[u8]) -> BulkResult;
}
```

---

## Browser Event Capture

The canonical browser capture implementation lives in `web/src/input/`:

- `web/src/input/input_capture.ts`: installs listeners (focus/blur, Pointer Lock, keyboard/mouse/wheel, optional Gamepad polling) and flushes input at a fixed rate (default 125Hz).
- `web/src/input/event_queue.ts`: packs events into an `Int32Array`-compatible `ArrayBuffer` and sends batches to the active input injector worker (`vmRuntime=legacy`: I/O worker; `vmRuntime=machine`: machine CPU worker).

The input injector worker (`io.worker.ts` in `vmRuntime=legacy`, `machine_cpu.worker.ts` in `vmRuntime=machine`) consumes the batches (`type: "in:input-batch"`), decodes `InputEventType`, and injects into the active backend (PS/2, USB HID, or virtio-input).

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: i8042 registers accessed via `CpuBus::io_read/io_write`
- **Integration (H)**: USB controllers wired into PCI bus

### What Other Workstreams Need From You

- Working keyboard/mouse for all other testing
- USB HID for more complex input scenarios

---

## Testing

```bash
# Run the USB/input-focused test suite (Rust + targeted web unit tests).
# (Assumes Node deps are installed; run `npm ci` from repo root if needed.)
# If your Node workspace entrypoint is `web/` (instead of the repo root), use:
#   cargo xtask input --node-dir web
#   cargo xtask input --web-dir web
# Note: by default this runs a focused subset of `aero-usb` tests (UHCI + external hub + EHCI +
# EHCI snapshot roundtrip + USB2 companion routing + WebUSB passthrough (UHCI + xHCI) + key HID
# snapshot compatibility/clamping tests + shared HID usage fixtures + xHCI bring-up smoke/reg-gating).
# Use `--usb-all` if you want to run the full `aero-usb` integration suite (all xHCI tests, etc).
cargo xtask input

# If you run Node tooling from `web/` directly (or your `node_modules/` live under `web/`),
# you can point `cargo xtask input` at that workspace:
cargo xtask input --node-dir web
# (Equivalent env var forms.)
AERO_NODE_DIR=web cargo xtask input
AERO_WEB_DIR=web cargo xtask input

# Run only the Rust USB/input tests (skips Node + Playwright; does not require `node_modules`).
cargo xtask input --rust-only

# Run the full USB stack test suite (all `aero-usb` integration tests; can be slow).
cargo xtask input --usb-all

# Also run the canonical machine integration tests (snapshot + USB container wiring).
cargo xtask input --machine

# Also run the targeted WASM USB/input regression tests (runs in Node; does not require `node_modules`).
# Note: `cargo xtask input --wasm` enforces the Node.js *major* version from `.nvmrc` (CI baseline).
# If you need to bypass the check (unsupported Node; e.g. sandbox), you can run:
#   AERO_ALLOW_UNSUPPORTED_NODE=1 cargo xtask input --wasm --rust-only
# but expect wasm-pack tooling to be flaky/hang in unsupported Node releases.
cargo xtask input --wasm --rust-only

# Also run the aero-wasm input integration smoke tests (public Machine API surface + backend wiring).
# This is a host-side Rust test suite (does not use wasm-pack) and can be run without `node_modules`.
cargo xtask input --rust-only --with-wasm

# Targeted WASM USB/input regression tests (run in Node).
wasm-pack test --node crates/aero-wasm --test webusb_uhci_bridge --locked
wasm-pack test --node crates/aero-wasm --test uhci_controller_topology --locked
wasm-pack test --node crates/aero-wasm --test uhci_runtime_webusb --locked
wasm-pack test --node crates/aero-wasm --test uhci_runtime_webusb_drain_actions --locked
wasm-pack test --node crates/aero-wasm --test uhci_runtime_topology --locked
wasm-pack test --node crates/aero-wasm --test uhci_runtime_external_hub --locked
wasm-pack test --node crates/aero-wasm --test uhci_runtime_snapshot_roundtrip --locked
wasm-pack test --node crates/aero-wasm --test ehci_controller_bridge_snapshot_roundtrip --locked
wasm-pack test --node crates/aero-wasm --test ehci_controller_topology --locked
wasm-pack test --node crates/aero-wasm --test webusb_ehci_passthrough_harness --locked
wasm-pack test --node crates/aero-wasm --test xhci_webusb_bridge --locked
wasm-pack test --node crates/aero-wasm --test xhci_controller_bridge --locked
wasm-pack test --node crates/aero-wasm --test xhci_controller_bridge_topology --locked
wasm-pack test --node crates/aero-wasm --test xhci_controller_bridge_webusb --locked
wasm-pack test --node crates/aero-wasm --test xhci_controller_topology --locked
wasm-pack test --node crates/aero-wasm --test xhci_bme_event_ring --locked
wasm-pack test --node crates/aero-wasm --test xhci_webusb_snapshot --locked
wasm-pack test --node crates/aero-wasm --test usb_bridge_snapshot_roundtrip --locked
wasm-pack test --node crates/aero-wasm --test usb_snapshot --locked
wasm-pack test --node crates/aero-wasm --test machine_input_injection_wasm --locked
wasm-pack test --node crates/aero-wasm --test wasm_machine_ps2_mouse --locked
wasm-pack test --node crates/aero-wasm --test usb_hid_bridge_keyboard_reports_wasm --locked
wasm-pack test --node crates/aero-wasm --test usb_hid_bridge_mouse_reports_wasm --locked
wasm-pack test --node crates/aero-wasm --test usb_hid_bridge_consumer_reports_wasm --locked
wasm-pack test --node crates/aero-wasm --test webhid_interrupt_out_policy_wasm --locked
wasm-pack test --node crates/aero-wasm --test webhid_report_descriptor_synthesis_wasm --locked

# Note: `wasm-pack test` currently builds *all* `crates/aero-wasm` integration tests, even if you
# pass `--test ...`. This means compile errors in unrelated WASM tests (e.g. other bridge tests)
# can still break this command.

# Canonical machine library tests (covers snapshot + USB container wiring).
cargo test -p aero-machine --lib --locked

# Canonical USB stack tests (catches UHCI/EHCI/xHCI regressions).
cargo test -p aero-usb --locked

# Lint: CI treats clippy warnings as errors (`-D warnings`), including in tests.
# If you're iterating on USB/input code, running these focused checks locally can save time:
cargo clippy -p aero-usb --tests --locked -- -D warnings
cargo clippy -p aero-devices-input --tests --locked -- -D warnings

# xHCI gotcha: transfer-ring execution is gated on `USBCMD.RUN`. If you're writing a unit test that
# rings xHCI doorbells and expects DMA to occur, make sure to set RUN first (see existing xHCI tests
# for the `ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN))` pattern).

# Optional: also run a small input-focused Playwright subset.
# (Defaults to Chromium + 1 worker; sets `AERO_WASM_PACKAGES=core` unless already configured.)
cargo xtask input --e2e

# If you're running in a constrained sandbox, consider using safe-run:
bash ./scripts/safe-run.sh cargo xtask input
# If your Node workspace is `web/`, you can also use:
AERO_NODE_DIR=web bash ./scripts/safe-run.sh cargo xtask input
# (Or: AERO_WEB_DIR=web)
bash ./scripts/safe-run.sh cargo xtask input --rust-only
bash ./scripts/safe-run.sh wasm-pack test --node crates/aero-wasm \
  --test webusb_uhci_bridge \
  --test uhci_controller_topology \
  --test uhci_runtime_webusb \
  --test uhci_runtime_webusb_drain_actions \
  --test uhci_runtime_topology \
  --test uhci_runtime_external_hub \
  --test uhci_runtime_snapshot_roundtrip \
  --test ehci_controller_bridge_snapshot_roundtrip \
  --test ehci_controller_topology \
  --test webusb_ehci_passthrough_harness \
  --test xhci_webusb_bridge \
  --test xhci_controller_bridge \
  --test xhci_controller_bridge_topology \
  --test xhci_controller_bridge_webusb \
  --test xhci_controller_topology \
  --test xhci_bme_event_ring \
  --test xhci_webusb_snapshot \
  --test usb_bridge_snapshot_roundtrip \
  --test usb_snapshot \
  --test machine_input_injection_wasm \
  --test wasm_machine_ps2_mouse \
  --test usb_hid_bridge_keyboard_reports_wasm \
  --test usb_hid_bridge_mouse_reports_wasm \
  --test usb_hid_bridge_consumer_reports_wasm \
  --test webhid_interrupt_out_policy_wasm \
  --test webhid_report_descriptor_synthesis_wasm \
  --locked

# Note: `safe-run.sh` defaults to a 10-minute timeout (`AERO_TIMEOUT=600`). On a cold build,
# `cargo xtask input` can exceed this, and `wasm-pack test` can be substantially slower (it may
# rebuild many targets even if you pass `--test ...`). Bump the timeout if you see a timeout kill:
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo xtask input --rust-only
# For wasm-pack, 20 minutes is sometimes still not enough on a very cold build.
AERO_TIMEOUT=2400 bash ./scripts/safe-run.sh wasm-pack test --node crates/aero-wasm \
  --test webusb_uhci_bridge \
  --test uhci_controller_topology \
  --test uhci_runtime_webusb \
  --test uhci_runtime_webusb_drain_actions \
  --test uhci_runtime_topology \
  --test uhci_runtime_external_hub \
  --test uhci_runtime_snapshot_roundtrip \
  --test ehci_controller_bridge_snapshot_roundtrip \
  --test ehci_controller_topology \
  --test webusb_ehci_passthrough_harness \
  --test xhci_webusb_bridge \
  --test xhci_controller_bridge \
  --test xhci_controller_bridge_topology \
  --test xhci_controller_bridge_webusb \
  --test xhci_controller_topology \
  --test xhci_bme_event_ring \
  --test xhci_webusb_snapshot \
  --test usb_bridge_snapshot_roundtrip \
  --test usb_snapshot \
  --test machine_input_injection_wasm \
  --test wasm_machine_ps2_mouse \
  --test usb_hid_bridge_keyboard_reports_wasm \
  --test usb_hid_bridge_mouse_reports_wasm \
  --test usb_hid_bridge_consumer_reports_wasm \
  --test webhid_interrupt_out_policy_wasm \
  --test webhid_report_descriptor_synthesis_wasm \
  --locked

# You can also limit web wasm-pack builds to the core runtime package (useful for Playwright E2E):
AERO_WASM_PACKAGES=core npm -w web run wasm:build

# --- Manual / debugging (run pieces individually) ---

# Rust device-model tests
bash ./scripts/safe-run.sh cargo test -p aero-devices-input --locked
# Fast focused subset (matches `cargo xtask input` default; see `cargo xtask input --help` for the canonical list):
bash ./scripts/safe-run.sh cargo test -p aero-usb --locked \
  --test uhci \
  --test uhci_external_hub \
  --test ehci \
  --test ehci_ports \
  --test ehci_snapshot_roundtrip \
  --test usb2_companion_routing \
  --test usb2_port_mux_remote_wakeup \
  --test hid_remote_wakeup \
  --test webusb_passthrough_uhci \
  --test hid_builtin_snapshot \
  --test hid_composite_mouse_snapshot_compat \
  --test hid_configuration_snapshot_clamping \
  --test hid_consumer_control_snapshot_clamping \
  --test hid_gamepad_snapshot_clamping \
  --test hid_keyboard_snapshot_sanitization \
  --test hid_mouse_snapshot_clamping \
  --test usb_hub_snapshot_configuration_clamping \
  --test attached_device_snapshot_address_clamping \
  --test hid_usage_keyboard_fixture \
  --test hid_usage_consumer_fixture \
  --test xhci_enum_smoke \
  --test xhci_port_remote_wakeup \
  --test xhci_controller_webusb_ep0 \
  --test xhci_doorbell0 \
  --test xhci_stop_endpoint_unschedules \
  --test xhci_usbcmd_run_gates_transfers \
  --test xhci_webusb_passthrough
# Full USB suite:
bash ./scripts/safe-run.sh cargo test -p aero-usb --locked

# WASM integration sanity (routes input through the same public WASM APIs used by the web runtime).
bash ./scripts/safe-run.sh cargo test -p aero-wasm --locked --test machine_input_injection --test machine_input_backends --test machine_defaults_usb_hid --test webhid_report_descriptor_synthesis --test machine_virtio_input

# Web unit tests (full suite)
npm -w web run test:unit

# Playwright E2E suite (repo root)
npm run test:e2e
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/08-input-devices.md`](../docs/08-input-devices.md)
4. ☐ Read [`docs/adr/0015-canonical-usb-stack.md`](../docs/adr/0015-canonical-usb-stack.md)
5. ☐ Explore `crates/aero-devices-input/src/` and `crates/aero-usb/src/`
6. ☐ Run existing tests to establish baseline
7. ☐ Pick a task from the tables above and begin

---

*Input makes the emulator interactive. Without it, you're just watching a movie.*
