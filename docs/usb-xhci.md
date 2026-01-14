# USB xHCI (USB 3.x): Host controller emulation

xHCI (“eXtensible Host Controller Interface”) is the USB host controller architecture used by most modern machines. Unlike UHCI/EHCI, xHCI is designed to support USB 3.x and also subsumes USB 2.0/1.1 device support.

This repo’s USB stack historically started with a **minimal UHCI (USB 1.1)** implementation (`crates/aero-usb`) because it is sufficient for Windows 7 in-box USB + HID drivers.

EHCI (USB 2.0) is implemented for **high-speed** device support; see
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
- EHCI supports minimal async/periodic schedule walking (control/bulk + interrupt polling) and
  snapshot/restore; see [`docs/usb-ehci.md`](./usb-ehci.md) for current scope/limitations.
- The web runtime exposes an xHCI PCI function backed by `aero_wasm::XhciControllerBridge` (wrapping
  `aero_usb::xhci::XhciController`). It implements a limited subset of xHCI (MMIO registers, USB2
  root ports + PORTSC, interrupter 0 + ERST-backed event ring delivery, deterministic snapshot/restore,
  and some host-side topology/WebUSB hooks). Endpoint-0 doorbell-driven control transfers can execute,
  and doorbell 0-driven command ring processing exists for a limited subset of commands; however,
  non-control transfers and full xHCI scheduling/state-machine coverage are still incomplete, so treat
  it as bring-up quality and incomplete.

> Canonical USB stack selection: see [ADR 0015](./adr/0015-canonical-usb-stack.md) (`crates/aero-usb` + `crates/aero-wasm` + `web/`).

Related docs:

- USB HID device/report details: [`docs/usb-hid.md`](./usb-hid.md)
- EHCI (USB 2.0) controller bring-up + contract: [`docs/usb-ehci.md`](./usb-ehci.md)
- WebUSB passthrough (supports UHCI and, when available, EHCI/xHCI; the async “pending → NAK” pattern applies to any controller): [`docs/webusb-passthrough.md`](./webusb-passthrough.md)
- Canonical PCI layout + INTx routing: [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md)
- IRQ line semantics in the web runtime: [`docs/irq-semantics.md`](./irq-semantics.md)

---

## Goals and scope (MVP)

**MVP goal:** enough xHCI behavior for modern guests to enumerate USB 2.0 devices and poll interrupt
endpoints reliably (HID), with deterministic snapshot/restore and a path toward high-speed passthrough.

The intended xHCI MVP covers:

1. **PCI function identity + MMIO BAR + INTx**
2. **USB2-only root hub ports** (connect/disconnect/reset/change) and delivery of port-change events
   via the guest-visible event ring
3. **Command ring + event ring** integration sufficient for OS driver bring-up (slot enable, address
   device, configure endpoints, and a minimal subset of endpoint commands)
4. **Transfers**
   - Endpoint 0 control transfers via Setup/Data/Status TRBs
   - Interrupt + bulk endpoints via Normal TRBs
5. **Snapshot/restore**
   - Guest RAM owns rings/contexts/buffers; device snapshot captures guest-visible regs + controller
     bookkeeping required for forward progress.

SuperSpeed, isochronous transfers, MSI-X, streams, and other advanced features remain out of scope
for the initial xHCI MVP.

## PCI identity and wiring

The xHCI controller is exposed as a **PCI function** with a single MMIO BAR for the xHCI register space and a single interrupt.

### Where the code lives (at a glance)

Rust controller/model building blocks:

- xHCI core module: `crates/aero-usb/src/xhci/*`
  - Controller MMIO model: `crates/aero-usb/src/xhci/mod.rs` (`XhciController`)
  - Register offsets/constants: `crates/aero-usb/src/xhci/regs.rs`
  - Root hub port model + PORTSC bits: `crates/aero-usb/src/xhci/port.rs`
  - Interrupter 0 runtime regs (IMAN/ERST/ERDP): `crates/aero-usb/src/xhci/interrupter.rs`
  - Guest event ring producer (ERST-backed): `crates/aero-usb/src/xhci/event_ring.rs`
  - TRB helpers: `crates/aero-usb/src/xhci/trb.rs`
  - Ring helpers: `crates/aero-usb/src/xhci/ring.rs`
  - Command helpers: `crates/aero-usb/src/xhci/command_ring.rs`, `crates/aero-usb/src/xhci/command.rs`
  - Transfer helpers (Normal TRBs + EP0 control): `crates/aero-usb/src/xhci/transfer.rs`

Web runtime integration:

- Guest-visible PCI wrapper: `web/src/io/devices/xhci.ts` (`XhciPciDevice`)
- Worker wiring: `web/src/workers/io_xhci_init.ts` (`tryInitXhciDevice`)
- WASM bridge export: `crates/aero-wasm/src/xhci_controller_bridge.rs` (`XhciControllerBridge`)
- WebHID guest-topology manager (xHCI attachment path): `web/src/hid/xhci_hid_topology.ts`
  (`XhciHidTopologyManager`)

Native integration (not yet wired into the canonical `Machine` by default):

- Canonical PCI profile (QEMU xHCI identity): `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`)
- Native PCI wrapper (IRQ/MSI plumbing): `crates/devices/src/usb/xhci.rs` (`XhciPciDevice`)
- Emulator crate glue (module path): `emulator::io::usb::xhci` (thin wrapper around `aero_usb::xhci`)

Notes:

- `crates/devices/src/usb/xhci.rs` is the canonical native PCI/MMIO wrapper around
  `aero_usb::xhci::XhciController` (BAR sizing, PCI `COMMAND` gating for MMIO/DMA/INTx, optional MSI,
  and snapshot/restore).
- `aero_machine::Machine` does not yet expose xHCI by default, but the shared controller model
  (`aero_usb::xhci::XhciController`) is exercised via Rust tests, the web/WASM bridge
  (`aero_wasm::XhciControllerBridge`), and native wrappers/integrations.

### PCI identity (canonical)

The repo defines a stable PCI identity for xHCI in `crates/devices`. The web runtime mirrors the key
identity fields (BDF, VID/DID, class code, BAR sizing) so guests enumerate a consistent xHCI PCI
function across environments. (Some platform-specific details like MSI capability exposure may
differ.)

| Field | Value |
|---|---|
| BDF | `00:0d.0` |
| Vendor ID | `0x1b36` (Red Hat / QEMU) |
| Device ID | `0x000d` |
| Class code | `0x0c/0x03/0x30` (Serial bus / USB / xHCI) |
| Interrupt | PCI INTx (INTA#, level-triggered) |
| BARs | BAR0 = MMIO32 (`0x10000` bytes) |

Notes:

- The canonical PCI identity is defined in `crates/devices/src/pci/profile.rs` as `USB_XHCI_QEMU`.
- The canonical PCI profile reserves a 64KiB BAR0 even though the current controller model
  implements only a subset of the architectural register set.
- Interrupt delivery is **platform-dependent**:
  - Web runtime: INTx only.
  - Native integrations may choose INTx or MSI (the native PCI wrapper exposes an MSI capability),
    but MSI-X is not implemented yet.
- Web runtime wiring:
  - Guest-visible PCI wrapper: `web/src/io/devices/xhci.ts` (`XhciPciDevice`).
  - Worker wiring: `web/src/workers/io_xhci_init.ts` (`tryInitXhciDevice`). Prefers registering at
    `00:0d.0`, but falls back to auto-allocation if the slot is occupied.
  - WASM bridge export: `crates/aero-wasm/src/xhci_controller_bridge.rs` (`XhciControllerBridge`),
    which wraps the Rust controller model (`aero_usb::xhci::XhciController`) and exposes:
    - the full 64KiB MMIO window (`aero_usb::xhci::XhciController::MMIO_SIZE == 0x10000`, matching
      the TS BAR size `XHCI_MMIO_BAR_SIZE`),
    - MMIO reads/writes,
    - PCI command gating (DMA gated on Bus Master Enable via `set_pci_command()`),
    - a non-time-advancing poll hook (`poll()`) that drains queued event TRBs into the guest event ring,
    - INTx IRQ level (`irq_asserted()` mirrors `XhciController::irq_level()` / USBSTS.EINT), and
    - deterministic snapshot/restore (controller state + a tick counter, plus optional WebUSB
      passthrough device state).
- The IRQ line observed by the guest depends on platform routing (PIRQ swizzle); see [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) and [`docs/irq-semantics.md`](./irq-semantics.md).
- `aero_machine::Machine` does not yet expose an xHCI controller by default (today it wires UHCI for
  USB). Treat the native PCI profile as an intended contract for future wiring.
- WebHID passthrough attachment behind xHCI is managed via `XhciHidTopologyManager`
  (`web/src/hid/xhci_hid_topology.ts`) and the optional topology APIs exported by
  `XhciControllerBridge` (`attach_hub`, `detach_at_path`, `attach_webhid_device`,
  `attach_usb_hid_passthrough_device`). The I/O worker routes WebHID passthrough devices to xHCI
  when these exports are present (falling back to UHCI otherwise).
- WebUSB passthrough supports both legacy UHCI (full-speed view) and high-speed controllers. When
  the WASM build exports the WebUSB passthrough hooks on xHCI/EHCI bridges (`set_connected`,
  `drain_actions`, `push_completion`, `reset`), the I/O worker deterministically prefers xHCI (then
  EHCI) for guest-visible WebUSB passthrough and disables the UHCI-only
  `OTHER_SPEED_CONFIGURATION` descriptor translation. Otherwise it falls back to the UHCI-based
  passthrough path. As of today, xHCI remains bring-up quality: command ring coverage is incomplete
  and only endpoint 0 transfers are executed (non-control transfers are still missing), so UHCI
  remains the known-good attachment path for HID in practice. See
  [`docs/webusb-passthrough.md`](./webusb-passthrough.md).
- Synthetic USB HID devices (keyboard/mouse/gamepad/consumer-control) are still expected to attach
  behind UHCI when available (Windows 7 compatibility), with EHCI/xHCI used as a fallback for WASM
  builds that omit UHCI.
- The web runtime currently does **not** expose MSI/MSI-X capabilities for xHCI.

---

## Implementation status (today) vs MVP target

The current xHCI effort is intentionally staged. The long-term goal is a real xHCI host controller
for modern guests and for high-speed/superspeed passthrough, but the in-tree code today is mostly
**MVP scaffolding**: a minimal-but-realistic MMIO register model (USB2 ports + PORTSC, interrupter 0
runtime regs, ERST-backed event ring delivery) plus TRB/ring/command/transfer helpers used by tests
and harnesses. Major guest-visible pieces are still missing (full command-set/state-machine coverage,
non-control transfers, and full ring scheduling), so treat the implementation as “bring-up” quality
rather than a complete xHCI.

### What exists today

#### Minimal controller MMIO surfaces

- Rust controller model: `aero_usb::xhci::XhciController`
  - 64KiB MMIO window (`XhciController::MMIO_SIZE == 0x10000`) with basic unaligned access handling.
  - Minimal MMIO register file with basic unaligned access handling:
    - Capability registers: CAPLENGTH/HCIVERSION, HCSPARAMS1 (port count), HCCPARAMS1 (xECP), DBOFF, RTSOFF.
    - A small xHCI extended capability list (xECP), including:
      - USB Legacy Support (BIOS owned cleared, OS owned set), and
      - Supported Protocol (USB 2.0 + speed IDs) sized to `port_count`.
    - Operational registers (subset): USBCMD, USBSTS, CRCR, DCBAAP.
  - DBOFF/RTSOFF report realistic offsets. The doorbell array is **partially** implemented:
    - command ring doorbell (doorbell 0) is latched; while the controller is running it triggers
      bounded command ring processing (`No-Op`, `Enable Slot`, `Disable Slot`, `Address Device`) and
      queues `Command Completion Event` TRBs.
    - device endpoint doorbells are latched and can drive a bounded endpoint-0 control transfer
      executor (Setup/Data/Status TRBs) when the controller is ticked.
    - runtime interrupter 0 registers + ERST-backed guest event ring producer are modeled (used by
      Rust tests and by the web/WASM bridge via `step_frames()`/`poll()`).
  - A DMA read on the first transition of `USBCMD.RUN` (primarily to validate **PCI Bus Master Enable gating** in wrappers).
  - A level-triggered interrupt condition surfaced as `irq_level()` (USBSTS.EINT + interrupter
    pending), used to validate **INTx disable gating**.
  - DCBAAP register storage and controller-local slot allocation (Enable Slot scaffolding).
  - Partial slot / Address Device plumbing used by tests/harnesses:
    - resolves topology via Slot Context `RootHubPortNumber` + `RouteString`.
      - Route String encodes up to 5 downstream hub tiers as 4-bit port numbers (1..=15) terminated
        by 0; the least-significant nibble is closest to the device (so hex digits read root→device).
    - supports a limited Address Device command handler (Input Context parsing + EP0 `SET_ADDRESS` +
      Slot/EP0 context mirroring).
  - USB2-only root hub/port model: PORTSC operational registers + reset timer + Port Status Change
    Event TRBs (queued host-side and delivered via interrupter 0 event ring when configured).
- Web/WASM: `aero_wasm::XhciControllerBridge`
  - Wraps `XhciController` (shared Rust model) and forwards MMIO reads/writes from the TS PCI device.
  - Enforces **PCI BME DMA gating** by swapping the memory bus implementation when bus mastering is
    disabled (the controller still updates register state, but must not touch guest RAM).
  - `step_frames()` advances controller time; when BME is enabled it also executes pending transfer
    ring work (currently endpoint 0 only) and drains queued events
    (`XhciController::tick_1ms_and_service_event_ring`).
  - `poll()` drains any queued event TRBs into the guest event ring (`XhciController::service_event_ring`);
    DMA is gated on BME.
  - WebUSB passthrough hooks (`set_connected`, `drain_actions`, `push_completion`, `reset`) used by the
    web I/O worker to attach/detach a passthrough device behind a fixed xHCI root port.
  - `irq_asserted()` reflects `XhciController::irq_level()` (USBSTS.EINT / interrupter pending).
  - Optional host-side topology mutation APIs for passthrough HID/hubs (`attach_hub`,
    `detach_at_path`, `attach_webhid_device`, `attach_usb_hid_passthrough_device`).
  - Optional WebUSB passthrough device APIs (`set_connected`, `drain_actions`, `push_completion`,
    `reset`, `pending_summary`). The passthrough device is attached to a reserved xHCI root port
    (currently root port index `1`).
  - Deterministic snapshot/restore of the controller state + tick counter (+ optional WebUSB device
    state when connected).

These are **not** full xHCI implementations. In particular, command ring coverage is still incomplete
(bounded to a small subset of commands), and transfer execution for non-control endpoints (bulk/interrupt)
is not yet integrated into the guest-visible MMIO model.

#### TRB + ring building blocks

`crates/aero-usb/src/xhci/` also provides:

- TRB encoding helpers (`trb`)
- TRB ring cursor/polling helpers (`ring`)
- Context parsing helpers (`context`)

These are used by tests and by higher-level “transfer engine” harnesses.

#### Command ring + endpoint-management helpers (used by tests)

`crates/aero-usb/src/xhci/` includes a few early building blocks that model **parts** of xHCI
command/event behavior:

- `XhciController::{set_command_ring,process_command_ring}`: a command ring processor that can
  consume a guest command ring (via `RingCursor`) and queue `Command Completion Event` TRBs for a
  small subset of commands (`Enable Slot`, `Disable Slot`, `Address Device`, `No-Op`). It is used by
  unit tests and by the guest-visible MMIO path (CRCR + doorbell 0). These events are delivered to
  the guest only once the event ring is configured and `service_event_ring` is called (e.g. via the
  WASM bridge `step_frames()`/`poll()` hook).
- `command_ring::CommandRingProcessor`: parses a guest command ring and writes completion events into
  a guest event ring (single-segment).
  - Implemented commands (subset): `Enable Slot`, `Disable Slot`, `No-Op`, `Address Device`,
    `Configure Endpoint`, `Evaluate Context`, `Stop Endpoint`, `Reset Endpoint`, `Set TR Dequeue Pointer`.
- `command`: a minimal endpoint-management state machine used by tests and by early enumeration
  harnesses.

These helpers are partially wired into the guest-visible MMIO/doorbell model (CRCR + doorbell 0 for
the command subset, plus slot doorbells for endpoint 0), but many commands/endpoints remain bring-up-only.
The more complete `command_ring::CommandRingProcessor` and non-control transfer executor are not yet
wired into that MMIO model; integrations/tests still call them explicitly as part of staged bring-up.

#### Transfers (non-control endpoints via Normal TRBs)

`aero_usb::xhci::transfer::XhciTransferExecutor` can execute **Normal TRBs** for non-control endpoints:

- Interrupt IN/OUT (HID input/output reports)
- Bulk IN/OUT (primarily for passthrough/WebUSB-style flows)

Key semantics:

- `UsbInResult::Nak` / `UsbOutResult::Nak` leaves a TD pending so it can be retried on a later tick.
- Short packets generate a `ShortPacket` completion code and report *residual bytes* (xHCI semantics).
- `Stall` halts the endpoint and produces a `StallError` completion.

#### Transfers (endpoint 0 control via Setup/Data/Status TRBs)

`aero_usb::xhci::transfer::Ep0TransferEngine` can process **endpoint 0** control transfers from a
guest transfer ring:

- Setup Stage / Data Stage / Status Stage TRBs.
- IN + OUT directions.
- Data Stage supports buffer pointers (IDT=0) and immediate data (IDT=1, <=8 bytes).
- `NAK` leaves the TD pending and retries on the next `tick_1ms` (no busy loops).
- Emits Transfer Event TRBs into a simple contiguous event ring (used by unit tests).

This engine is currently a standalone transfer-plane component used by tests; `XhciController` has
its own minimal doorbell-driven endpoint-0 executor (driven by slot doorbells +
`XhciController::tick()`), so `Ep0TransferEngine` is not wired into the guest-visible MMIO model.
Note: the web/WASM bridge’s `step_frames()` path runs `tick_1ms_and_service_event_ring` when PCI BME
is enabled, so doorbelled endpoint-0 transfers can make forward progress and queued events are
drained. Non-control transfers are still not executed yet.

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
controller MMIO/doorbell model.

Dedicated EP0 unit tests also exist:

- `crates/aero-usb/tests/xhci_control_get_descriptor.rs`
- `crates/aero-usb/tests/xhci_control_set_configuration.rs`
- `crates/aero-usb/tests/xhci_control_in_nak_retry.rs`
- `crates/aero-usb/tests/xhci_control_immediate_data.rs`
- `crates/aero-usb/tests/xhci_controller_immediate_data.rs`

### Still MVP-relevant but not implemented yet

- Full root hub model (USB3 ports, additional link states, full port register/event coverage).
- Full command ring coverage via doorbell 0 (doorbell 0 is modeled, but only a subset of commands is
  implemented today) and the corresponding slot/endpoint context state machines (`Configure Endpoint`,
  `Evaluate Context`, endpoint commands, etc).
- Bulk/interrupt transfer execution (Normal TRBs) wired into the controller MMIO/doorbell model
  (today, only endpoint 0 runs).
- More complete event-ring servicing / “main loop” integration in wrappers: regularly call
  `tick_1ms_and_service_event_ring` (or equivalent) so port timers, transfers, and event delivery make
  forward progress, with DMA gated on PCI BME.
- Wiring xHCI into the canonical machine/topology (native). (PCI identity is already aligned across
  web + native via the QEMU-style `1b36:000d` profile.)

---

## Unsupported features / known gaps

xHCI is a large spec. The MVP intentionally leaves out many features that guests and/or real hardware may use:

- **Root hub / port model** beyond the current USB2-only PORTSC subset + reset timer scaffolding (no USB3 ports/link states yet).
- **Doorbell-driven command ring + transfer execution**: the doorbell array is partially implemented
  (doorbell 0 triggers a bounded subset of command ring processing; endpoint doorbells can drive
  endpoint 0), but the command set is incomplete and non-control transfers are still missing from the
  guest-visible MMIO model.
- **Full xHCI slot/endpoint context state machines** (`Configure Endpoint`, `Evaluate Context`,
  endpoint commands, etc). A subset of command processing is exposed today via doorbell 0, but full
  coverage/state machines remain incomplete.
- **Non-control transfer execution via Normal TRBs** (bulk/interrupt endpoints) integrated into the
  guest-visible controller and driven by a transfer tick loop. The Rust transfer executor is covered
  by unit tests, but it is not yet wired into the controller MMIO model (today, only endpoint 0 is
  executed).
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
  - Today, `aero_usb::xhci::XhciController` snapshots:
    - operational/runtime state (`USBCMD`, `USBSTS`, `CONFIG`, `MFINDEX`, `CRCR`, `DCBAAP`, port count,
      Interrupter 0 regs: `IMAN`, `IMOD`, `ERSTSZ`, `ERSTBA`, `ERDP`), and
    - per-port snapshot records (connection/change bits/reset timers/link state/speed + attached
      `AttachedUsbDevice` snapshot, when present),
    under `IoSnapshot::DEVICE_ID = b\"XHCI\"`, version `0.4`.
  - Current limitations: the `XHCI` snapshot does **not** yet capture pending event TRBs or xHCI
    driver state (slots/contexts beyond what is mirrored into guest memory); restores should be
    treated as “best-effort bring-up” rather than a bit-perfect resume of an in-flight xHCI driver.
- The web/WASM bridge (`aero_wasm::XhciControllerBridge`) snapshots as `XHCB` (version `1.1`) and currently stores:
  - the underlying `aero_usb::xhci::XhciController` snapshot bytes,
  - a tick counter, and
  - (when connected) the `UsbWebUsbPassthroughDevice` snapshot bytes.
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

- `crates/aero-usb/tests/xhci_controller_mmio.rs`
- `crates/aero-usb/tests/xhci_event_ring.rs`
- `crates/aero-usb/tests/xhci_trb_ring.rs`
- `crates/aero-usb/tests/xhci_context_parse.rs`
- `crates/aero-usb/tests/xhci_extcaps.rs`
- `crates/aero-usb/tests/xhci_supported_protocol.rs`
- `crates/aero-usb/tests/xhci_ports.rs`
- `crates/aero-usb/tests/xhci_interrupt_in.rs`
- `crates/aero-usb/tests/xhci_control_get_descriptor.rs`
- `crates/aero-usb/tests/xhci_control_set_configuration.rs`
- `crates/aero-usb/tests/xhci_control_in_nak_retry.rs`
- `crates/aero-usb/tests/xhci_webusb_passthrough.rs`

When adding or extending xHCI functionality, prefer adding focused Rust tests (for controller semantics) and/or web unit tests (for host integration and PCI wrapper behavior) alongside the implementation.
