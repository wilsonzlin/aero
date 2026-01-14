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
- Native builds can also expose xHCI via `crates/devices/src/usb/xhci.rs` (`XhciPciDevice`), a PCI/MMIO
  wrapper around `aero_usb::xhci::XhciController` that enforces PCI `COMMAND` gating (`MEM`/`BME`/
  `INTX_DISABLE`) and supports MSI/MSI-X delivery (when a platform `MsiTrigger` target is provided).
- The web runtime exposes an xHCI PCI function backed by `aero_wasm::XhciControllerBridge` (wrapping
  `aero_usb::xhci::XhciController`). It implements a limited subset of xHCI (MMIO registers, USB2
  root ports + PORTSC, interrupter 0 + ERST-backed event ring delivery, deterministic snapshot/restore,
  and some host-side topology/WebUSB hooks). Endpoint-0 doorbell-driven control transfers can execute,
  and doorbell 0-driven command ring processing exists for a limited subset of commands. Bulk/interrupt
  endpoints (Normal TRBs) can also execute when configured/doorbelled, but overall xHCI coverage is
  still incomplete, so treat it as bring-up quality and incomplete.

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

SuperSpeed, isochronous transfers, streams, and other advanced features remain out of scope for the
initial xHCI MVP.

## PCI identity and wiring

The xHCI controller is exposed as a **PCI function** with a single MMIO BAR for the xHCI register space and a single interrupt.

### Where the code lives (at a glance)

Rust controller/model building blocks:

- xHCI core module: `crates/aero-usb/src/xhci/*`
  - Controller MMIO model: `crates/aero-usb/src/xhci/mod.rs` (`XhciController`)
  - Register offsets/constants: `crates/aero-usb/src/xhci/regs.rs`
  - Root hub port model + PORTSC bits: `crates/aero-usb/src/xhci/port.rs`
  - Snapshot encode/decode: `crates/aero-usb/src/xhci/snapshot.rs`
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

Native integration (wired into the PC platform behind a config flag, but not exposed by
`aero_machine::Machine` by default):

- Canonical PCI profile (QEMU xHCI identity): `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`)
- Native PCI wrapper (canonical PCI glue): `crates/devices/src/usb/xhci.rs` (`XhciPciDevice`)
- Emulator crate glue (legacy/compat; feature-gated by `emulator/legacy-usb-xhci`): `emulator::io::usb::xhci` (thin wrapper around `aero_usb::xhci`; tracked for deletion in [`docs/21-emulator-crate-migration.md`](./21-emulator-crate-migration.md))
- PC platform wiring (optional): `crates/aero-pc-platform/src/lib.rs`
  (`PcPlatformConfig.enable_xhci`, default `false`)

Notes:

- `crates/devices/src/usb/xhci.rs` is the canonical native PCI/MMIO wrapper around
  `aero_usb::xhci::XhciController` (BAR sizing, PCI `COMMAND` gating for MMIO/DMA/INTx via
  `COMMAND.MEM`/`COMMAND.BME`/`COMMAND.INTX_DISABLE`, optional MSI, and snapshot/restore).
- `aero_machine::Machine` does not yet expose xHCI by default, but the shared controller model
  (`aero_usb::xhci::XhciController`) is exercised via Rust tests, the web/WASM bridge
  (`aero_wasm::XhciControllerBridge`), and native wrappers/integrations. The PC platform
  (`crates/aero-pc-platform`) can also expose xHCI when `PcPlatformConfig.enable_xhci` is set.

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

- **Source of truth:** `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`).
- **Identity sync points** (keep consistent):
  - `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`) defines the canonical identity.
  - `crates/devices/src/usb/xhci.rs` unit tests assert `XhciPciDevice` matches the profile.
  - `web/src/io/devices/xhci.ts` mirrors the identity for the web runtime.
- The canonical PCI profile reserves a 64KiB BAR0 even though the current controller model
  implements only a subset of the architectural register set.
- Interrupt delivery is **platform-dependent**:
  - Web runtime: INTx only.
  - Native integrations may choose INTx, MSI, or MSI-X. The canonical PCI profile exposes both MSI
    and a minimal single-vector MSI-X capability (table/PBA in BAR0), and the native PCI wrapper can
    deliver interrupts via MSI/MSI-X when enabled (falling back to INTx if no `MsiTrigger` target is
    configured).
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
    - INTx IRQ level (`irq_asserted()` mirrors `XhciController::irq_level()`), and
    - deterministic snapshot/restore (controller state + tick counter, plus WebUSB device state when
      connected).
- The IRQ line observed by the guest depends on platform routing (PIRQ swizzle); see [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) and [`docs/irq-semantics.md`](./irq-semantics.md).
- `aero_machine::Machine` does not yet expose an xHCI controller by default (today it wires UHCI for
  USB). The PC platform (`crates/aero-pc-platform`) can expose xHCI behind the
  `PcPlatformConfig.enable_xhci` flag; treat the native PCI profile as the shared contract.
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
  and transfer semantics are still under active development/validation, so UHCI remains the known-good
  attachment path for HID in practice. See
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
and harnesses. Major guest-visible pieces are still missing (full command-set/state-machine coverage
and large parts of the xHCI spec), so treat the implementation as “bring-up” quality rather than a
complete xHCI.

### What exists today

#### Minimal controller MMIO surfaces

- Rust controller model: `aero_usb::xhci::XhciController`
  - 64KiB MMIO window (`XhciController::MMIO_SIZE == 0x10000`) with basic unaligned access handling.
  - Minimal MMIO register file with basic unaligned access handling:
    - Capability registers: CAPLENGTH/HCIVERSION, HCSPARAMS1 (port count), HCCPARAMS1 (xECP), DBOFF, RTSOFF.
    - A small xHCI extended capability list (xECP), including:
      - USB Legacy Support (BIOS owned cleared, OS owned set), and
      - Supported Protocol (USB 2.0 + speed IDs) sized to `port_count`.
    - Operational registers (subset): USBCMD, USBSTS, PAGESIZE, DNCTRL, CRCR, DCBAAP, CONFIG.
    - Runtime registers (subset): MFINDEX, and Interrupter 0 regs (IMAN/IMOD/ERST/ERDP).
  - DBOFF/RTSOFF report realistic offsets. The doorbell array is **partially** implemented:
    - command ring doorbell (doorbell 0) is latched; while the controller is running it triggers
      bounded command ring processing (`No-Op`, `Enable Slot`, `Disable Slot`, `Address Device`,
      `Configure Endpoint`, `Evaluate Context`, `Stop Endpoint`, `Reset Endpoint`,
      `Set TR Dequeue Pointer`; most other commands complete with TRB Error) and queues
      `Command Completion Event` TRBs.
    - device endpoint doorbells are latched and can drive a bounded endpoint-0 control transfer
      executor (Setup/Data/Status TRBs) when the controller is ticked.
    - runtime interrupter 0 registers + ERST-backed guest event ring producer are modeled (used by
      Rust tests and by the web/WASM bridge via `step_frames()`/`poll()`).
  - A DMA read on the first transition of `USBCMD.RUN` (primarily to validate **PCI Bus Master Enable gating** in wrappers).
  - A level-triggered interrupt condition surfaced as `irq_level()` (interrupter 0 interrupt enable +
    pending; USBSTS.EINT is derived from pending), used to validate **INTx disable gating**.
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
    ring work (endpoint 0 control + bulk/interrupt endpoints) and drains queued events
    (`XhciController::tick_1ms_and_service_event_ring`).
  - `poll()` drains any queued event TRBs into the guest event ring (`XhciController::service_event_ring`);
    DMA is gated on BME.
  - WebUSB passthrough device APIs (`set_connected`, `drain_actions`, `push_completion`, `reset`,
    `pending_summary`) used by the web I/O worker to attach/detach a passthrough device behind a
    reserved xHCI root port (typically root port index `1`; falls back to `0` if the controller only
    exposes a single root port).
  - `irq_asserted()` reflects `XhciController::irq_level()` (interrupter 0 interrupt enable + pending).
  - Optional host-side topology mutation APIs for passthrough HID/hubs (`attach_hub`,
    `detach_at_path`, `attach_webhid_device`, `attach_usb_hid_passthrough_device`).
  - Deterministic snapshot/restore of controller state + tick counter (and WebUSB device state when
    connected).

These are **not** full xHCI implementations. In particular, command ring coverage is still incomplete
(bounded to a subset of commands), and transfer execution is still incomplete compared to real xHCI
(no isochronous, no USB3/SuperSpeed, etc).

#### TRB + ring building blocks

`crates/aero-usb/src/xhci/` also provides:

- TRB encoding helpers (`trb`)
- TRB ring cursor/polling helpers (`ring`)
- Context parsing helpers (`context`)

These are used by tests and by higher-level “transfer engine” harnesses.

#### Command ring + endpoint-management helpers (used by tests)

`crates/aero-usb/src/xhci/` includes a few early building blocks that model **parts** of xHCI
command/event behavior:

- `XhciController::{set_command_ring,process_command_ring}`: command ring processing used by unit
  tests and by the guest-visible MMIO path (CRCR + doorbell 0). It consumes a guest command ring
  (via `RingCursor`) and queues `Command Completion Event` TRBs for a small subset of commands used
  during bring-up: `No-Op`, `Enable Slot`, `Disable Slot`, `Address Device`, `Configure Endpoint`,
  `Evaluate Context`, and endpoint commands `Stop Endpoint`, `Reset Endpoint`, `Set TR Dequeue Pointer`.
  These events are delivered to the guest only once the event ring is
  configured and `service_event_ring` is called (e.g. via the WASM bridge `step_frames()`/`poll()`
  hook).
- `command_ring::CommandRingProcessor`: parses a guest command ring and writes completion events into
  a guest event ring (single-segment).
  - Implemented commands (subset): `Enable Slot`, `Disable Slot`, `No-Op`, `Address Device`,
    `Configure Endpoint`, `Evaluate Context`, `Stop Endpoint`, `Reset Endpoint`, `Set TR Dequeue Pointer`.
- `command`: a minimal endpoint-management state machine used by tests and by early enumeration
  harnesses.

These helpers are wired into the guest-visible MMIO/doorbell model (CRCR + doorbell 0 for the command
subset, plus slot doorbells for transfer execution on endpoint 0 and bulk/interrupt endpoints), but
many commands/endpoints remain bring-up-only. The more complete
`command_ring::CommandRingProcessor` remains a test/harness helper and is not used by the MMIO
doorbell path today.

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
is enabled, so doorbelled transfers (endpoint 0 + bulk/interrupt) can make forward progress and
queued events are drained.

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
- More complete transfer scheduling/performance for bulk/interrupt endpoints (today execution is
  intentionally bounded to keep guest-induced work finite).
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
  endpoint 0 and bulk/interrupt endpoints), but the command set is incomplete and many advanced xHCI
  semantics are still missing.
- **Full xHCI slot/endpoint context state machines** (`Configure Endpoint`, `Evaluate Context`,
  endpoint commands, etc). A subset of command processing is exposed today via doorbell 0, but full
  coverage/state machines remain incomplete.
- **Non-control transfers are limited** to bulk/interrupt endpoints (Normal TRBs). Advanced transfer
  types and semantics (isochronous, USB3/SuperSpeed, streams, etc) are not implemented.
- **USB 3.x SuperSpeed** (5/10/20Gbps link speeds) and related link state machinery.
- **Isochronous transfers** (audio/video devices).
- **MSI/MSI-X in the web runtime**: the TS PCI wrapper currently uses INTx only (no MSI/MSI-X
  capabilities exposed to the guest). Native wrappers can deliver MSI/MSI-X (single-vector MSI-X)
  when configured.
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
  - Today, `aero_usb::xhci::XhciController` snapshots (device ID `XHCI`, version `0.7`) capture:
    - operational/runtime state (`USBCMD`, `USBSTS`, `CONFIG`, `MFINDEX`, `CRCR`, `DCBAAP`, port count,
      `DNCTRL`, Interrupter 0 regs: `IMAN`, `IMOD`, `ERSTSZ`, `ERSTBA`, `ERDP` + internal generation counters),
    - per-port snapshot records (connection/change bits/reset timers/link state/speed + nested
      `AttachedUsbDevice` snapshot, when present),
    - controller-local slot/endpoint state (enabled slots, Slot/Endpoint context mirrors + transfer
      ring cursors, active endpoints, EP0 control TD state), and
    - controller-local forward-progress state (command ring cursor/kick flag, pending event TRBs,
      and event ring producer state).
  - Current limitations: the snapshot only covers the subset of xHCI behavior implemented by this
    model; guest RAM contents for rings/contexts/buffers are still owned by the VM memory snapshot,
    and host-side async work (WebUSB/WebHID) is reset across restore.
- The web/WASM bridge (`aero_wasm::XhciControllerBridge`) snapshots as `XHCB` (version `1.1`) and currently stores:
  - the underlying `aero_usb::xhci::XhciController` snapshot bytes,
  - a tick counter, and
  - (when connected) the `UsbWebUsbPassthroughDevice` snapshot bytes.
- The native PCI wrapper (`crates/devices/src/usb/xhci.rs`) snapshots as `XHCP` (version `1.2`) and stores:
  - PCI config space (including MSI/MSI-X capability state and BAR bookkeeping),
  - internal IRQ bookkeeping,
  - MSI-X table + PBA state (when present), and
  - the underlying `aero_usb::xhci::XhciController` snapshot bytes.
- **Host resources are not snapshotted.** Any host-side asynchronous USB work (e.g. in-flight WebUSB/WebHID requests) must be treated as **reset** across restore; the host integration is responsible for resuming forwarding after restore.

Practical implication: restores are deterministic for pure-emulated devices, but passthrough devices may need re-authorization/re-attachment and may observe a transient disconnect.

---

## Testing

Rust-side USB/controller/device-model tests:

```bash
cargo test -p aero-usb --locked
cargo test -p aero-devices --locked
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
