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
  - Virtio-input is opt-in:
    - Native: `MachineConfig.enable_virtio_input = true` (requires `enable_pc_platform = true`).
    - JS/WASM: `api.Machine.new_with_options(..., { enable_virtio_input: true })`.
    - Canonical BDFs: `00:0A.0` (keyboard) and `00:0A.1` (mouse).
- **Browser worker runtime (production)**
  - Main thread captures browser events and batches them in `web/src/input/*`.
  - The **I/O worker** (`web/src/workers/io.worker.ts`) receives batches (`in:input-batch`) and routes them to:
    - **virtio-input** (fast path, once the guest driver sets `DRIVER_OK`)
    - **synthetic USB HID devices behind UHCI** (when enabled/available)
    - **PS/2 i8042** fallback (via the `aero-devices-input` model / equivalents)

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-machine/` | Canonical full-system VM (`aero_machine::Machine`) |
| `crates/aero-wasm/` | WASM exports (`Machine`, virtio-input core, device bridges) |
| `crates/aero-usb/` | Canonical USB stack (ADR 0015) |
| `crates/aero-devices-input/` | PS/2 controller (i8042), keyboard, mouse |
| `web/src/workers/io.worker.ts` | I/O worker routing (PS/2 vs USB HID vs virtio-input) |
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

### Canonical browser runtime input pipeline (main thread → I/O worker)

```
Browser DOM events
  → `web/src/input/*` capture + batching
  → `postMessage({ type: "in:input-batch", buffer })`
  → `web/src/workers/io.worker.ts` (route + inject)
     → virtio-input (fast path) OR USB HID (UHCI) OR PS/2 i8042 (fallback)
  → Windows 7 guest input stacks
```

Routing policy (high level):

- **Keyboard:** virtio-input (when `DRIVER_OK`) → synthetic USB keyboard (once configured) → PS/2 i8042
- **Mouse:** virtio-input (when `DRIVER_OK`) → PS/2 until the synthetic USB mouse is configured → synthetic USB mouse (once configured; or if PS/2 is unavailable)
- **Gamepad:** synthetic USB gamepad (no PS/2 fallback)

### USB HID devices behind UHCI (synthetic + passthrough)

The browser runtime can expose input as guest-visible USB HID devices in two ways:

- **Synthetic HID devices** (keyboard/mouse/gamepad) attached behind the UHCI external hub (see `web/src/usb/uhci_external_hub.ts` and the attachment logic in `web/src/workers/io.worker.ts`).
- **Physical device passthrough** via WebHID/WebUSB, bridged into UHCI (see `docs/webhid-webusb-passthrough.md`).

Guest-visible topology (UHCI external hub):

- UHCI root port 0: external hub (synthetic HID devices + WebHID passthrough)
- UHCI root port 1: reserved for WebUSB passthrough
- External hub ports:
  - ports 1..3 reserved for synthetic keyboard/mouse/gamepad
  - dynamic passthrough ports start at 4

Note: the canonical `aero_machine::Machine` does not auto-attach an external hub or synthetic HID devices; it only exposes UHCI when enabled (hosts can attach devices explicitly via `Machine.usb_attach_*`).

---

## Scancode Translation

DOM `KeyboardEvent.code` is mapped to PS/2 **Set 2** scancode bytes via a single source-of-truth table:

- `tools/gen_scancodes/scancodes.json`

Generated outputs:

- `web/src/input/scancodes.ts` (browser capture)
- `src/input/scancodes.ts` (repo-root harness)
- `crates/aero-devices-input/src/scancodes_generated.rs` (Rust/WASM)

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
- `web/src/input/event_queue.ts`: packs events into an `Int32Array`-compatible `ArrayBuffer` and sends batches to the I/O worker.

The I/O worker consumes the batches in `web/src/workers/io.worker.ts` (`type: "in:input-batch"`), decodes `InputEventType`, and injects into the active backend (PS/2, USB HID, or virtio-input).

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
cargo xtask input

# Optional: also run a small input-focused Playwright subset.
cargo xtask input --e2e

# If you're running in a constrained sandbox, consider using safe-run:
bash ./scripts/safe-run.sh cargo xtask input

# --- Manual / debugging (run pieces individually) ---

# Rust device-model tests
bash ./scripts/safe-run.sh cargo test -p aero-devices-input --locked
bash ./scripts/safe-run.sh cargo test -p aero-usb --locked

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
