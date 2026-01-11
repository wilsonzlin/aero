# WebUSB passthrough (browser → guest UHCI) architecture

This document describes the planned **USB passthrough** path where a *real* USB device
is exposed to the guest OS using **WebUSB** in the browser.

The goal is to keep three moving parts coherent and spec-aligned:

- **UHCI** emulation (guest USB host controller; synchronous, TD-driven)
- **Rust device model** (`UsbPassthroughDevice`; runs inside WASM/worker)
- **TypeScript WebUSB broker/executor** (runs where WebUSB is available; usually main thread)

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
   streaks on bulk/control status stages).

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

Suggested shapes (names are important; exact fields can evolve):

```rust
enum UsbHostAction {
  ControlIn { id: u64, setup: [u8; 8], length: u16 },
  ControlOut { id: u64, setup: [u8; 8], data: Vec<u8> },
  BulkIn { id: u64, endpoint: u8, length: u16 },
  BulkOut { id: u64, endpoint: u8, data: Vec<u8> },
  // (Optional) ClaimInterface, SetConfiguration, ClearHalt, Reset, ...
}

enum UsbHostCompletion {
  ControlIn { id: u64, status: UsbStatus, data: Vec<u8> },
  ControlOut { id: u64, status: UsbStatus },
  BulkIn { id: u64, status: UsbStatus, data: Vec<u8> },
  BulkOut { id: u64, status: UsbStatus, bytes_written: u32 },
}
```

The `id` correlates an action with its completion and is also how we prevent duplicate
WebUSB calls when the guest retries a NAKed TD.

### Layer 2 (host/TS): WebUSB executor + broker (main thread)

The host side owns the actual `USBDevice` handle and performs WebUSB calls:

- **Executor**: receives `UsbHostAction`, runs the corresponding WebUSB operation, and
  sends back `UsbHostCompletion`.
- **Broker**: deals with UI and lifecycle concerns:
  - `navigator.usb.requestDevice()` (must be triggered by user activation)
  - (re)open/select configuration/claim interfaces
  - handling `disconnect` events and surfacing errors to UI

In this repo, the TypeScript-side action/completion contract and executor live in:

- `web/src/usb/webusb_backend.ts`

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

Recommended mapping:

- **SETUP TD**
  - Parse and validate the 8-byte setup packet.
  - Create a control-transfer context keyed by the UHCI queue/TD identity.
  - Emit exactly one `UsbHostAction::{ControlIn,ControlOut}` for the *whole* transfer.
    - For Control-OUT, the payload can be obtained by walking the subsequent DATA TDs
      and copying guest memory (the driver must have already populated it).
  - Complete the SETUP TD normally (ACK) so the guest can advance the chain.

- **DATA TD(s)**
  - If the corresponding WebUSB action is still in flight: return **NAK** and keep the TD
    active (no new host action emitted).
  - Once the completion arrives:
    - Control-IN: copy returned bytes into the guest buffers, completing each DATA TD with
      the actual length for that TD.
    - Control-OUT: complete DATA TDs as successful (the bytes were already sourced from
      guest memory for the host action).

- **STATUS TD**
  - Treat STATUS as the guest-visible completion point for the whole control transfer.
  - If the WebUSB completion is not available yet: return **NAK** and keep STATUS active.
  - Once completion is available: complete STATUS with success or STALL/ERR.

This keeps the guest’s mental model intact: SETUP succeeds quickly; DATA/STATUS may NAK
until the device/host stack completes the request.

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

- **Bulk OUT TD** → `UsbHostAction::BulkOut { endpoint, data }` → `USBDevice.transferOut(...)`
- **Bulk IN TD** → `UsbHostAction::BulkIn { endpoint, length }` → `USBDevice.transferIn(...)`

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

The passthrough bridge normalizes this into a small set of guest-visible outcomes:

| Host-side outcome | Guest-side handshake | Notes |
|---|---|---|
| Transfer result `"ok"` | `ACK` | Complete TD, write returned bytes for IN transfers. |
| Transfer result `"stall"` | `STALL` | Complete TD with STALLED; guest driver is responsible for recovery (e.g. CLEAR_FEATURE HALT). |
| Transfer result `"babble"` (if surfaced) | `TIMEOUT` (or controller error) | Aero currently does not model babble distinctly in `UsbHandshake`; treat as an error until a richer mapping exists. |
| Thrown exception / other error | `TIMEOUT` | Most non-stall failures are best surfaced as a retryable error/timeout from the guest’s point of view. |
| No completion yet (Promise pending / action in-flight) | `NAK` | Keep TD active so the UHCI schedule naturally retries. |

Implementation note: Aero’s UHCI model already has first-class NAK semantics:

- `UsbHandshake::Nak` sets the TD’s NAK bit and leaves it active, so the same TD will be retried.
- This is the intended mechanism for “WebUSB transfer pending” without blocking the worker.

In code, see `crates/aero-usb/src/uhci.rs` (`UsbHandshake::Nak` branch) and the handshake enum in
`crates/aero-usb/src/usb.rs`.

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

---

## Worker and threading constraints (browser reality)

### User activation is required for device selection

`navigator.usb.requestDevice()` must be called from a **user-activated** event handler
(e.g. button click). This forces a “broker” role on the UI layer:

- the main thread is responsible for prompting and persisting the selected device,
- the emulator worker cannot autonomously attach arbitrary devices.

### `USBDevice` is likely non-transferable

In practice, `USBDevice` should be treated as **non-structured-cloneable** and therefore
non-transferable to workers. Even if some browser versions eventually allow worker access,
this should not be relied upon for the core architecture.

### Recommended architecture when WebUSB is unavailable in workers

Assume WebUSB calls must run on the main thread:

- **Worker (WASM):** UHCI + `UsbPassthroughDevice` emits actions via a queue/ring buffer.
- **Main thread:** broker/executor receives actions, calls WebUSB, and returns completions.

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

Use the existing WebUSB debug panel as a starting point (`web/src/usb/webusb_panel.ts`), which:

- prompts for a device (`requestDevice`)
- runs a simple control transfer (`GET_DESCRIPTOR(Device)`) and prints the raw bytes
- can run a simple control transfer (e.g. `GET_DESCRIPTOR`, vendor request) and a bulk
  transfer (if endpoints exist), showing status/bytes
- shows broker/executor state (open/claimed interfaces, disconnect events)

This is primarily for debugging permission issues, protected interfaces, and descriptor
translation.

### Emulator unit tests (automated)

Add unit tests around `UsbPassthroughDevice` + UHCI transaction handling for:

1. **Pending control transfer behavior**
   - Setup TD emits exactly one `UsbHostAction`
   - Data/Status TDs return NAK while the action is in flight
   - Completion unblocks the chain and fills buffers for Control-IN
2. **Bulk TD behavior**
   - one action per TD (packet granularity)
   - NAK while pending without duplicate actions
   - short packet handling sets actual length correctly

These tests should run without a real USB device by injecting synthetic `UsbHostCompletion`
events.

---

## Related docs

- [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md) — overall physical device passthrough model (WebHID MVP + WebUSB future)
- [`docs/webusb.md`](./webusb.md) — WebUSB troubleshooting (permissions, WinUSB, udev, etc.)
