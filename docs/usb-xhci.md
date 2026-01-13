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
- EHCI bring-up exists (regs + root hub), but schedule walking and snapshot support are still
  staged; see [`docs/usb-ehci.md`](./usb-ehci.md).

> Canonical USB stack selection: see [ADR 0015](./adr/0015-canonical-usb-stack.md) (`crates/aero-usb` + `crates/aero-wasm` + `web/`).

---

## PCI identity and wiring

The xHCI controller is exposed as a **PCI function** with a single MMIO BAR for the xHCI register space and a single interrupt.

### PCI identity (native runtime)

Native (`aero_machine` / `crates/devices`) uses a stable PCI identity so guests can bind class drivers predictably.

| Field | Value |
|---|---|
| BDF | `00:0d.0` |
| Vendor ID | `0x1b36` (Red Hat / QEMU) |
| Device ID | `0x000d` |
| Class code | `0x0c/0x03/0x30` (Serial bus / USB / xHCI) |
| Interrupt | PCI INTx (INTA#) |
| BARs | BAR0 = MMIO (xHCI register space) |

Notes:

- We currently target **legacy INTx** (level-triggered) instead of MSI/MSI-X (see [Unsupported features / known gaps](#unsupported-features--known-gaps)).
- The IRQ line observed by the guest depends on platform routing (PIRQ swizzle); see [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) and [`docs/irq-semantics.md`](./irq-semantics.md).

### PCI identity (web runtime)

The browser/WASM runtime exposes the same guest-facing PCI identity unless explicitly noted otherwise.

| Field | Value |
|---|---|
| BDF | `00:0d.0` |
| Vendor ID | `0x1b36` |
| Device ID | `0x000d` |
| Class code | `0x0c/0x03/0x30` |
| Interrupt | PCI INTx (level-triggered) |
| BARs | BAR0 = MMIO (xHCI register space) |

---

## Supported feature set (MVP)

The current xHCI effort is intentionally an **MVP** aimed at USB input and basic enumeration rather than full USB 3.x fidelity.

At a high level, the MVP supports:

### Root hub + ports

- A virtual root hub with **hot-plug capable ports** (connect/disconnect, reset, port status change events).
- Attaching Aero’s built-in USB device models (HID keyboard/mouse/gamepad, hubs) behind xHCI.

### Command + event plumbing (rings)

- Command ring bring-up sufficient for basic enumeration flows, including the core “make a device usable” commands:
  - Enable Slot
  - Address Device
  - Configure Endpoint
- Event ring + interrupter 0, with interrupts raised when events are pending (INTx). Event delivery is expected to include:
  - command completion events
  - transfer events
  - port status change events

### Transfer types

- **Control transfers** via endpoint 0 (SETUP/DATA/STATUS stages).
  - Standard requests needed for enumeration (GET_DESCRIPTOR, SET_ADDRESS, SET_CONFIGURATION, etc.).
- **Interrupt endpoints** (primarily interrupt IN for HID input reports).

### Device model layer

- Reuses the same high-level USB device model abstractions as UHCI (`crates/aero-usb`), so device work (HID descriptors, report formats, passthrough normalization) does not need to be duplicated per controller type.

---

## Unsupported features / known gaps

xHCI is a large spec. The MVP intentionally leaves out many features that guests and/or real hardware may use:

- **USB 3.x SuperSpeed** (5/10/20Gbps link speeds) and related link state machinery.
- **Isochronous transfers** (audio/video devices).
- **Bulk endpoints** (mass storage, many USB bridges) and advanced bulk scheduling semantics.
- **MSI/MSI-X** interrupt delivery (currently INTx only).
- **Bandwidth scheduling** / periodic scheduling details beyond “enough for HID interrupt polling”.
- **Streams** (bulk streams), TRB chaining corner cases, and advanced endpoint state transitions.
- **Multiple interrupters**, interrupt moderation, and more complex event-ring configurations.
- **Power management** features (D3hot/D3cold, runtime PM, USB link power management) beyond the minimal bits required for driver bring-up.

If you are debugging a device/guest issue and you see the guest attempting to use one of the above features, it is likely hitting an unimplemented xHCI path.

---

## Snapshot / restore behavior

Snapshotting follows the repo’s general device snapshot conventions (see [`docs/16-snapshots.md`](./16-snapshots.md)):

- **Guest RAM** holds most of the xHCI “data plane” structures:
  - command ring, transfer rings, event ring segments / ERST,
  - DCBAA + device contexts, input contexts, scratchpad buffers, etc.
  These are captured by the VM memory snapshot, not duplicated inside the xHCI device snapshot.
- The xHCI device snapshot captures **guest-visible register state** and **controller bookkeeping** that is *not* stored in guest RAM (plus the attached USB topology, same as UHCI).
- **Host resources are not snapshotted.** In particular, any host-side asynchronous USB work (e.g. WebUSB/WebHID requests in flight) must be treated as **reset** across restore; the host integration should re-establish device handles and resume forwarding after restore.

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

When adding or extending xHCI functionality, prefer adding focused Rust tests (for controller semantics) and/or web unit tests (for host integration and PCI wrapper behavior) alongside the implementation.
