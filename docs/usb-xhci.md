# USB xHCI (USB 3.x): Host controller emulation

xHCI (“eXtensible Host Controller Interface”) is the USB host controller architecture used by most modern machines. Unlike UHCI/EHCI, xHCI is designed to support USB 3.x and also subsumes USB 2.0/1.1 device support.

This repo’s USB stack historically started with a **minimal UHCI (USB 1.1)** implementation (`crates/aero-usb`) because it is sufficient for Windows 7 in-box USB + HID drivers.

EHCI (USB 2.0) is being brought up in parallel for **high-speed** device support; see
[`docs/usb-ehci.md`](./usb-ehci.md).

xHCI is being added to:

- Support **modern guests** that expect xHCI to exist (or prefer it for USB input).
- Remove full-speed-only constraints that limit **USB passthrough** compatibility (many real devices are high-speed-only or behave poorly when forced into a UHCI full-speed view).
- Provide the foundation for future **USB 3.x** support.

Status:

- xHCI support is **in progress** and is not expected to be feature-complete.
- UHCI remains the “known-good” controller for Windows 7 in-box driver binding today.
- Windows 7 does **not** include an in-box xHCI (USB 3.x) driver; xHCI is primarily targeted at
  modern guests (or Windows 7 only when an xHCI driver is installed).
- EHCI bring-up exists (regs + root hub), but schedule walking and snapshot support are still
  staged; see [`docs/usb-ehci.md`](./usb-ehci.md).
- The web runtime currently exposes an xHCI *placeholder* (a minimal MMIO register file + snapshot
  plumbing). It is not yet a functional guest-visible USB controller.

> Canonical USB stack selection: see [ADR 0015](./adr/0015-canonical-usb-stack.md) (`crates/aero-usb` + `crates/aero-wasm` + `web/`).

Related docs:

- USB HID device/report details: [`docs/usb-hid.md`](./usb-hid.md)
- EHCI (USB 2.0) controller bring-up + contract: [`docs/usb-ehci.md`](./usb-ehci.md)
- WebUSB passthrough (currently UHCI-focused, but the async “pending → NAK” pattern applies to any controller): [`docs/webusb-passthrough.md`](./webusb-passthrough.md)
- Canonical PCI layout + INTx routing: [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md)
- IRQ line semantics in the web runtime: [`docs/irq-semantics.md`](./irq-semantics.md)

---

## PCI identity and wiring

The xHCI controller is exposed as a **PCI function** with a single MMIO BAR for the xHCI register space and a single interrupt.

### Where the code lives (at a glance)

Rust controller/model building blocks:

- xHCI core module: `crates/aero-usb/src/xhci/*`
  - `XhciController` (minimal MMIO surface): `crates/aero-usb/src/xhci/mod.rs`
  - TRB helpers: `crates/aero-usb/src/xhci/trb.rs`
  - Ring helpers: `crates/aero-usb/src/xhci/ring.rs`
  - Transfer executor (Normal TRBs): `crates/aero-usb/src/xhci/transfer.rs`

Web runtime integration:

- Guest-visible PCI wrapper: `web/src/io/devices/xhci.ts` (`XhciPciDevice`)
- Worker wiring: `web/src/workers/io_xhci_init.ts` (`tryInitXhciDevice`)
- WASM bridge export: `crates/aero-wasm/src/xhci_controller_bridge.rs` (`XhciControllerBridge`)

Native integration (not yet wired into the canonical `Machine` by default):

- Canonical PCI profile (QEMU xHCI identity): `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`)
- Native PCI wrapper (IRQ/MSI plumbing): `crates/devices/src/usb/xhci.rs` (`XhciPciDevice`)
- Emulator crate glue (module path): `emulator::io::usb::xhci` (thin wrapper around `aero_usb::xhci`)

### PCI identity (native runtime)

The repo defines a stable PCI identity for xHCI in `crates/devices` so native integrations can bind
class drivers predictably.

| Field | Value |
|---|---|
| BDF | `00:0d.0` |
| Vendor ID | `0x1b36` (Red Hat / QEMU) |
| Device ID | `0x000d` |
| Class code | `0x0c/0x03/0x30` (Serial bus / USB / xHCI) |
| Interrupt | PCI INTx (INTA#) |
| BARs | BAR0 = MMIO32 (`0x10000` bytes) |

Notes:

- The canonical PCI identity is defined in `crates/devices/src/pci/profile.rs` as `USB_XHCI_QEMU`.
- The canonical PCI profile reserves a 64KiB BAR0 even though current controller stubs implement
  only a small subset of registers.
- Interrupt delivery is **platform-dependent**:
  - Web runtime: INTx only.
  - Native integrations may choose INTx or MSI (the native PCI wrapper exposes an MSI capability),
    but MSI-X is not implemented yet.
- The IRQ line observed by the guest depends on platform routing (PIRQ swizzle); see [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) and [`docs/irq-semantics.md`](./irq-semantics.md).
- `aero_machine::Machine` does not yet expose an xHCI controller by default (today it wires UHCI for
  USB). Treat the native PCI profile as an intended contract for future wiring.

### PCI identity (web runtime)

The browser/WASM runtime currently uses a different (Intel-ish) identity and BDF than the native
profile. This keeps the web runtime’s PCI layout stable for its own integration tests and allows
xHCI to be enabled independently of the native canonical PCI profiles.

| Field | Value |
|---|---|
| BDF | `00:02.0` |
| Vendor ID | `0x8086` (Intel) |
| Device ID | `0x1e31` |
| Class code | `0x0c/0x03/0x30` |
| Interrupt | PCI INTx (level-triggered) |
| BARs | BAR0 = MMIO32 (`0x10000` bytes) |

Notes:

- Web implementation: `web/src/io/devices/xhci.ts` (`XhciPciDevice`).
- Web init wiring: `web/src/workers/io_xhci_init.ts` (attempts to initialize `XhciControllerBridge` if present in the WASM build).
- WASM export: `crates/aero-wasm/src/xhci_controller_bridge.rs` (`XhciControllerBridge`). Today this
  is a **stub** register file (byte-addressed MMIO window) with a tick counter and snapshot helpers;
  `irq_asserted()` currently always returns `false`.
- The web runtime currently does **not** expose MSI/MSI-X capabilities for xHCI.

---

## Implementation status (today) vs MVP target

The current xHCI effort is intentionally staged. The long-term goal is a real xHCI host controller
for modern guests and for high-speed/superspeed passthrough, but the in-tree code today is mostly
**scaffolding** (TRB/ring helpers + small controller stubs).

### What exists today

#### Minimal controller MMIO surfaces

- Native/shared Rust: `aero_usb::xhci::XhciController`
  - Minimal MMIO register file (CAPLENGTH/HCIVERSION, HCSPARAMS/HCCPARAMS, USBCMD, USBSTS, CRCR, DCBAAP) with basic unaligned access handling.
  - A DMA read on the first transition of `USBCMD.RUN` (primarily to validate **PCI Bus Master Enable gating** in wrappers).
  - A level-triggered interrupt condition surfaced as `irq_level()` (USBSTS.EINT), used to validate **INTx disable gating**.
  - DCBAAP register storage and controller-local slot allocation (Enable Slot scaffolding).
- Web/WASM: `aero_wasm::XhciControllerBridge`
  - Byte-addressed MMIO register file (bounded, currently `0x4000` bytes).
  - `tick()` counter only (no scheduling yet).
  - Deterministic snapshot/restore of the register file + tick count.
  - No IRQs yet (`irq_asserted()` always `false`).

These are **not** full xHCI implementations (no doorbells, no event ring, no port state machine; slot/context support is partial).

#### TRB + ring building blocks

`crates/aero-usb/src/xhci/` also provides:

- TRB encoding helpers (`trb`)
- TRB ring cursor/polling helpers (`ring`)
- Context parsing helpers (`context`)

These are used by tests and by higher-level “transfer engine” harnesses.

#### Transfers (non-control endpoints via Normal TRBs)

`aero_usb::xhci::transfer::XhciTransferExecutor` can execute **Normal TRBs** for non-control endpoints:

- Interrupt IN/OUT (HID input/output reports)
- Bulk IN/OUT (primarily for passthrough/WebUSB-style flows)

Key semantics:

- `UsbInResult::Nak` / `UsbOutResult::Nak` leaves a TD pending so it can be retried on a later tick.
- Short packets generate a `ShortPacket` completion code and report *residual bytes* (xHCI semantics).
- `Stall` halts the endpoint and produces a `StallError` completion.

### Device model layer

xHCI shares the same USB device model abstractions as UHCI (`crate::UsbDeviceModel` / `device::AttachedUsbDevice`), so device work (HID descriptors, report formats, passthrough normalization) does not need to be duplicated per controller type.

#### Test-only: xHCI-style command + control transfer harness

`crates/aero-usb/tests/xhci_webusb_passthrough.rs` contains a small **xHCI-style** harness that
consumes TRBs from guest memory (via `RingCursor`) and drives the existing `AttachedUsbDevice`
control pipe:

- Command ring bring-up: `Enable Slot` → `Address Device` → `Configure Endpoint`.
- EP0 control-IN transfer built from `Setup Stage` / `Data Stage` / `Status Stage` TRBs (e.g.
  `GET_DESCRIPTOR`).
- Bulk IN/OUT via Normal TRBs for passthrough-style flows.

This harness is a reference/validation tool; it is **not** yet integrated into the guest-visible
MMIO controller stubs.

### Still MVP-relevant but not implemented yet

- Root hub + per-port register model (connect/reset/change bits, timers).
- Doorbells, command ring + event ring, interrupters, and slot/endpoint context state machines.
- Endpoint 0 control transfer engine wired into the controller (beyond the test harness).
- Wiring xHCI into the canonical machine/topology (native) and aligning PCI identity across runtimes.

---

## Unsupported features / known gaps

xHCI is a large spec. The MVP intentionally leaves out many features that guests and/or real hardware may use:

- **Root hub / port model** (connect/disconnect/reset/change events) at the xHCI level.
- **Command ring** and xHCI slot/endpoint context state machines (`Enable Slot`, `Address Device`, `Configure Endpoint`, etc).
- **Setup TRBs / full endpoint 0 control transfer engine** (control requests are handled at the USB device-model layer, but not yet via xHCI-style TRBs).
- **USB 3.x SuperSpeed** (5/10/20Gbps link speeds) and related link state machinery.
- **Isochronous transfers** (audio/video devices).
- **MSI-X** interrupt delivery. (MSI support is platform-dependent; the web runtime uses INTx only today.)
- **Bandwidth scheduling** / periodic scheduling details beyond “enough to exercise basic interrupt polling”.
- **Streams** (bulk streams), TRB chaining corner cases, and advanced endpoint state transitions.
- **Multiple interrupters**, interrupt moderation, and more complex event-ring configurations.
- **Power management** features (D3hot/D3cold, runtime PM, USB link power management) beyond the minimal bits required for driver bring-up.

If you are debugging a device/guest issue and you see the guest attempting to use one of the above features, it is likely hitting an unimplemented xHCI path.

---

## Snapshot / restore behavior

Snapshotting follows the repo’s general device snapshot conventions (see [`docs/16-snapshots.md`](./16-snapshots.md)):

- **Guest RAM** holds most of the xHCI “data plane” structures (rings, contexts, transfer buffers). These are captured by the VM memory snapshot, not duplicated inside the xHCI device snapshot.
- The xHCI device snapshot captures **guest-visible register state** and any controller bookkeeping that is not stored in guest RAM.
  - Today, `aero_usb::xhci::XhciController` snapshots a small subset of state (`USBCMD`, `USBSTS`, `CRCR`, `PORT_COUNT`, `DCBAAP`) under `IoSnapshot::DEVICE_ID = b\"XHCI\"`, version `0.2` (slot state is not snapshotted yet).
- The web/WASM bridge (`aero_wasm::XhciControllerBridge`) snapshots as `XHCB` (version `1.0`) and currently stores:
  - its in-memory register byte array, and
  - a tick counter.
- **Host resources are not snapshotted.** Any host-side asynchronous USB work (e.g. in-flight WebUSB/WebHID requests) must be treated as **reset** across restore; the host integration is responsible for resuming forwarding after restore.

Practical implication: restores are deterministic for pure-emulated devices, but passthrough devices may need re-authorization/re-attachment and may observe a transient disconnect.

---

## Testing

Rust-side USB/controller/device-model tests:

```bash
bash ./scripts/safe-run.sh cargo test -p aero-usb --locked
```

Web runtime unit tests (includes USB broker/runtime helpers, rings, and device wrappers):

```bash
npm -w web run test:unit
```

USB-related unit tests commonly live under:

- `web/src/usb/*.test.ts`
- `web/src/io/devices/xhci.ts` + `web/src/io/devices/xhci.test.ts` (xHCI PCI wrapper + INTx semantics)
- `web/src/workers/io_xhci_init.test.ts` (xHCI WASM bridge init + device registration)
- `web/src/hid/xhci_hid_topology.test.ts` (xHCI guest USB topology manager)

Rust xHCI-focused tests commonly live under:

- `crates/aero-usb/tests/xhci_trb_ring.rs`
- `crates/aero-usb/tests/xhci_context_parse.rs`
- `crates/aero-usb/tests/xhci_interrupt_in.rs`
- `crates/aero-usb/tests/xhci_webusb_passthrough.rs`

When adding or extending xHCI functionality, prefer adding focused Rust tests (for controller semantics) and/or web unit tests (for host integration and PCI wrapper behavior) alongside the implementation.
