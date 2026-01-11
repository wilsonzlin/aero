# WebUSB passthrough (browser → guest UHCI) architecture

This document describes the **USB passthrough** path where a *real* USB device
is exposed to the guest OS using **WebUSB** in the browser.

The goal is to keep three moving parts coherent and spec-aligned:

- **UHCI** emulation (guest USB host controller; synchronous, TD-driven)
- **Rust device model** (`UsbPassthroughDevice`; runs inside WASM/worker)
- **TypeScript WebUSB broker/executor** (runs where WebUSB is available; usually main thread)

Implementation references (current repo):

- Rust device model + host-action protocol: `crates/emulator/src/io/usb/passthrough.rs`
- TS main-thread broker + worker client RPC: `src/platform/webusb_{broker,client,protocol}.ts`

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

Current shapes (see `crates/emulator/src/io/usb/passthrough.rs`):

```rust
enum UsbHostAction {
  ControlIn { id: u64, setup: SetupPacket, len: u16 },
  ControlOut { id: u64, setup: SetupPacket, data: Vec<u8> },
  BulkIn { id: u64, ep: u8, len: usize },
  BulkOut { id: u64, ep: u8, data: Vec<u8> },
}

enum UsbHostResult {
  OkIn { data: Vec<u8> },
  OkOut { bytes_written: usize },
  Stall,
  Timeout,
  Error(String),
}

enum UsbHostCompletion {
  Completed { id: u64, result: UsbHostResult },
}
```

The `id` correlates an action with its completion and is also how we prevent duplicate
WebUSB calls when the guest retries a NAKed TD.

Cancellation behavior (important):

- A new control SETUP can legally abort a previous control transfer. The passthrough model may
  drop the previous in-flight request and ignore its eventual completion (WebUSB does not provide
  strong cancellation for an in-flight transfer).
- If the host has not yet dequeued the old `UsbHostAction`, the model may also remove it from the
  action queue so the host does not execute a stale transfer.
- Stale completions are therefore expected and must be safely ignored (the Rust model already
  does this by checking `id` against in-flight state).

In Aero, this cancellation is triggered by the UHCI control pipe: when a new SETUP arrives before
the previous control transfer completes, `AttachedUsbDevice` invokes the device model hook
`UsbDeviceModel::cancel_control_transfer()` (`crates/emulator/src/io/usb/core/mod.rs`).

Descriptor status (current code):

- `UsbPassthroughDevice` currently returns empty device/config/HID descriptors. Full passthrough
  enumeration requires synthesizing guest-visible descriptors from the physical device (including
  `OTHER_SPEED_CONFIGURATION` fixups for UHCI/full-speed).

### Layer 2 (host/TS): WebUSB executor + broker (main thread)

The host side owns the actual `USBDevice` handle and performs WebUSB calls:

- **Executor**: receives `UsbHostAction`, runs the corresponding WebUSB operation, and
  sends back `UsbHostCompletion`.
- **Broker**: deals with UI and lifecycle concerns:
  - `navigator.usb.requestDevice()` (must be triggered by user activation)
  - (re)open/select configuration/claim interfaces
  - handling `disconnect` events and surfacing errors to UI

In this repo, the browser-side WebUSB integration is split into:

- **Main thread broker** (owns `USBDevice` handles): `src/platform/webusb_broker.ts`
- **Worker client** (RPC stub used from workers): `src/platform/webusb_client.ts`
- **Typed request/response protocol**: `src/platform/webusb_protocol.ts`

The broker attaches a `MessagePort` to a worker, and the worker uses `WebUsbClient`
to perform WebUSB operations without requiring `USBDevice` transferability.

Note: `src/` is the repo-root Vite harness entrypoint used for debugging/tests; the production
browser host lives under `web/` (ADR 0001). The broker/client pattern is still the recommended
shape for production when WebUSB work must be serviced on the main thread.

There is also a smaller “direct executor” implementation under the production host tree:

- `web/src/usb/webusb_backend.ts`

### Device lifecycle: open/configuration/interface claiming

WebUSB requires the browser process to:

1. `device.open()`
2. `device.selectConfiguration(...)` (if no active configuration)
3. `device.claimInterface(...)` for any interface whose endpoints will be used

In this repo, these operations are exposed to workers via the `WebUsbBroker`/`WebUsbClient`
protocol (`src/platform/webusb_protocol.ts`). The current RPC surface includes:

- open/close
- select configuration
- claim/release interface
- reset
- controlTransferIn/controlTransferOut
- transferIn/transferOut

If the guest requires alternate interface settings or endpoint-halt recovery, extend the
protocol to cover `selectAlternateInterface` / `clearHalt` as needed.

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
interface” actions; today these operations are expected to be performed by the host-side broker
when attaching a physical device.

### Physical disconnect / guest hot-unplug

When the physical device is unplugged, the browser fires a `navigator.usb` `"disconnect"` event.
For guest correctness, this must be reflected as a **USB disconnect** on the emulated root hub
port so the guest OS can tear down drivers cleanly.

In this repo:

- `src/platform/webusb_broker.ts` listens for `usb.addEventListener('disconnect', ...)` and broadcasts
  `{ type: 'disconnect', deviceId }` events to attached worker ports.
- `src/platform/webusb_client.ts` exposes `onBrokerEvent(...)` for workers to subscribe.
- The UHCI root hub supports detach via `RootHub::detach(port)` (`crates/emulator/src/io/usb/hub/root.rs`),
  which sets the connect-status-change bits the guest driver expects.

Recommended behavior on a physical disconnect:

1. Detach the passthrough device model from the associated emulated port (root hub or downstream hub).
2. Cancel any in-flight host actions (by dropping the device model or calling `reset()` / cancel hooks).
3. Ignore any completions that arrive after detach (they will be stale by `id` anyway).

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

Aero mapping (current code):

- **SETUP TD**
  - Decoded into a `SetupPacket` by the UHCI scheduler (`crates/emulator/src/io/usb/uhci/schedule.rs`).
  - Routed to the endpoint-0 control pipe state machine (`AttachedUsbDevice` in
    `crates/emulator/src/io/usb/core/mod.rs`).
  - For **Control-IN** requests (Device→Host), `AttachedUsbDevice::handle_setup` calls the device
    model’s `handle_control_request(setup, None)` immediately:
    - `ControlResponse::Nak` → SETUP TD **ACKs** and the control pipe enters a pending state. The
      subsequent **DATA (IN)** stage is NAKed until the asynchronous host completion arrives (this
      is how “WebUSB Promise pending” is represented).
    - `ControlResponse::Data(bytes)` → SETUP TD ACKs and the bytes become the source for subsequent
      IN DATA TDs (including `bytes=[]` which results in a zero-length DATA packet / ZLP when
      `wLength > 0`).
    - `ControlResponse::Ack` → SETUP TD ACKs. If `wLength > 0`, the control pipe still completes a
      one-shot **DATA (IN)** stage with a zero-length packet (ZLP), then proceeds to STATUS.
      Otherwise (`wLength == 0`) it skips directly to STATUS.
    - `ControlResponse::Stall` → SETUP TD stalls.
  - For **Control-OUT** requests with `wLength > 0`, SETUP TD ACKs and the control pipe transitions to
    “collect OUT data bytes”.
  - **`SET_ADDRESS` virtualization:** the control pipe intercepts the standard `SET_ADDRESS` request
    and updates only the guest-visible address state. This request must **not** be forwarded to the
    physical device (the host OS already enumerated it).
    - If a new SETUP arrives before the `SET_ADDRESS` status stage completes, the pending address is
      discarded (matching USB semantics: a new SETUP aborts the previous control transfer).

Note: on real USB, devices must **ACK SETUP** transactions; NAK is not a valid handshake for the
SETUP stage. Aero’s control pipe therefore always ACKs the SETUP TD and expresses “pending” via
NAK on the DATA/STATUS TDs until the asynchronous host completion arrives.

- **DATA TD(s)**
  - **Control-IN:** IN DATA TDs read from the already-buffered `Data(bytes)` returned by
    `handle_control_request` and are chunked to each TD’s `max_len`.
    - If the control pipe is still waiting on an async completion, **IN DATA TDs are NAKed**
      (the SETUP TD is not retried).
  - **Control-OUT:** OUT DATA TDs append into an internal buffer until `wLength` bytes are received.
    Once complete, the device model is called exactly once:
    - `handle_control_request(setup, Some(data))`
    - If it returns `Nak`, the *final* OUT DATA TD **ACKs** (payload already buffered) and the
      control pipe represents “still waiting” by NAKing the **STATUS (IN)** stage until completion.

- **STATUS TD**
  - Driven entirely by the control-pipe state machine (zero-length IN for Control-OUT, or zero-length
    OUT for Control-IN).
  - When a control transfer is pending (`ControlResponse::Nak`), STATUS TDs are where the NAK
    backpressure is applied for:
    - Control-IN requests with `wLength == 0` (NAK the STATUS OUT stage), and
    - Control-OUT requests (NAK the STATUS IN stage).
  - This stage may call back into the device model to poll for completion.

Net effect: control requests are emitted as **one host action per request**, and NAK is used to keep
the relevant TD active until the asynchronous host completion arrives.

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

Mapping:

- **Bulk OUT TD** → `UsbHostAction::BulkOut { ep, data }` → `USBDevice.transferOut(ep & 0x0f, ...)`
- **Bulk IN TD** → `UsbHostAction::BulkIn { ep, len }` → `USBDevice.transferIn(ep & 0x0f, ...)`

(`ep` here is the USB endpoint *address* as seen by UHCI: bit 7 is direction and bits 0–3 are the
endpoint number. WebUSB takes the endpoint number separately.)

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

The WebUSB host integration should normalize those outcomes into `UsbHostResult` values, then send
`UsbHostCompletion::Completed { id, result }` back to the Rust device model.

Guest-visible behavior is then derived from the Rust mapping in
`crates/emulator/src/io/usb/passthrough.rs`:

| Host-side result (`UsbHostResult`) | Guest-visible outcome | Notes |
|---|---|---|
| `OkIn { data }` | `DATA` (for IN TDs) | Data is truncated to `wLength` for control-IN and to the TD `max_len` for bulk IN. |
| `OkOut { .. }` | `ACK` | OUT TD completes successfully. |
| `Stall` | `STALL` | TD completes with STALLED; guest driver is responsible for recovery. |
| `Timeout` | `STALL` (current behavior) | Passthrough currently treats timeouts as fatal to unblock the guest. |
| `Error(_)` | `STALL` (current behavior) | Same as timeout. Consider a richer mapping later (e.g. CRC/timeout bit vs stall). |
| (no completion yet; action in-flight) | `NAK` | Keep TD active so the UHCI schedule naturally retries without duplicating host work. |

Implementation note: in the emulator UHCI scheduler, `Nak` is a first-class “retry later” outcome:

- `UsbInResult::Nak` / `UsbOutResult::Nak` set the TD NAK bit and keep the TD active
  (`crates/emulator/src/io/usb/uhci/schedule.rs`).
- For control transfers, `ControlResponse::Nak` propagates to a NAK on the relevant control TD via the
  endpoint-0 state machine (`crates/emulator/src/io/usb/core/mod.rs`).

---

## Speed and descriptor handling (UHCI full-speed view)

### Constraint: UHCI is full-speed

For the passthrough MVP, assume the guest controller is **UHCI** and therefore operates
as a **full-speed** host from the guest’s perspective.

This creates a mismatch when the physical device is high-speed (USB 2.0) on the real
machine: we must present a **full-speed-compatible configuration** to the guest so it
chooses correct max packet sizes and intervals.

### Using `OTHER_SPEED_CONFIGURATION` to synthesize a full-speed configuration

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
3. If not present:
   - Fall back to the regular `CONFIGURATION` descriptor (device is likely full-speed-only),
     or reject passthrough if the device cannot sensibly operate under a full-speed host.

Practical implications:

- Devices that are **high-speed-only** without a usable other-speed configuration are not
  good candidates for UHCI passthrough; they likely require EHCI/xHCI emulation.
- Even for high-speed devices, sending smaller full-speed-sized transfers via WebUSB is
  typically valid (it is legal to transfer less than max packet size), but correctness
  depends on descriptors matching what the guest believes.

In this repo, the minimal “rewrite OTHER_SPEED_CONFIGURATION → CONFIGURATION” helper is implemented
as `other_speed_config_to_config` in `crates/emulator/src/io/usb/descriptor_fixups.rs`.

---

## Worker and threading constraints (browser reality)

### User activation is required for device selection

`navigator.usb.requestDevice()` must be called from a **user-activated** event handler
(e.g. button click). This forces a “broker” role on the UI layer:

- the main thread is responsible for prompting and persisting the selected device,
- the emulator worker cannot autonomously attach arbitrary devices.

In the repo’s WebUSB broker implementation, this is enforced via `navigator.userActivation`
(`src/platform/webusb_broker.ts`).

### `USBDevice` is likely non-transferable

In practice, `USBDevice` should be treated as **non-structured-cloneable** and therefore
non-transferable to workers. Even if some browser versions eventually allow worker access,
this should not be relied upon for the core architecture.

The Vite harness includes a probe that attempts both structured cloning and transfer-list
transfer of a `USBDevice` to validate this assumption (`src/main.ts`).

### Recommended architecture when WebUSB is unavailable in workers

Assume WebUSB calls must run on the main thread:

- **Worker (WASM):** UHCI + `UsbPassthroughDevice` emits actions via a queue/ring buffer.
- **Main thread:** broker/executor receives actions, calls WebUSB, and returns completions.

This pattern is implemented by `WebUsbBroker` (main thread) + `WebUsbClient` (worker) using
a `MessagePort` RPC protocol (`src/platform/webusb_{broker,client,protocol}.ts`).

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

### TODO: Protected interface class list (confirm and cite)

TODO: Confirm the exact “protected interface class” policy used by the Chromium version we
target (including whether any subclass/protocol combinations are treated specially), and
record:

- the Chromium source reference (file path / CL) and last-verified Chrome/Edge version
- the final canonical list (class/subclass/protocol) used by Aero for compatibility docs

---

## Testing plan

### Web UI smoke panel (manual)

Use the repo-root Vite harness WebUSB panel (`src/main.ts`), which:

- prompts for a device (`requestDevice`)
- probes worker-side WebUSB availability and (non-)transferability of `USBDevice`
- demonstrates broker-backed worker I/O via `WebUsbBroker`/`WebUsbClient`
- runs a simple control transfer (`GET_DESCRIPTOR(Device)`) and prints the raw bytes
- (recommended extension) add buttons for `transferIn`/`transferOut` so bulk endpoints can be smoke-tested too

This is primarily for debugging permission issues, protected interfaces, and descriptor
translation.

### Emulator unit tests (automated)

The Rust passthrough device model already has unit tests in
`crates/emulator/src/io/usb/passthrough.rs` that validate:

- control-IN/OUT emits exactly one `UsbHostAction` and returns NAK while in flight
- bulk IN/OUT emits one action per endpoint while in flight (no duplicates) and completes on
  injected `UsbHostCompletion`

Extend these tests as the descriptor model and more error mapping is implemented.

---

## Related docs

- [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md) — overall physical device passthrough model (WebHID MVP + WebUSB future)
- [`docs/webusb.md`](./webusb.md) — WebUSB troubleshooting (permissions, WinUSB, udev, etc.)
