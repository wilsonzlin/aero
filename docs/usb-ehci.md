# EHCI (USB 2.0) host controller emulation (Aero)

This document is a **design + implementation contract** for Aero’s EHCI (Enhanced Host Controller
Interface) model: what we emulate, what we intentionally omit in the first version, and how the
controller integrates with Aero’s runtime (IRQs, timers, snapshots, and host passthrough).

It is written to be “spec-adjacent”: it calls out the EHCI concepts and register fields the guest
driver depends on, but it does **not** attempt to restate the entire EHCI specification.

> Source of truth for USB stack ownership: [ADR 0015](./adr/0015-canonical-usb-stack.md) (canonical
> USB stack is `crates/aero-usb` + `crates/aero-wasm` + `web/` host integration).

Related docs:

- USB HID devices/report formats: [`docs/usb-hid.md`](./usb-hid.md)
- USB xHCI (USB 3.x) controller emulation: [`docs/usb-xhci.md`](./usb-xhci.md)
- IRQ line-level semantics in the browser runtime: [`docs/irq-semantics.md`](./irq-semantics.md)
- WebUSB passthrough architecture (currently UHCI-focused, but the async “pending → NAK” pattern
  applies to EHCI too): [`docs/webusb-passthrough.md`](./webusb-passthrough.md)

---

## Goals and scope (MVP)

EHCI is the USB 2.0 host controller interface used by Windows 7’s in-box `usbehci.sys` driver. In a
PC, EHCI typically co-exists with USB 1.1 companion controllers (UHCI/OHCI) that service full-speed
and low-speed traffic for the same physical ports (see [Companion controllers](#companion-controllers-configflag--port_owner)).

**MVP goal:** enough EHCI behavior for Windows to enumerate high-speed devices and poll interrupt
endpoints reliably, with deterministic snapshot/restore.

### Implementation status (today) vs MVP target

The EHCI bring-up work in-tree is intentionally staged.

What exists today (bring-up stage):

- Capability + operational MMIO registers are implemented (including W1C status masking).
- EHCI extended capability: **USB Legacy Support** (`HCCPARAMS.EECP` → `USBLEGSUP` / `USBLEGCTLSTS`)
  is implemented for the “BIOS handoff” semaphore flow.
- Root hub ports are implemented with **deterministic timers** (reset/resume) and change-bit latching.
- `FRINDEX` advances in 1ms ticks (adds 8 microframes per tick) when the controller is running.
- IRQ line level is derived from `USBSTS`/`USBINTR` (notably `PCD` for port changes).
- A minimal **asynchronous schedule** engine (QH/qTD) is implemented for control + bulk transfers.
- A minimal **periodic schedule** engine (frame list + interrupt QH/qTD) is implemented for interrupt polling.
- Snapshot/restore is implemented for EHCI controller state and attached USB topology.
- Companion routing semantics (`CONFIGFLAG` / `PORT_OWNER`) are implemented. EHCI treats
  `PORT_OWNER=1` ports as companion-owned/unreachable; full shared-port wiring to UHCI companions is
  still being integrated.

What is *not* implemented yet (still MVP-relevant):

- Isochronous periodic descriptors (`iTD` / `siTD`) and split/TT behavior.
- MSI/MSI-X (Aero uses PCI INTx for EHCI).
- Full platform wiring for **shared root ports** between EHCI and UHCI companions (routing a single
  physical device between two PCI functions) is still in progress.

Current code locations:

- Rust EHCI controller core: `crates/aero-usb/src/ehci/{mod.rs, regs.rs, hub.rs, schedule_async.rs, schedule_periodic.rs}`
- Rust EHCI tests: `crates/aero-usb/tests/ehci*.rs`
- Shared USB2 port mux model (EHCI↔UHCI routing building block): `crates/aero-usb/src/usb2_port.rs`
  (+ `crates/aero-usb/tests/usb2_companion_routing.rs`)
- Browser PCI device wrapper (worker runtime): `web/src/io/devices/ehci.ts` (+ `ehci.test.ts`)
- Native PCI device wrapper (MMIO BAR + IRQ + DMA gating): `crates/devices/src/usb/ehci.rs`

Important: the async + periodic schedule engines now exist in `aero-usb`. The sections below document
the intended contracts and call out remaining limitations (e.g. no iTD/siTD, no split/TT).

### Planned for Aero’s EHCI MVP (target scope)

The EHCI MVP design covers:

1. **Capability and operational registers**
   - Standard EHCI capability regs (`CAPLENGTH`, `HCIVERSION`, `HCSPARAMS`, `HCCPARAMS`) and the full
     operational register block (`USBCMD`, `USBSTS`, `USBINTR`, `FRINDEX`, `CTRLDSSEGMENT`,
     `PERIODICLISTBASE`, `ASYNCLISTADDR`, `CONFIGFLAG`, `PORTSC[n]`).
   - Read/modify/write masking, W1C status behavior, and “reserved reads as 0” rules where relevant.
2. **Root hub ports + timers**
   - Root hub ports are exposed via `PORTSC[n]` with realistic connect/enable/reset/suspend state.
   - Port reset/resume behaviors use **real timers** (ms-scale) so OS drivers that wait/poll observe
     plausible transitions.
3. **Asynchronous schedule** (QH/qTD) for **control + bulk**
   - Walking the async schedule (`ASYNCLISTADDR`) with QH and qTD parsing, and performing USB
     transactions against attached device models.
4. **Periodic schedule** for **interrupt polling**
   - Walking the periodic frame list (`PERIODICLISTBASE`) and QH/qTD chains sufficient to poll
     interrupt IN endpoints (e.g. HID input).
5. **Interrupt/IRQ semantics** (Aero runtime contract)
   - EHCI uses **PCI INTx level-triggered interrupts** in Aero (no MSI initially).
   - `irq_level()` is derived from `USBSTS` + `USBINTR` gating; runtime translates this into
     `raiseIrq`/`lowerIrq` transitions (see [`docs/irq-semantics.md`](./irq-semantics.md)).
6. **Snapshot/restore**
   - Deterministic save/load of EHCI register state, root hub port state/timers, and any internal
     scheduler bookkeeping required for forward progress.

### Intentionally not implemented (initially)

The initial EHCI implementation intentionally omits:

- **Isochronous transfers** (`iTD` / `siTD`)
  - Audio/video-class workloads are not targeted in the first bring-up; the schedule walker should
    treat non-QH periodic entries as “not supported / skipped” rather than crashing.
- **Split transactions / Transaction Translators (TT)**
  - EHCI can service full-/low-speed devices behind a high-speed hub using split transactions. That
    requires TT + companion routing behavior that we defer until we have a stable companion
    controller story.
- **MSI/MSI-X**
  - Aero’s EHCI uses PCI INTx only; `MSI` capability exposure and message-signaled interrupt
    routing are out of scope for MVP.

---

## PCI identity and wiring (Aero)

EHCI is exposed as a **PCI function** with the standard USB/EHCI class code (`0x0c/0x03/0x20`) and
one MMIO BAR for the EHCI register space.

### PCI identity (native runtime)

Native (`aero_machine` / `crates/devices`) uses the `USB_EHCI_ICH9` profile
(`crates/devices/src/pci/profile.rs`) for a Windows-7-friendly identity:

| Field | Value |
|---|---|
| BDF | `00:12.0` |
| Vendor ID | `0x8086` (Intel) |
| Device ID | `0x293a` (ICH9 EHCI) |
| Class code | `0x0c/0x03/0x20` (Serial bus / USB / EHCI) |
| Interrupt | PCI INTx (INTA#) |
| BARs | BAR0 = MMIO (0x1000 bytes) |

Note: the IRQ *line* observed by the guest depends on platform routing (PIRQ swizzle); see
[`docs/irq-semantics.md`](./irq-semantics.md).

### PCI identity (web runtime)

The browser runtime exposes an equivalent identity via `web/src/io/devices/ehci.ts`:

- BDF: `00:12.0`
- Vendor/device ID: `8086:293a`
- BAR0: `mmio32` 0x1000 bytes
- INTx: level-triggered, forwarded via `raiseIrq`/`lowerIrq` transitions

---

## Register model (contracts that matter to guests)

EHCI exposes a **memory-mapped register block** (typically 4 KiB). The first part is the
capability registers; the operational registers start at offset `CAPLENGTH`.

The following contract is what Aero’s implementation targets; consult the EHCI spec for full bit
definitions.

Implementation note (bring-up):

- In the web runtime, the EHCI PCI function exposes a **4 KiB MMIO BAR** (`web/src/io/devices/ehci.ts`).
- The current `aero-usb` EHCI model only implements the subset of registers needed for early driver
  bring-up (capability regs, operational regs, the USB Legacy Support extended capability, and
  `PORTSC[n]`). Reads from unimplemented offsets inside the “core” register window return `0`; reads
  beyond the modelled window are treated as open-bus (`0xff` bytes). This may be tightened to
  “reserved reads as 0” across the full 4 KiB window as the model matures.

### Capability registers (read-only)

The EHCI capability registers primarily allow the guest driver to discover:

- where the operational regs live (`CAPLENGTH`)
- number of root hub ports (`HCSPARAMS.N_PORTS`)
- whether 64-bit addresses are supported (`HCCPARAMS.AC64`)
- the optional “extended capabilities pointer” (`HCCPARAMS.EECP`)

**Aero contract:**

- Capability registers are **read-only**; writes are ignored.
- `CAPLENGTH` points to the start of the operational register block (commonly `0x20`).
- `HCSPARAMS.N_PORTS` matches the number of implemented `PORTSC[n]` registers.
- Aero currently models a **32-bit** EHCI controller (`HCCPARAMS.AC64=0`); schedule pointer upper
  bits (`CTRLDSSEGMENT`) are ignored and read back as 0.

#### EHCI extended capability: USB Legacy Support (BIOS handoff)

Many EHCI drivers (Windows/Linux) perform the “BIOS handoff” sequence so firmware stops owning the
controller before the OS begins DMA scheduling.

The EHCI spec places the **USB Legacy Support** capability (`USBLEGSUP` / `USBLEGCTLSTS`) in PCI
configuration space (and points to it via `HCCPARAMS.EECP`).

**Aero contract / current implementation:**

- `HCCPARAMS.EECP` is **non-zero** and points to the first extended capability.
- The extended capability registers are currently exposed **inside the EHCI MMIO window** at the
  `EECP` offset (see `crates/aero-usb/src/ehci/regs.rs` for rationale).
- `USBLEGSUP` behavior:
  - Low byte encodes `CAPID=0x01` (USB Legacy Support) and `NEXT=0`.
  - On reset, **BIOS-owned semaphore** starts set (`BIOS_SEM=1`).
  - When the guest sets **OS-owned semaphore** (`OS_SEM=1`), Aero immediately clears `BIOS_SEM`
    (models a successful handoff, without SMI/firmware timing).
- `USBLEGCTLSTS` is currently stored as a plain read/write register; SMI trap side-effects are not
  modeled.

### Operational registers (read/write)

The EHCI operational register block includes:

- `USBCMD`: run/stop, reset, schedule enables, doorbells.
- `USBSTS`: interrupt and schedule status (W1C for most status bits).
- `USBINTR`: interrupt enable mask.
- `FRINDEX`: current frame/microframe index.
- `CTRLDSSEGMENT`: upper 32 bits for 64-bit pointers (optional).
- `PERIODICLISTBASE`: base of periodic frame list.
- `ASYNCLISTADDR`: head of async QH list.
- `CONFIGFLAG`: port routing indicator (EHCI vs companions).
- `PORTSC[n]`: per-port status/control.

**Aero contract (read/write behavior):**

- Status bits that are defined as **W1C** are W1C in Aero. Writes that attempt to set read-only
  bits are masked out.
- `USBSTS.HCHALTED` is derived from `USBCMD.RunStop` and reset state; it is not directly writable.
- The “schedule status” bits (`USBSTS.ASS` / `USBSTS.PSS`) are modeled as derived state from
  `USBCMD.RunStop` + `USBCMD.ASE`/`USBCMD.PSE` (set when the controller is running and the
  corresponding schedule is enabled).
- `USBCMD.HCRESET` resets controller-local state (registers and scheduler bookkeeping) but should
  not implicitly detach devices from the root hub; device topology is modeled separately.
- `USBCMD.IAAD` (Interrupt on Async Advance Doorbell) is implemented:
  - software can set it to request an async advance interrupt, and
  - the controller clears it deterministically at the end of a tick and sets `USBSTS.IAA` (W1C).
  - If `USBINTR.IAA` is enabled, this contributes to `irq_level()`.

---

## Root hub ports (PORTSC) and timers

EHCI exposes a “root hub” via `PORTSC[n]` registers. These are **not** a USB hub device model; they
are a set of register-backed ports with connect/reset/enable state.

### Port state modeled per port

Each port tracks:

- **Connected** + **Connect Status Change** (CSC)
- **Enabled** + **Port Enable/Disable Change** (PEDC)
- **Reset** (PR) and a **reset timer** (real-time delay before reset completes)
- **Suspend/Resume** (SUSP/FPR) and a **resume timer** (if modeled)
- **Port power** (`PP`) (Aero currently models ports as powered-on by default, but honors writes)
- **Port owner** (`PORT_OWNER`) for companion routing (modeled; `PORT_OWNER=1` makes the port
  unreachable from EHCI; full shared-port wiring to UHCI companions is still being integrated)

Implementation note:

- The current `aero-usb` EHCI model defaults to **6 root hub ports** (see
  `crates/aero-usb/src/ehci/mod.rs::DEFAULT_PORT_COUNT`), which is a common PC-style EHCI
  configuration.

### Timing model

The guest driver typically performs sequences like:

1. detect connection (CCS/CSC)
2. set `PORTSC.PR` (reset)
3. wait for reset to complete
4. observe port enabled and begin enumeration

**Aero contract:**

- Port reset is modeled with a **~50 ms** countdown (USB-reset-scale), not “instant”.
- Timer advancement is driven by the VM/device tick (`tick_*`), not wall clock inside the device.
- When the reset timer expires:
  - `PORTSC.PR` clears.
  - The port becomes enabled (PED=1) if the device is still connected and the port is owned by EHCI.
  - Change bits are latched appropriately so the guest driver can observe the transition.

### Port change interrupts

EHCI has an interrupt cause for port changes. Aero models:

- Any event that sets a port change bit (CSC/PEDC/…) also latches `USBSTS.PCD` (Port Change Detect).
- If `USBINTR.PCD` is enabled, `USBSTS.PCD` contributes to `irq_level()`.

---

## Scheduler and time base

EHCI has two independent schedules:

- **Asynchronous schedule**: primarily control + bulk.
- **Periodic schedule**: interrupt and isochronous (we implement only the interrupt subset).

EHCI is defined in terms of **frames (1 ms)** subdivided into **microframes (125 µs)**.
`FRINDEX` carries both:

- low 3 bits: microframe (0–7)
- higher bits: frame index

**Aero contract (time stepping):**

- The controller advances `FRINDEX` in fixed increments from the emulator tick (e.g. an internal
  “step microframes” loop, or a “step 1ms” that performs 8 microframes).
- Work per tick is capped (like UHCI’s “max frames per tick” clamp) so background tab pauses do not
  cause multi-second catch-up stalls.
- Schedule processing is gated by:
  - PCI Bus Master Enable (DMA allowed) at the platform/device integration layer, and
  - `USBCMD.RunStop` + schedule enable bits (`USBCMD.ASE` / `USBCMD.PSE`) inside the controller.

---

## Asynchronous schedule (QH / qTD) — control + bulk

> Status: implemented (QH/qTD) in `crates/aero-usb/src/ehci/schedule_async.rs`.

### Structures (guest memory)

The async schedule is a linked list of **Queue Heads (QH)** starting at `ASYNCLISTADDR`. Each QH
contains an “overlay” region that behaves like the currently executing qTD.

Transfers are described by chained **queue element transfer descriptors (qTD)**.

**MVP:**

- QH + qTD parsing sufficient for:
  - control transfer stages (SETUP / DATA / STATUS)
  - bulk IN/OUT
- qTD buffer pointer handling sufficient for typical short, contiguous buffers used during
  enumeration and HID polling.

### Progress rules (how the guest observes completion)

In EHCI, “NAK” is not represented as an explicit completion code written back to guest memory; the
controller simply leaves the qTD **Active** and retries on later microframes.

**Aero contract:**

- When a transaction completes successfully:
  - qTD `Active` is cleared.
  - qTD `Total Bytes` is decremented to reflect **bytes remaining** (EHCI semantics).
  - The QH overlay is advanced to the next qTD.
- When a transaction is “pending” because it requires async host work (e.g. WebUSB/WebHID
  passthrough completion that hasn’t arrived yet), the qTD is left **Active** with no error bits set.
  This is the EHCI analogue of UHCI “return NAK while pending”.
- When a qTD has `IOC` set and completes, the controller latches `USBSTS.USBINT`.

### Error semantics (minimal but predictable)

For MVP, error modeling focuses on being deterministic and unblocking the guest:

- A device stall maps to qTD **Halted** + `USBSTS.USBERRINT` (if enabled).
- Other unexpected failures can map to a transfer error condition (and clear Active) rather than
  leaving the qTD permanently active.

---

## Periodic schedule — interrupt polling

> Status: implemented (periodic frame list + interrupt QH/qTD) in `crates/aero-usb/src/ehci/schedule_periodic.rs`.

The periodic schedule is driven by:

- `PERIODICLISTBASE`: base address of the periodic frame list.
- `FRINDEX`: selects the current frame list entry.

Real EHCI supports periodic entries for:

- iTD (isochronous)
- siTD (split isochronous / split interrupt)
- QH (interrupt)
- FSTN

**MVP:**

- Only periodic **QH** entries are executed.
- This is sufficient for interrupt IN polling used by:
  - USB HID keyboards/mice (when modeled as high-speed or behind a high-speed hub)
  - other “interrupt-style” devices that produce small periodic reports

### Microframe masks

High-speed interrupt endpoints use the QH **S-mask** to indicate which microframes the endpoint is
eligible to execute in.

**Aero contract:**

- The periodic walker honors `S-mask` at least at the coarse “eligible/not eligible” level so the
  guest driver doesn’t observe polling in every microframe.
- The walker does not implement split transaction `C-mask` behavior in MVP.

---

## Interrupt / IRQ semantics (Aero runtime)

### PCI interrupt type

EHCI is exposed as a PCI device that signals interrupts using **INTx** (level-triggered).

> Aero runtime contract for INTx and shared lines: [`docs/irq-semantics.md`](./irq-semantics.md)

**Aero contract:**

- The EHCI device model exposes `irq_level()` (or equivalent) which reflects *current* interrupt
  line assertion state.
- The platform integration translates transitions into `raiseIrq`/`lowerIrq` calls, and also gates
  assertion on PCI Command `Interrupt Disable`.

### What causes `irq_level()` to assert

`irq_level()` is asserted when:

1. A `USBSTS` interrupt cause bit is set **and**
2. The corresponding `USBINTR` enable bit is set.

Minimum set of causes implemented:

- `USBINT` (transaction completion; typically IOC-driven)
- `USBERRINT` (transaction error)
- `PCD` (Port Change Detect; root hub port change bits)
- `IAA` (interrupt on async advance doorbell)
  - Aero implements the `USBCMD.IAAD` → `USBSTS.IAA` doorbell/interrupt behavior.

Deassertion occurs once the guest clears the relevant `USBSTS` bits (W1C) and no other enabled
causes remain pending.

---

## Snapshot/restore requirements

EHCI state must be restorable deterministically across:

- native runs (`crates/emulator` / `aero_machine`)
- browser/WASM runs (`crates/aero-wasm` + `web/`)

### What must be snapshotted (minimum)

The EHCI snapshot must include:

- Capability values that are not purely compile-time constants (if any).
- Operational registers:
  - `USBCMD`, `USBSTS`, `USBINTR`
  - `FRINDEX`
  - `CTRLDSSEGMENT` (if supported)
  - `PERIODICLISTBASE`, `ASYNCLISTADDR`
  - `CONFIGFLAG`
- USB Legacy Support extended capability registers (BIOS handoff semaphores / legacy control bits):
  - `USBLEGSUP`, `USBLEGCTLSTS`
- Root hub port state for each `PORTSC[n]`, including:
  - connect/enable/change bits
  - reset/suspend state
  - countdown timers (reset/resume)
- The full **USB device topology** behind the root hub (i.e. `AttachedUsbDevice` snapshots for
  any attached hubs/HID devices/passthrough wrappers). EHCI snapshots should follow the same
  “controller snapshot includes nested device-model snapshots” approach used by UHCI so restores
  do not depend on the host pre-attaching devices purely to satisfy snapshot loading.
- Any internal bookkeeping that is *not* represented in guest RAM (e.g. cached “previous port
  change” values used to latch global status bits).

### What must NOT be snapshotted

- The PCI INTx “wire level” itself should not be persisted; it is derived from the above registers.
- Guest schedule structures (QHs/qTDs) are in guest RAM and are captured by the VM memory snapshot
  layer, not by the device snapshot blob.

### Passthrough host-state after restore (important)

If EHCI drives passthrough devices (WebUSB/WebHID), **host in-flight work cannot be resumed** after
restore (Promises cannot be rewound). The restore path must:

- drop queued/in-flight host actions/completions, and
- leave guest-visible schedule descriptors in a state where the guest will retry and re-issue work.

This is the same rule described for UHCI passthrough in
[`docs/webusb-passthrough.md#snapshotrestore-save-state`](./webusb-passthrough.md#snapshotrestore-save-state).

### Browser snapshot container note (current runtime wiring)

In the browser runtime, the I/O worker may store multiple USB controller blobs inside a single
`"AUSB"` container so newer snapshots can carry UHCI + EHCI side-by-side.

See:

- `web/src/workers/usb_snapshot_container.ts` (`USB_SNAPSHOT_TAG_UHCI`, `USB_SNAPSHOT_TAG_EHCI`)

---

## Companion controllers (CONFIGFLAG / PORT_OWNER)

### Why companions exist

EHCI is fundamentally a **high-speed** controller. On real PCs, full-speed/low-speed traffic on
root ports is often serviced by **companion controllers** (UHCI on Intel, OHCI on some other
chipsets). EHCI participates in *routing* rather than directly handling FS/LS on root ports.

Two key pieces of guest-visible behavior:

- `CONFIGFLAG` (in EHCI operational regs)
  - When set to 1 by the OS, it indicates the OS is taking ownership and routing ports to EHCI.
- `PORTSC[n].PORT_OWNER`
  - When set, the port is owned by a companion controller; EHCI should treat the port as not under
    its control for scheduling.

### Current behavior in Aero (implemented)

In Aero’s EHCI model:

- Root hub ports start with `PORT_OWNER=1` (companion-owned) by default.
- `CONFIGFLAG` is implemented as the “claim/release all ports” knob:
  - `CONFIGFLAG` `0→1` clears `PORT_OWNER` on all ports (EHCI owns).
  - `CONFIGFLAG` `1→0` sets `PORT_OWNER` on all ports (companion owns).
- `PORT_OWNER` is only writable while the port is disabled (matches typical EHCI semantics).
- If `PORT_OWNER=1`, EHCI treats the port as **unreachable** for scheduling:
  - `CCS` still reflects physical connection.
  - Reset/enable/suspend/resume state is dropped when the port is handed off.
- Any ownership change (`CONFIGFLAG` transition or `PORT_OWNER` toggle) asserts `USBSTS.PCD` so the
  guest sees a port change interrupt when `USBINTR.PCD` is enabled.

Separate (but related): `aero_usb::usb2_port::Usb2PortMux` can model a **single physical USB 2.0
root port** shared between an EHCI controller and a UHCI companion, and is used by unit tests as a
building block for future platform wiring (`crates/aero-usb/tests/usb2_companion_routing.rs`).

### Planned integration with UHCI companions

The intended platform topology is:

```
PCI: EHCI function (USB 2.0, high-speed)
PCI: UHCI companions (USB 1.1, full/low-speed)
Shared physical root ports
```

**Contract for ownership/routing (planned):**

- Each physical root port has a single “device attached” slot, but two logical controllers can
  potentially observe it depending on ownership.
- When `CONFIGFLAG=0` (or `PORT_OWNER=1`):
  - The port is routed to the companion controller.
  - EHCI reports the port as owned by companion and does not attempt to enumerate/schedule it.
- When `CONFIGFLAG=1` and `PORT_OWNER=0`:
  - The port is routed to EHCI and can enumerate high-speed devices.

**Important deferred part:** correct handling of **FS/LS devices behind EHCI** requires split
transactions + TT behavior, which we intentionally defer. Until then, “routing” is primarily about
selecting which controller owns a root port, not about TT.

---

## Testing strategy

EHCI touches guest memory scheduling, timing, interrupts, and snapshotting. The test plan is
layered:

### 1) Rust unit tests (synthetic schedules, memory-bus)

Current bring-up tests cover basic register/port behavior, schedule traversal, and snapshot/restore:

- `crates/aero-usb/tests/ehci.rs` (capability regs stability, port reset timer, `HCHALTED` tracking)
- `crates/aero-usb/tests/ehci_ports.rs` (PORTSC behavior: `CONFIGFLAG`/`PORT_OWNER`, high-speed indicator, line status)
- `crates/aero-usb/tests/ehci_async.rs` (async schedule QH/qTD traversal)
- `crates/aero-usb/tests/ehci_periodic.rs` (periodic frame list + interrupt QH/qTD polling)
- `crates/aero-usb/tests/ehci_snapshot.rs` (small snapshot roundtrip smoke test)
- `crates/aero-usb/tests/ehci_snapshot_roundtrip.rs` (snapshot/restore)
- `crates/aero-usb/tests/ehci_legacy_handoff.rs` (legacy BIOS handoff)

In `crates/aero-usb`, write unit tests that:

- build synthetic QH/qTD chains in a fake `MemoryBus`
- tick the controller through frames/microframes
- assert on:
  - qTD token updates (Active cleared, bytes updated, Halted on stall)
  - QH overlay advancement
  - `USBSTS` W1C behavior
  - interrupt causes → `irq_level()` transitions
  - port reset timing and change-bit latching

This is the EHCI analogue of the existing UHCI “schedule walker” tests.

### 2) `aero-pc-platform` integration tests

Add platform-level integration tests that instantiate a PCI topology with:

- EHCI controller
- UHCI companions (even if companions are initially inert)
- a small set of attached synthetic USB devices

Then validate:

- Windows enumerates the EHCI function (class `0x0C0320`) and binds the in-box EHCI driver.
- Root hub ports behave sensibly (reset/enumeration works).
- Interrupt polling (periodic schedule) delivers HID input reports.

### 3) Browser runtime harness (dev panel)

The browser runtime already has unit tests for the TypeScript PCI wrapper:

- `web/src/io/devices/ehci.test.ts` (MMIO forwarding/masking, INTx level→edge transitions, tick→frame conversion)

#### Current dev harness: WebUSB EHCI passthrough harness (not a full schedule engine)

The repo includes a developer-facing “EHCI-like” WebUSB passthrough harness that is deliberately
**not** the full EHCI DMA schedule walker. It exists to validate the WebUSB action↔completion
plumbing and basic EHCI-style interrupt/status reporting (`USBSTS` bits + IRQ level) without needing
a guest OS EHCI driver.

Implementation pointers:

- WASM harness: `crates/aero-wasm/src/webusb_ehci_passthrough_harness.rs` (`WebUsbEhciPassthroughHarness`)
- Worker runtime pump + message schema: `web/src/usb/webusb_ehci_harness_runtime.ts`
- UI panel (IO worker): `web/src/main.ts` (`renderWebUsbEhciHarnessWorkerPanel`)

The panel can:

- attach/detach the harness “controller” and a passthrough device
- execute basic control transfers (`GET_DESCRIPTOR(Device)` / `GET_DESCRIPTOR(Config)`)
- display:
  - last forwarded `UsbHostAction` and last applied `UsbHostCompletion`
  - `USBSTS` bits (`USBINT`/`USBERRINT`/`PCD`) and the derived INTx level
  - descriptor bytes received

#### Future: full EHCI controller introspection panel

Now that EHCI schedule walking (async/periodic) exists, expand the dev harness/panel to show
controller-level state (e.g. `USBCMD/USBSTS/FRINDEX/PORTSC`) and schedule traversal (QH/qTD overlay
state, IOC behavior, periodic polling cadence).

This is invaluable for debugging timing and schedule traversal issues that are hard to see from
inside the guest OS alone.
