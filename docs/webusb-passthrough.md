# WebUSB passthrough (browser → guest USB controller) architecture

This document describes the **USB passthrough** path where a *real* USB device
is exposed to the guest OS using **WebUSB** in the browser.

Runtime note: This document describes the legacy browser runtime (`vmRuntime="legacy"`) where guest USB controllers and the
`UsbPassthroughBridge` live in the I/O worker. The canonical machine runtime (`vmRuntime="machine"`) does not currently support WebUSB
passthrough.

> Source of truth: [ADR 0015](./adr/0015-canonical-usb-stack.md) defines the canonical USB stack
> selection for the browser runtime. This document describes the chosen `aero-usb` + `web/` design
> in detail.

The goal is to keep three moving parts coherent and spec-aligned:

- **Guest USB host controller emulation**:
  - **UHCI** (USB 1.1 full-speed; synchronous, TD-driven)
  - **EHCI** (USB 2.0 high-speed; supported/experimental for passthrough)
  - **xHCI** (USB 3.x, also handles USB 2.0/1.1; supported/experimental for passthrough)
    - EHCI controller emulation design/implementation notes: [`docs/usb-ehci.md`](./usb-ehci.md)
    - xHCI controller emulation design/implementation notes: [`docs/usb-xhci.md`](./usb-xhci.md)
- **Rust device model** (`UsbPassthroughDevice`; runs inside WASM/worker)
- **TypeScript WebUSB broker/executor** (runs where WebUSB is available; usually main thread)

Implementation references (current repo):

- Rust wire contract + action/completion queue (`UsbPassthroughDevice`): `crates/aero-usb/src/passthrough.rs`
- Rust guest-visible UHCI controller + TD handshake mapping: `crates/aero-usb/src/uhci/mod.rs`
- WASM export bridge (`UsbPassthroughBridge`): `crates/aero-wasm/src/lib.rs`
- WASM guest-visible UHCI controller (`UhciControllerBridge`) + WebUSB passthrough device lifecycle (`set_connected`, `drain_actions`, `push_completion`, `reset` on root port 1): `crates/aero-wasm/src/uhci_controller_bridge.rs` (re-exported from `crates/aero-wasm/src/lib.rs`)
- WASM guest-visible EHCI controller (`EhciControllerBridge`) + WebUSB passthrough device lifecycle (`set_connected`, `drain_actions`, `push_completion`, `reset` on root port 1; root port 0 remains available for an external hub / HID passthrough): `crates/aero-wasm/src/ehci_controller_bridge.rs` (re-exported from `crates/aero-wasm/src/lib.rs`)
- WASM guest-visible xHCI controller (`XhciControllerBridge`) + WebUSB passthrough device lifecycle (`set_connected`, `drain_actions`, `push_completion`, `reset` on a reserved root port; typically root port 1): `crates/aero-wasm/src/xhci_controller_bridge.rs` (re-exported from `crates/aero-wasm/src/lib.rs`)
- (Dev/harness) WASM standalone WebUSB UHCI bridge (`WebUsbUhciBridge`): `crates/aero-wasm/src/webusb_uhci_bridge.rs` (re-exported from `crates/aero-wasm/src/lib.rs`)
- WASM demo driver (`UsbPassthroughDemo`; queues GET_DESCRIPTOR requests to validate the action↔completion contract end-to-end): `crates/aero-wasm/src/lib.rs`
- WASM UHCI enumeration harness (dev smoke; `WebUsbUhciPassthroughHarness`): `crates/aero-wasm/src/webusb_uhci_passthrough_harness.rs`
- (Dev/harness) WASM EHCI passthrough harness (EHCI-like; validates action↔completion + USBSTS/IRQ semantics without full qTD/QH walking): `crates/aero-wasm/src/webusb_ehci_passthrough_harness.rs`
- TS canonical wire types (`SetupPacket`/`UsbHostAction`/`UsbHostCompletion`): `web/src/usb/usb_passthrough_types.ts`
- TS WebUSB backend/executor (`WebUsbBackend`): `web/src/usb/webusb_backend.ts` (+ `web/src/usb/webusb_executor.ts`)
- TS main-thread broker for workers (optional): `web/src/usb/usb_broker.ts` (+ `web/src/usb/usb_proxy_protocol.ts`, `web/src/usb/usb_proxy_ring.ts`)
- TS worker-side completion ring dispatcher (completion-ring fan-out when multiple runtimes subscribe): `web/src/usb/usb_proxy_ring_dispatcher.ts`
- TS worker-side passthrough runtime (action/completion pump): `web/src/usb/webusb_passthrough_runtime.ts`
- Guest-visible worker wiring (guest controller init/selection + WebUSB hotplug + passthrough runtime; `vmRuntime="legacy"`): `web/src/workers/io.worker.ts`
- TS worker-side demo runtime (drains `UsbPassthroughDemo` actions, pushes completions, defines `usb.demo.run`, emits `usb.demoResult`): `web/src/usb/usb_passthrough_demo_runtime.ts`
- TS worker-side UHCI harness runner (dev smoke): `web/src/usb/webusb_harness_runtime.ts`
- TS worker-side EHCI harness runner (dev smoke): `web/src/usb/webusb_ehci_harness_runtime.ts`
- TS guest-visible UHCI PCI device (I/O worker): `web/src/io/devices/uhci.ts`
- (Dev/harness) TS standalone WebUSB UHCI PCI device: `web/src/io/devices/uhci_webusb.ts`
- TS UI harness panels:
  - WebUSB diagnostics panel: `web/src/usb/webusb_panel.ts`
  - WebUSB passthrough broker panel: `web/src/usb/usb_broker_panel.ts` (rendered from `web/src/main.ts`)
  - WebUSB UHCI harness panel (main thread): `web/src/usb/webusb_uhci_harness_panel.ts`
- WebUSB passthrough demo panel (IO worker result + Run buttons, including a “Configuration full” rerun when `wTotalLength` indicates truncation): `web/src/main.ts` (`renderWebUsbPassthroughDemoWorkerPanel`)
- WebUSB UHCI harness panel (I/O worker): `web/src/main.ts` (`renderWebUsbUhciHarnessWorkerPanel`)
- WebUSB EHCI harness panel (I/O worker): `web/src/main.ts` (`renderWebUsbEhciHarnessWorkerPanel`)
- Cross-language wire fixture: `docs/fixtures/webusb_passthrough_wire.json`
- (Legacy repo-root WebUSB demo broker/client RPC; removed; not the passthrough wire contract): previously lived under `src/platform/legacy/webusb_{broker,client,protocol}.ts`

Note: An early WebUSB passthrough prototype lived in `crates/aero-wasm/src/usb_passthrough.rs`.
It has been removed in favor of the single canonical `UsbPassthroughBridge` WASM export in
`crates/aero-wasm/src/lib.rs` that uses `aero_usb::passthrough::{UsbHostAction, UsbHostCompletion}`
with `serde_wasm_bindgen`.

Note: `crates/emulator` consumes `crates/aero-usb` via a thin integration layer (PCI/PortIO wiring +
compatibility re-exports) for native/emulator tests. Per [ADR 0015](./adr/0015-canonical-usb-stack.md),
the browser/WASM runtime uses `aero-usb` + `aero-wasm` + `web/` and should not grow a parallel USB
stack in `crates/emulator`.

---

## Problem statement: async WebUSB vs synchronous UHCI

WebUSB operations are **asynchronous** (`Promise`-based) and can take an unbounded amount
of time from the VM’s point of view (scheduler latency, permission prompts, OS USB stack
latency, device latency).

UHCI, in contrast, is driven by a periodic schedule (frames) and consumes **Transfer
Descriptors (TDs)** that describe individual USB transactions. When the controller
processes a TD, the emulation must produce a **synchronous outcome** (“this TD is still
active”, “it completed with ACK”, “it stalled”, etc.) without blocking the worker thread.

### Why “TD-level NAK while pending” is the right mechanism

On real USB, endpoints can legitimately respond with **NAK** to indicate “not ready yet”.
For UHCI this maps cleanly to:

- leaving the TD **Active** so it remains scheduled, and
- retrying the TD on a later frame without treating it as an error.

This is the correct way to represent “we started a host-side WebUSB transfer but do not
have a completion yet”:

1. The guest continues running (no blocking `await` in the emulation core).
2. The controller naturally retries at the same granularity the guest OS expects (TDs).
3. Guest-side timeouts/cancellation behave correctly (drivers already handle long NAK
   streaks on bulk transfers and control-transfer stages).

Avoid alternatives such as:

- **Blocking the emulation thread** until a Promise resolves (would freeze the VM and can
  deadlock UI/worker scheduling).
- **Completing TDs later without NAK semantics** (guest drivers can observe “stuck” TDs
  and expect NAK/timeout behavior; also risks re-issuing transfers incorrectly).

---

## Two-layer design

### Layer 1 (Rust/WASM): `UsbPassthroughDevice`

The Rust-side device model must never call WebUSB directly. Instead it:

- emits **host actions** (`UsbHostAction`) describing what needs to happen on the host,
- consumes **host completions** (`UsbHostCompletion`) to finish TDs and deliver data.

Key properties:

- Pure state machine: deterministic given input TD stream + completions.
- No `async`/`await` required inside the VM core; “pending” is represented by NAK.
- One in-flight action per endpoint (recommended) to keep data-toggle behavior coherent
  (see [Bulk transfers](#bulk-transfers-packet-granularity-and-toggle-sync)).

Canonical wire shapes (locked down by `docs/fixtures/webusb_passthrough_wire.json` and shared
between Rust and TypeScript; see `crates/aero-usb/src/passthrough.rs` and
`web/src/usb/usb_passthrough_types.ts` (re-exported from `web/src/usb/webusb_backend.ts`):

```ts
type SetupPacket = {
  bmRequestType: number;
  bRequest: number;
  wValue: number;
  wIndex: number;
  wLength: number;
};

type UsbHostAction =
  | { kind: "controlIn"; id: number /* u32 */; setup: SetupPacket }
  | { kind: "controlOut"; id: number /* u32 */; setup: SetupPacket; data: Uint8Array }
  | { kind: "bulkIn"; id: number /* u32 */; endpoint: number /* endpoint address */; length: number }
  | { kind: "bulkOut"; id: number /* u32 */; endpoint: number /* endpoint address */; data: Uint8Array };

type UsbHostCompletion =
  | { kind: "controlIn"; id: number /* u32 */; status: "success"; data: Uint8Array }
  | { kind: "controlIn"; id: number /* u32 */; status: "stall" }
  | { kind: "controlIn"; id: number /* u32 */; status: "error"; message: string }
  | { kind: "controlOut"; id: number /* u32 */; status: "success"; bytesWritten: number }
  | { kind: "controlOut"; id: number /* u32 */; status: "stall" }
  | { kind: "controlOut"; id: number /* u32 */; status: "error"; message: string }
  | { kind: "bulkIn"; id: number /* u32 */; status: "success"; data: Uint8Array }
  | { kind: "bulkIn"; id: number /* u32 */; status: "stall" }
  | { kind: "bulkIn"; id: number /* u32 */; status: "error"; message: string }
  | { kind: "bulkOut"; id: number /* u32 */; status: "success"; bytesWritten: number }
  | { kind: "bulkOut"; id: number /* u32 */; status: "stall" }
  | { kind: "bulkOut"; id: number /* u32 */; status: "error"; message: string };
```

For bulk transfers, `endpoint` is a USB endpoint **address** (direction bit included), not just the
endpoint number. See [Bulk transfers](#bulk-transfers-packet-granularity-and-toggle-sync) for details.

The `id` correlates an action with its completion and is also how we prevent duplicate
WebUSB calls when the guest retries a NAKed TD.

⚠️ **WASM note:** ids are generated in Rust and must fit in a JS `number` without loss.
The canonical wire contract uses **non-zero `u32` ids** (`1..=0xFFFF_FFFF`; `0` is reserved/invalid).
The worker-side runtime
(`web/src/usb/webusb_passthrough_runtime.ts`) accepts `number` or `bigint` ids from WASM, but
will reject and reset the bridge if an action id is missing or out of the `u32` range (to avoid
deadlocking the Rust-side action queue on an action we can never complete).

⚠️ **WASM note:** the USB passthrough drain APIs return `null` when there are no queued actions
(to keep the poll path allocation-free). Treat `null`/`undefined` as “no work”.
This applies to:
- `UsbPassthroughBridge.drain_actions()`
- `UhciControllerBridge.drain_actions()`
- `EhciControllerBridge.drain_actions()`
- `XhciControllerBridge.drain_actions()`
- `WebUsbUhciBridge.drain_actions()`
- `UhciRuntime.webusb_drain_actions()`

Cancellation behavior:

- A new control SETUP can legally abort a previous control transfer. `UsbPassthroughDevice` treats
  any new `(setup, data)` tuple as a new request. It cancels the previous in-flight id and:
  - if the host has not dequeued the old `UsbHostAction` yet, it drops it from the action queue so
    we do not execute a stale control transfer, and
  - ignores any later completion for the canceled id (WebUSB does not provide strong cancellation
    for an in-flight transfer).
- Stale completions are expected and must be ignored (the Rust model does this by checking `id`
  against in-flight state in `push_completion`).
- `UsbPassthroughDevice::reset()` clears queued actions/completions and cancels all in-flight
  requests.

### Snapshot/restore (save-state)

`crates/aero-usb` device models support deterministic snapshot/restore using the repo-standard
`aero-io-snapshot` TLV encoding (`aero_io_snapshot::io::state::IoSnapshot`).

For WebUSB passthrough specifically:

- `UsbPassthroughDevice` snapshots only its monotonic action id counter (`next_id`) and **drops all**
  queued/in-flight host I/O on restore. This prevents replaying side effects after restore; the guest
  will naturally retry transfers, emitting fresh `UsbHostAction`s.
- `UsbWebUsbPassthroughDevice` snapshots guest-visible USB state (address + control pipe stage) so a
  restore taken mid-control-transfer does not deadlock the guest. Newer snapshots also include the
  full internal `UsbPassthroughDevice` host-action state, so an in-flight transfer can resume
  deterministically: the relevant TD will continue returning NAK until a completion with the same
  action id is injected (and no duplicate `UsbHostAction`s are emitted).

Browser integration note (important):

- WebUSB host actions are backed by JS Promises that cannot be resumed after a VM snapshot restore.
  The WASM restore paths therefore call `reset_host_state_for_restore()` after restore (e.g.
  `UhciControllerBridge.load_state()`, `WebUsbUhciBridge.load_state()`, `UhciRuntime.load_state()`,
  `EhciControllerBridge.load_state()`, `XhciControllerBridge.load_state()`).
  - This clears queued/in-flight host actions/completions, preventing deadlock on a completion that
     will never arrive.
  - The monotonic `next_id` is preserved, so re-emitted actions still have deterministic ids.

WASM snapshot API note:

- The UHCI WASM bridges expose deterministic snapshot bytes via `snapshot_state()/restore_state()`
  (aliases over the existing `save_state/load_state` entrypoints). These snapshot bytes represent only
  USB stack state (controller + device models), not guest RAM.

Idempotency / retry behavior (important):

- A NAKed TD will be retried by the UHCI schedule. This means the device model entrypoints
  (`handle_control_request`, `handle_in_transfer`, `handle_out_transfer`) can be called multiple
  times for the same guest-visible transfer.
- `UsbPassthroughDevice` is written to be **idempotent** under retries:
  - control requests use an “in-flight” record to avoid emitting duplicate host actions while a
    completion is pending
  - non-control endpoints use a per-endpoint in-flight map for the same reason
  - completions are keyed by `id` and consumed exactly once

### Layer 2 (host/TS): WebUSB executor + broker (main thread)

The host side owns the actual `USBDevice` handle and performs WebUSB calls:

- **Executor**: receives `UsbHostAction`, runs the corresponding WebUSB operation, and
  sends back `UsbHostCompletion`.
- **Broker**: deals with UI and lifecycle concerns:
  - `navigator.usb.requestDevice()` (must be triggered by user activation)
  - (re)open/select configuration/claim interfaces
  - handling `disconnect` events and surfacing errors to UI

In this repo, the canonical WebUSB passthrough integration lives under `web/src/usb/`:

- **Executor** (canonical `UsbHostAction` contract): `web/src/usb/webusb_backend.ts`
  - (thin wrapper): `web/src/usb/webusb_executor.ts`
- **Main thread broker** (worker proxy): `web/src/usb/usb_broker.ts`
  - (message schema + validators): `web/src/usb/usb_proxy_protocol.ts`

Note: the repo previously included a separate **legacy/demo** WebUSB broker/client RPC under
`src/platform/legacy/webusb_*`. It has been removed and was never the `UsbHostAction` passthrough
contract described in this document.

### Device lifecycle: open/configuration/interface claiming

WebUSB requires the browser process to:

1. `device.open()`
2. `device.selectConfiguration(...)` (if no active configuration)
3. `device.claimInterface(...)` for any interface whose endpoints will be used

In this repo:

- `WebUsbBackend.ensureOpenAndClaimed()` (`web/src/usb/webusb_backend.ts`) performs the open/select
  configuration/claim steps before executing a `UsbHostAction`.
- If WebUSB must run on the main thread, `UsbBroker` (`web/src/usb/usb_broker.ts`) owns the
  `USBDevice` handle and services worker requests via the `usb_proxy_protocol.ts` message schema.

The TypeScript executor (`WebUsbBackend.execute()`) also recognizes a few **standard USB control
requests** that represent high-level device state transitions and routes them through the
dedicated WebUSB APIs instead of emitting a raw `controlTransferOut`:

- `SET_CONFIGURATION` → `USBDevice.selectConfiguration(...)`
  - Note: WebUSB can reject `selectConfiguration` while interfaces are claimed; the executor
    releases claimed interfaces first.
  - The executor handles this before the general “claim interfaces” path so that a guest-driven
    configuration switch cannot be blocked by unrelated interface-claim failures.
- `SET_INTERFACE` → `USBDevice.selectAlternateInterface(...)` (claiming the interface if needed)
- `CLEAR_FEATURE(ENDPOINT_HALT)` → `USBDevice.clearHalt(...)`

This keeps the canonical `UsbHostAction` / `UsbHostCompletion` wire contract unchanged, but tends
to be more reliable on real devices because browsers/OS stacks may treat these requests specially.

Important constraints for passthrough:

- **Do not blindly claim every interface** on composite devices. Devices can expose a mix of
  protected (unclaimable) and unprotected interfaces; claiming a protected interface can fail
  even when the device was selectable due to an unprotected interface.
  - Use the repo’s protected-class classifier (`web/src/platform/webusb_protection.ts`) to choose
    claimable interfaces.
- **Guest-visible configuration vs host-visible configuration:** the guest may issue
  `SET_CONFIGURATION` / `SET_INTERFACE`. The passthrough backend must decide whether to:
  - mirror those changes to the physical device (calling WebUSB `selectConfiguration` and
    `claimInterface`/`selectAlternateInterface` as needed), or
  - virtualize them (presenting descriptors/configuration state to the guest without mutating the
    already-open physical device).

The current Rust `UsbHostAction` surface does not yet include “select configuration / claim
interface” actions; open/config/claim is still handled by `WebUsbBackend.ensureOpenAndClaimed()`,
but the executor will mirror the most common standard configuration/interface transitions to the
host device as described above (and update its internal “claimed interface” cache when the active
configuration changes).

### Encoding `SetupPacket` for WebUSB

Rust-side USB control requests use a USB 1.1-style `SetupPacket`:

```text
bmRequestType, bRequest, wValue, wIndex, wLength
```

WebUSB represents the same information as:

- operation choice: `controlTransferIn(...)` vs `controlTransferOut(...)` (direction)
- `USBControlTransferParameters`:
  - `requestType: 'standard' | 'class' | 'vendor'`
  - `recipient: 'device' | 'interface' | 'endpoint' | 'other'`
  - `request` / `value` / `index`
- and an explicit `length` argument for IN transfers

Important rules:

- The **direction bit** (`bmRequestType & 0x80`) must match the WebUSB call you make.
  - For `controlTransferIn`, require Device→Host.
  - For `controlTransferOut`, require Host→Device.
- For OUT transfers, the payload length must match `wLength`. For a zero-length OUT request (`wLength == 0`),
  omit the data argument (or use an empty payload in the `UsbHostAction` contract).

In this repo:

- The production WebUSB executor has helpers for this conversion and direction checking:
  `web/src/usb/webusb_backend.ts` (`parseBmRequestType`, `validateControlTransferDirection`,
  `setupPacketToWebUsbParameters`).
- `WebUsbBackend` normalizes WebUSB `DataView` payloads into `Uint8Array` completions
  (`dataViewToUint8Array`). If you forward completions across `postMessage`, you may transfer the
  underlying `ArrayBuffer` to avoid copies.
  - `web/src/usb/usb_proxy_protocol.ts` exports helpers (`getTransferablesForUsbProxyMessage`, etc.)
    to pick the correct transfer list for `usb.action` / `usb.completion` messages.
  - Note: transferring detaches the sender’s `ArrayBuffer`. Treat the payload `Uint8Array` as
    consumed after `postMessage`.
  - Some buffers (notably `WebAssembly.Memory.buffer`) are not transferable and will throw if put
    in the transfer list. Production code should fall back to non-transfer `postMessage` (copy) in
    that case; the built-in passthrough runtime/broker already do this.

### Physical disconnect / guest hot-unplug

When the physical device is unplugged, the browser fires a `navigator.usb` `"disconnect"` event.
For guest correctness, this must be reflected as a **USB disconnect** on the emulated root hub
port so the guest OS can tear down drivers cleanly.

In this repo:

- `UsbBroker` listens for `navigator.usb` disconnect events and tears down the selected device
  (`web/src/usb/usb_broker.ts`). It resolves any in-flight actions and broadcasts
  `{ type: "usb.selected", ok: false, error: ... }` to attached worker ports.
- Guest-side hot-unplug should detach the emulated device from its UHCI port so the guest observes
  the connect-status-change bits (e.g. `UhciController::hub_mut().detach(port_index)` in
  `crates/aero-usb/src/uhci/mod.rs`).

Recommended behavior on a physical disconnect:

1. Detach the passthrough device model from the associated emulated port (root hub or downstream hub).
2. Cancel any in-flight host actions (call `reset()` / cancel hooks on the *existing* device model).
3. Ignore any completions that arrive after detach (they will be stale by `id` **as long as ids are not reused**;
   see [Action id monotonicity across disconnect/reconnect](#action-id-monotonicity-across-disconnectreconnect)).

#### Action id monotonicity across disconnect/reconnect

`UsbPassthroughDevice` treats completions as *stale* by checking whether the completion `id` is currently
recorded as “in flight” (`push_completion` drops completions whose id is not in the in-flight maps).
This relies on action ids being **monotonic / never reused** across the lifetime of the passthrough device
model.

Because WebUSB transfers are Promise-based, a transfer started *before* a disconnect can still resolve
later (success/error) even after the browser has fired a `"disconnect"` event. If the host integration
**drops** the `UsbWebUsbPassthroughDevice` / `UsbPassthroughDevice` instance on disconnect and later creates
a fresh one, its action ids restart at `1`. Late completions from the previous “session” can then collide
with newly reused ids and be incorrectly accepted as completions for the new in-flight transfers.

**Requirement / recommendation:**

- Keep a single `UsbWebUsbPassthroughDevice` (or underlying `UsbPassthroughDevice`) instance alive across
  connect/disconnect/reconnect.
- On disconnect:
  - detach it from the emulated hub/port (`set_connected(false)` / `hub.detach(...)`), and
  - call `reset()` (or equivalent) to clear queued/in-flight host actions/completions,
  - but **do not reset** the monotonic `next_id` counter (note: `UsbPassthroughDevice::reset()` clears
    host state but intentionally does *not* reset `next_id`).
- On reconnect, reattach the same device model instance so ids continue increasing.

This applies equally to WASM UHCI integrations: keep the same `WebUsbUhciBridge` / `UhciControllerBridge`
handle alive across physical disconnect/reconnect, and use `set_connected(false)` + `reset()` instead of
destroying and recreating the bridge (which would restart ids).

Data flow (conceptual):

```
Guest UHCI TDs
    │
    ▼
UHCI emulation (worker) ──calls──► UsbPassthroughDevice (worker)
    │                                 │
    │                                 ├─ emits UsbHostAction ───────────────┐
    │                                 │                                       │
    │                                 ◄─ consumes UsbHostCompletion ─────────┘
    │
    ▼
IRQ / TD status updates back to guest

Host-side (main thread)
  WebUSB broker/executor owns USBDevice and services actions.
```

---

## Stage mapping (UHCI TDs ↔ WebUSB transfers)

### Control transfers (SETUP/DATA/STATUS)

WebUSB exposes control transfers as **one call**:

- `controlTransferIn(setup, length)`
- `controlTransferOut(setup, data)`

UHCI represents the same operation as a TD chain:

1. **SETUP TD** (8 bytes, PID=SETUP, DATA0)
2. **DATA TD(s)** (PID=IN or OUT, DATA1 toggling)
3. **STATUS TD** (zero-length, PID opposite of DATA, DATA1)

Aero mapping (current `aero-usb` stack):

- **SETUP TD**
  - Decoded and dispatched by `UhciController` (`crates/aero-usb/src/uhci/mod.rs`): it reads the
    8-byte setup packet, parses `aero_usb::SetupPacket`, and forwards it to the addressed
    `device::AttachedUsbDevice::handle_setup` (`crates/aero-usb/src/device.rs`).
  - SETUP TDs always complete with **ACK** once a device is present. NAK is not used for SETUP.

- **DATA + STATUS TDs**
  - The UHCI-visible passthrough device wrapper turns the full control request (setup + optional OUT
    data stage) into exactly one `UsbHostAction::ControlIn` / `UsbHostAction::ControlOut`
    (wire `kind: "controlIn" | "controlOut"`) via
    `UsbPassthroughDevice::handle_control_request` (`crates/aero-usb/src/passthrough.rs`).
  - While the host action is in-flight, retries of the relevant **DATA** or **STATUS** TD return
    **NAK**, leaving the TD active so the UHCI schedule retries it on later frames.
    - Control-IN: pending is applied to the **DATA (IN)** stage (and to **STATUS (OUT)** when
      `wLength == 0`).
    - Control-OUT: OUT **DATA** TDs are ACKed as bytes are buffered; pending is applied to
      **STATUS (IN)** once the full payload is buffered (or immediately when `wLength == 0`).
- When the host completion arrives:
  - Control-IN: the completion’s `data` is served to IN TDs. An empty payload is represented as an
    ACK with `bytes=0` (ZLP). (`UhciController` encodes a 0-byte completion as `actlen=0x7FF`.)
  - Control-OUT: once the completion reports `status: "success"`, the STATUS stage ACKs with a
    0-byte packet.
  - `status: "stall"` maps to STALL; `status: "error"` maps to TIMEOUT (see
    [Host completion to guest TD status mapping](#host-completion-to-guest-td-status-mapping)).

Note on **short packets** (Control-IN):

- Real devices may legally return fewer bytes than `wLength` (e.g. descriptor reads where the OS
  asks for 255 bytes but the descriptor is shorter). This appears to the UHCI layer as a **short
  packet** (`actlen < maxlen`) on an IN DATA TD.
- Guest UHCI drivers typically rely on the UHCI **short packet detect** (SPD) bit + the
  `USBINTR_SHORT_PACKET` enable bit to get an interrupt and terminate the DATA stage early (skipping
  any remaining IN TDs and proceeding to the STATUS stage).
- `aero-usb`’s UHCI model honors SPD by stopping further TD processing for the current queue head
  within the same frame when a short packet is received and SPD is set.

Special-case note: `SET_ADDRESS` must be virtualized for full guest enumeration (guest-visible USB
address changes must not be forwarded to the physical device, which is already host-enumerated).

### Bulk transfers: packet granularity and toggle sync

UHCI bulk/interrupt transfers are naturally TD-per-packet. WebUSB bulk/interrupt APIs
(`transferIn`/`transferOut`) can represent **multi-packet** transfers, but we generally
should not use that for UHCI passthrough.

**Recommendation: issue one `UsbHostAction` per guest TD**, with `length <= wMaxPacketSize`.

Why:

- The guest driver sets the TD’s **DATA0/DATA1 toggle** expecting it to advance exactly
  once per successful packet.
- Collapsing multiple guest TDs into one WebUSB transfer advances the physical endpoint’s
  toggle multiple times while the guest only advances once, desynchronizing the stream.

Note: the current `aero-usb` UHCI implementation (`crates/aero-usb/src/uhci/mod.rs`) does not yet model
the TD token’s data-toggle bit. The “one packet per action” rule is therefore forward-looking, but
still the recommended shape to avoid subtle bugs once toggle tracking is implemented.

Mapping:

- **Bulk OUT TD** → `UsbHostAction::BulkOut { endpoint, data }` → `USBDevice.transferOut(endpoint & 0x0f, ...)`
- **Bulk IN TD** → `UsbHostAction::BulkIn { endpoint, length }` → `USBDevice.transferIn(endpoint & 0x0f, ...)`

(`endpoint` is a USB endpoint **address**. For IN transfers it is `0x80 | ep_num` (e.g. `0x81`);
for OUT transfers it is `ep_num` with bit7 clear (e.g. `0x02`). The host side should use
`endpoint & 0x0f` as the WebUSB `endpointNumber`, and use the action kind to determine direction.)

Pending behavior:

- When an action is in flight, retry attempts of the same TD return **NAK** without
  emitting another action (keyed by `id` / TD identity).
- When completion arrives, complete the TD with:
  - actual byte count (short packets are valid and often meaningful), or
  - STALL / error mapping (see [Host completion to guest TD status mapping](#host-completion-to-guest-td-status-mapping)).

---

## Host completion to guest TD status mapping

At the browser layer, WebUSB returns a `USBTransferStatus` (`"ok" | "stall" | "babble"`) for
`controlTransfer*` / `transfer*` calls, and can also throw `DOMException`s for other failures
(permissions, disconnects, OS driver issues, etc).

`web/src/usb/webusb_backend.ts` normalizes those outcomes into the canonical `UsbHostCompletion`
wire shape:

Recommended normalization rules (current implementation):

- `status === "ok"` → `status: "success"` (with `data` for IN or `bytesWritten` for OUT)
- `status === "stall"` → `status: "stall"`
- `status === "babble"` → `status: "error"` (with a message)
- thrown `DOMException` / other failures → `status: "error"` (with a message)

Guest-visible behavior is then derived from the Rust mapping in
`crates/aero-usb/src/passthrough.rs`:

| Host completion (`UsbHostCompletion`) | Guest-visible outcome | Notes |
|---|---|---|
| `{ status: "success", data }` (IN kinds) | `DATA` (for IN TDs) | Data is truncated to `wLength` for control-IN and to the TD `max_len` for bulk IN. |
| `{ status: "success", bytesWritten }` (OUT kinds) | `ACK` | OUT TD completes successfully. |
| `{ status: "stall" }` | `STALL` | TD completes with STALLED; guest driver is responsible for recovery. |
| `{ status: "error", message }` | `TIMEOUT` | Passthrough maps non-stall errors to a UHCI timeout/CRC error to unblock the guest. |
| (no completion yet; action in-flight) | `NAK` | Keep TD active so the UHCI schedule naturally retries without duplicating host work. |

Implementation note: in `aero-usb`, `Nak` is a first-class “retry later” outcome:

- `UhciController` sets `TD_CTRL_NAK` and leaves the TD active (`crates/aero-usb/src/uhci/mod.rs`).
- `UsbPassthroughDevice` returns `ControlResponse::Nak` / `UsbInResult::Nak` / `UsbOutResult::Nak`
  while a host action is pending (`crates/aero-usb/src/passthrough.rs`).

---

## Speed and descriptor handling (UHCI vs EHCI/xHCI)

### UHCI mode: guest is full-speed

When the passthrough device is attached to a guest **UHCI** controller, the guest sees it
as a **full-speed** device.

This creates a mismatch when the physical device is high-speed (USB 2.0) on the real
machine: we must present a **full-speed-compatible configuration** to the guest so it
chooses correct max packet sizes and intervals.

### UHCI-only fixup: `OTHER_SPEED_CONFIGURATION` → `CONFIGURATION`

USB 2.0 devices can expose an `OTHER_SPEED_CONFIGURATION` descriptor that describes how
they would look at the *other* speed:

- If the device is currently operating at **high-speed**, `OTHER_SPEED_CONFIGURATION`
  describes the **full-speed** configuration (endpoint max packet sizes, intervals, etc).

Approach:

1. During enumeration, issue a standard control request:
   - `GET_DESCRIPTOR(OTHER_SPEED_CONFIGURATION, index=0)`
2. If present and well-formed:
   - Use it as the basis for the guest-visible configuration, but rewrite the top-level
      `bDescriptorType` from `OTHER_SPEED_CONFIGURATION` to `CONFIGURATION` before exposing
      it to the guest stack (the layout is otherwise identical).
3. If not present (or rejected by WebUSB):
   - Fall back to the regular `CONFIGURATION` descriptor.

Practical implications:

- Devices that are **high-speed-only** without a usable other-speed configuration are not
  good candidates for UHCI passthrough; they should be attached via **EHCI/xHCI** so the
  guest can enumerate them at high-speed.
- Even for high-speed devices, sending smaller full-speed-sized transfers via WebUSB is
  typically valid (it is legal to transfer less than max packet size), but correctness
  depends on descriptors matching what the guest believes.

In this repo, the production WebUSB executor performs this as a best-effort fixup inside
`executeWebUsbControlIn` (`web/src/usb/webusb_backend.ts`; see `shouldTranslateConfigurationDescriptor`
and `rewriteOtherSpeedConfigAsConfig`).

⚠️ This translation is **UHCI/full-speed-only**. When a passthrough device is attached to a
guest **EHCI/xHCI** controller (high-speed view), the host executor must **not**:

- attempt to fetch `OTHER_SPEED_CONFIGURATION`, or
- rewrite an `OTHER_SPEED_CONFIGURATION` descriptor into a `CONFIGURATION` descriptor.

In EHCI/xHCI mode the guest expects the device’s **high-speed** descriptors as-is, and any
UHCI-oriented fixups would produce incorrect endpoint packet sizes/intervals.

### EHCI/xHCI mode: guest is high-speed

When a passthrough device is attached to a guest **EHCI/xHCI** controller (supported in the web
runtime when the WASM build exports the required WebUSB passthrough hooks on those controller
bridges), the intended behavior is:

- EHCI controller model design/contract: [`docs/usb-ehci.md`](./usb-ehci.md)
- xHCI controller model design/contract: [`docs/usb-xhci.md`](./usb-xhci.md)
- The guest enumerates the physical device as **high-speed**.
- The WebUSB executor should forward `GET_DESCRIPTOR(CONFIGURATION)` results without rewriting
  descriptor types or attempting other-speed translation.
- Devices that are **high-speed-only** should be attached via EHCI/xHCI rather than UHCI, since a
  UHCI/full-speed view cannot represent them correctly.

---

## Worker and threading constraints (browser reality)

### User activation is required for device selection

`navigator.usb.requestDevice()` must be called from a **user-activated** event handler
(e.g. button click). This forces a “broker” role on the UI layer:

- the main thread is responsible for prompting and persisting the selected device,
- the emulator worker cannot autonomously attach arbitrary devices.

In this repo, the production UI triggers selection via `UsbBroker.requestDevice()` (which must be
called directly from a click handler; see `web/src/usb/usb_broker_panel.ts` and
`web/src/usb/usb_broker.ts`).

### `USBDevice` is likely non-transferable

In practice, `USBDevice` should be treated as **non-structured-cloneable** and therefore
non-transferable to workers. Even if some browser versions eventually allow worker access,
this should not be relied upon for the core architecture.

The production WebUSB diagnostics panel includes a probe worker that attempts structured cloning of
the selected `USBDevice` (`web/src/usb/webusb_panel.ts`, `web/src/usb/webusb_probe_worker.ts`).

### Recommended architecture when WebUSB is unavailable in workers

Assume WebUSB calls must run on the main thread:

- **Worker (WASM):** UHCI + `UsbPassthroughDevice` emits actions via a queue/ring buffer.
- **Main thread:** broker/executor receives actions, calls WebUSB, and returns completions.

#### Default / legacy path: `postMessage` + transferred `ArrayBuffer`s

This pattern is implemented by `UsbBroker` (main thread) using a `postMessage` protocol
(`web/src/usb/usb_broker.ts`, `web/src/usb/usb_proxy_protocol.ts`):

- worker → main thread: `{ type: "usb.action", action: UsbHostAction }`
- main thread → worker: `{ type: "usb.completion", completion: UsbHostCompletion }`

Byte payloads (`Uint8Array`) are transferred where possible to avoid copies. (A legacy broker/client RPC
implementation previously existed under `src/platform/legacy/webusb_*`, but it has been removed and was not the
canonical passthrough wire contract.)

#### Fast path: SharedArrayBuffer ring buffers (`usb.ringAttach`)

When `globalThis.crossOriginIsolated === true` (COOP/COEP enabled) and `SharedArrayBuffer`/`Atomics`
are available, the broker/worker proxy enables an optional SharedArrayBuffer-backed ring-buffer fast path
negotiated by `{ type: "usb.ringAttach", actionRing, completionRing }`:

- **`actionRing` (worker → main thread):**
  - the worker-side passthrough runtime writes `UsbHostAction` records into the ring (SPSC producer).
  - the main thread drains actions on a timer and executes them via `WebUsbBackend`.
- **`completionRing` (main thread → worker):**
  - the main thread writes `UsbHostCompletion` records into the ring (SPSC producer).
  - the worker drains completions via a shared dispatcher (`usb_proxy_ring_dispatcher.ts`) so multiple runtimes
    on the same port can observe completions without racing each other to `popCompletion()`.

The fast path is opportunistic: when a ring is full (or a record is too large to fit), the sender falls back
to `postMessage` (`usb.action` / `usb.completion`) so passthrough continues to make forward progress even under
temporary backpressure.

If a worker-side runtime starts after the initial `usb.ringAttach` (e.g. WASM finished loading late), it can
request the ring handles via `{ type: "usb.ringAttachRequest" }`. The production `UsbBroker` responds by
re-sending the ring buffers when possible; older brokers may ignore this message and the runtime should keep
functioning via the `postMessage` path.

If a ring buffer becomes corrupted (e.g. a decode error while popping records) the runtime can request the
broker to disable the SharedArrayBuffer fast path for that port by sending `{ type: "usb.ringDetach", reason? }`.
The broker will stop draining the action ring / pushing completions into the completion ring and will fall back
to `postMessage` (`usb.action` / `usb.completion`). Runtimes should treat `usb.ringDetach` as a signal to detach
their local ring wrappers and continue proxying via `postMessage`.

Implementation pointers (current code):

- Ring buffer: `web/src/usb/usb_proxy_ring.ts` (`UsbProxyRing`)
- Protocol schema: `web/src/usb/usb_proxy_protocol.ts` (`usb.ringAttach`, `usb.ringAttachRequest`, `usb.ringDetach`)
- Main thread setup + action-ring drain: `web/src/usb/usb_broker.ts` (`attachRings`, `drainActionRing`)
- Worker-side attach + action forwarding: `web/src/usb/webusb_passthrough_runtime.ts` (`attachRings`, `pollOnce`)
- Worker-side completion drain + fan-out: `web/src/usb/usb_proxy_ring_dispatcher.ts`
- Integration tests: `web/src/usb/usb_proxy_ring_integration.test.ts`

If the emulator uses WASM threads / SharedArrayBuffer (preferred), use the existing
cross-thread IPC mechanism described in [`docs/11-browser-apis.md`](./11-browser-apis.md)
and [ADR 0002](./adr/0002-cross-origin-isolation.md).

---

## Security and compatibility notes

- **Cross-origin isolation:** not required by WebUSB itself, but required for Aero’s
  threaded build (`SharedArrayBuffer`, WASM threads). See:
  - [Browser APIs: deployment headers](./11-browser-apis.md#deployment-headers)
  - [ADR 0002: Cross-Origin Isolation](./adr/0002-cross-origin-isolation.md)
- **Browser support:** WebUSB is effectively **Chromium-only** (Chrome/Edge). Expect no
  support in Firefox/Safari; passthrough must be optional and feature-detected.
- **Secure context:** WebUSB requires HTTPS (or `http://localhost`).

### Protected interface classes (Chromium WebUSB restrictions)

Chrome blocks WebUSB access to certain “protected” USB interface classes (to avoid
interfering with system devices/drivers).

Aero maintains a best-effort list for diagnostics and UX:

- `web/src/platform/webusb_protection.ts` (`PROTECTED_USB_INTERFACE_CLASSES`)
- `web/src/platform/webusb.ts` (`WEBUSB_PROTECTED_INTERFACE_CLASSES`)

These lists may differ slightly by Chromium version. When in doubt, verify on the target
browser via `chrome://usb-internals` and keep the repo’s classifier in sync.

For the canonical documentation of Chromium’s protected interface classes (and Aero’s
compatibility guidance), see [`docs/webusb.md`](./webusb.md#1-chromium-protected-interface-classes).

---

## Testing plan

### Web UI smoke panel (manual)

Use the Web UI:

- WebUSB in-app diagnostics panel: `web/src/usb/webusb_panel.ts` (rendered from `web/src/main.ts`)
- WebUSB standalone diagnostics page: `/webusb_diagnostics.html` (`web/src/webusb_diagnostics.ts`)
- WebUSB passthrough broker panel: `web/src/usb/usb_broker_panel.ts` (rendered from `web/src/main.ts`)
- WebUSB passthrough demo panel (IO worker): `web/src/main.ts` (`renderWebUsbPassthroughDemoWorkerPanel`)
- WebUSB UHCI harness panel (IO worker): `web/src/main.ts` (`renderWebUsbUhciHarnessWorkerPanel`)
- WebUSB EHCI harness panel (IO worker): `web/src/main.ts` (`renderWebUsbEhciHarnessWorkerPanel`)

These panels cover: device selection (`requestDevice`), open/claim failures, protected interface
behavior, and basic `GET_DESCRIPTOR` control transfer smoke tests (including via `usb.demo.run`).
For configuration descriptors, the demo panel can optionally rerun the transfer using the
descriptor’s `wTotalLength` field to request the full blob.

### Rust/TypeScript unit tests (automated)

The Rust passthrough device model has unit tests in `crates/aero-usb/src/passthrough.rs` that validate:

- control-IN/OUT emits exactly one `UsbHostAction` and returns NAK while in flight
- bulk IN/OUT emits one action per endpoint while in flight (no duplicates) and completes on
  injected `UsbHostCompletion`

That file also includes a serde round-trip test to ensure the canonical wire fixture
(`docs/fixtures/webusb_passthrough_wire.json`) matches the `UsbHostAction`/`UsbHostCompletion` shapes.

The TypeScript side has unit tests for the WebUSB executor/broker under `web/src/usb/*test.ts`.

---

## Related docs

- [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md) — overall physical device passthrough model (WebHID MVP + experimental WebUSB passthrough)
- [`docs/webusb.md`](./webusb.md) — WebUSB troubleshooting (permissions, WinUSB, udev, etc.)
