# WebHID/WebUSB passthrough (physical device access)

This document describes the intended architecture and security model for
passing a **real, host-connected device** through a browser page into the guest
VM as a USB peripheral.

The goal is to support “real hardware” use cases (game controllers, specialty
HID devices, etc) without turning Aero into a native app.

## Scope: WebHID vs WebUSB

### WebHID (MVP)

WebHID is a browser API for **HID-class** devices (Human Interface Devices).
It exposes:

- a device identity (vendor/product IDs, names)
- HID report metadata (collections, report IDs/sizes)
- the ability to receive **input reports** and send **output/feature reports**

Why WebHID is the MVP for passthrough:

- **Narrower emulation surface:** we only need a generic USB + HID device model,
  not arbitrary USB class drivers and endpoint types.
- **Good UX fit:** the browser’s chooser UI is already oriented around “input-ish”
  peripherals.
- **Safer default:** compared to raw USB access, HID is more constrained (though
  still security-sensitive; see below).

### WebUSB (future)

WebUSB is a general USB API that can expose non-HID devices and arbitrary USB
transfers (control/bulk/interrupt, multiple interfaces, etc).

It is out of scope for the first implementation because it requires a much
larger passthrough bridge:

- endpoint scheduling and transfer completion semantics
- error mapping, timeouts, cancellation
- device reset/configuration state tracking

The intended end-state is to use the same “main-thread owns the handle, worker
models the USB device” architecture described below, but with a richer transfer
bridge.

For the detailed UHCI ↔ WebUSB transfer/TD mapping (including TD-level NAK pending
semantics), see:

- [`docs/webusb-passthrough.md`](./webusb-passthrough.md)

Note: WebUSB is also a poor fit for many HID-class devices because browsers
treat some USB interface classes as “protected” and disallow access via WebUSB.
For HID peripherals, prefer WebHID.

## High-level architecture

The key constraint is that **browser device handles are main-thread objects**.

- `HIDDevice` is not structured-cloneable and cannot be transferred to a worker.
- `USBDevice` transfer/worker support is browser-dependent, but Aero’s baseline
  architecture assumes the handle stays on the main thread (user-activation and
  permission UX are tied to the Window event loop).

So the design is split:

- **Main thread (Window):** selects the physical device, opens it, and forwards
  reports/transfer requests across a host ↔ worker boundary.
- **Worker (I/O / device-model):** emulates UHCI + a USB HID device and exposes
  it to the guest OS like any other USB peripheral.

Data flow (WebHID):

```
Physical HID device
  ↕ (WebHID API: HIDDevice)
Main thread (owns the handle)
  ↕ (report forwarding)
I/O worker (USB controller + device model)
  ↕ (UHCI ports, USB transfers)
Guest Windows USB/HID stack
```

## Host-side model (main thread owns the device)

### Why the main thread owns it

- `navigator.hid.requestDevice(...)` / `navigator.usb.requestDevice(...)` must
  be called from a **user gesture** on the main thread.
- `HIDDevice` is **not structured-cloneable** and therefore cannot be sent to a
  Worker via `postMessage`.
- `USBDevice` structured clone / worker access is not reliable enough to be a
  design assumption; treat the main thread as the default owner and proxy I/O as
  needed (see [`docs/webusb-passthrough.md`](./webusb-passthrough.md)).

### Responsibilities

The main-thread “passthrough manager” is responsible for:

1. **User-initiated selection**
   - Trigger a chooser from an explicit UI action (“Connect device…”).
2. **Open/close lifecycle**
   - `await device.open()` when attaching to a VM.
   - `await device.close()` when detaching or when the VM stops.
3. **Input report forwarding**
   - Listen for `inputreport` events.
   - Forward `(reportId, data bytes, timestamp)` to the worker.
4. **Output report execution**
   - Receive worker requests to send an output/feature report.
   - Call `device.sendReport(...)` / `device.sendFeatureReport(...)`.

### Forwarding mechanism

For MVP correctness, forwarding can be `postMessage` with small typed payloads
(e.g. `{ type: 'hid:in', deviceId, reportId, data: Uint8Array }`).

For performance, move to a fixed-size shared-memory ring buffer:

- main thread writes input reports into an **input queue**
- worker writes output report requests into an **output queue**
- both sides use a small `Int32Array` control header with Atomics to signal
  availability

This keeps the device model deterministic and avoids per-report allocations on
hot paths (gamepads can be high-frequency).

Note: `SharedArrayBuffer` requires cross-origin isolation (COOP/COEP) in modern
browsers. If the app is not `crossOriginIsolated`, fall back to `postMessage`-
based forwarding. See [`docs/11-browser-apis.md`](./11-browser-apis.md).

## Guest-side model (UHCI + generic HID passthrough device)

### Emulated topology

The guest-visible device is modeled as:

- **UHCI host controller** (USB 1.1)
- **root hub** (currently limited; see “Current limitations”)
- **one generic HID device per physical passthrough device**

On attach, the worker hot-plugs the device onto an available UHCI port, which
triggers the guest USB stack to enumerate it.

### Device identity and descriptors

The passthrough HID device should expose stable USB descriptors derived from the
WebHID device metadata:

- `idVendor` / `idProduct`: from WebHID vendor/product IDs
- strings: best-effort from `productName` / `manufacturerName` if available

WebHID does **not** expose the raw HID report descriptor byte stream. It exposes
a structured view (`HIDDevice.collections`, reports, and report items), so we
synthesize a semantically equivalent HID report descriptor from that metadata.
See [`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md)
for the exact synthesis contract.

The USB device model must still provide the normal USB descriptors used during
enumeration:

- device descriptor
- configuration/interface/endpoint descriptors
- HID descriptor

### Report queues (bridge between WebHID and USB polling)

WebHID is event-driven, but the guest’s USB HID stack is poll/transfer-driven.
To connect them, the worker maintains per-device queues:

#### Input reports (device → guest)

- Main thread pushes `(reportId, bytes)` into the device’s **input report queue**.
- When the guest performs an interrupt IN transfer:
  - if a report is queued: return it as the transfer payload
  - if the queue is empty: NAK (guest will poll again)

#### Output/feature reports (guest → device)

- When the guest sends a `SET_REPORT` (control transfer) or an interrupt OUT
  transfer (device-dependent):
  - worker enqueues `(reportId, bytes, kind)` into the **output report queue**
  - main thread drains the queue and calls the appropriate WebHID send method

This queue boundary is also where we can implement:

- backpressure / bounded memory
- ordering guarantees (preserve report order)
- VM snapshot/restore (queue contents are part of device state)

## Security and UX constraints

Passing through a physical device is **powerful and risky**. The UX must make
the security boundary explicit: you are giving an **untrusted guest OS** direct
access to a real device.

### User gesture requirement (`requestDevice`)

Both APIs require a user gesture:

- WebHID: `navigator.hid.requestDevice(...)`
- WebUSB: `navigator.usb.requestDevice(...)`

Do not attempt to call these APIs automatically on page load or in response to
background events; it will fail and is also poor UX.

In practice:

- Call `requestDevice()` directly from the gesture handler; if you `await` before
  calling it, the user activation can be lost.
- User activation does not propagate across `postMessage()`, so a “click →
  postMessage → worker calls `requestDevice()`” flow will fail.

### Secure context requirement

WebHID/WebUSB require a **secure context** (`https://` or `http://localhost`).
Passthrough should be disabled (with a clear UI error) when `isSecureContext`
is false.

### Origin-scoped permission persistence and revocation

Permissions are **scoped to the web origin** and may persist across reloads.
Typical flow:

- The first time, the user grants access via the chooser UI.
- On later visits, the page can often rediscover devices via
  `navigator.hid.getDevices()` / `navigator.usb.getDevices()` without showing
  the chooser.

Security model requirement for Aero:

- Even if the origin has permission, **do not auto-attach** the device to a VM
  without an explicit user action in the UI (e.g. a “Connect” button).

Revocation:

- There is no portable JS API to revoke permission.
- The user must revoke via browser UI (site settings / device permissions).
  The app should provide a help link/instructions in its settings panel.

### Explicit warnings and safer defaults

When offering passthrough, the UI should warn that the guest can:

- read inputs from the device (potentially sensitive)
- send outputs back to the device (e.g. LEDs, vibration, device state changes)

Recommended guardrails:

- Require the user to opt in per session (“Attach to this VM”).
- Show a persistent “Device connected to VM” indicator and a one-click
  “Disconnect” action.
- Prefer allowlisting device types that make sense (game controllers, specialty
  hardware) and avoid exposing high-risk devices by default.

## Current limitations (MVP constraints)

- **UHCI root hub: 2 ports**
  - Only two devices can be attached *directly* to the root hub.
  - Supporting more simultaneous passthrough devices likely requires attaching a
    virtual USB hub to a root port and mapping physical devices behind it.
- **No low-speed modeling**
  - Low-speed (1.5 Mbps) USB devices are not modeled correctly yet.
  - Expect some HID peripherals to fail enumeration or behave incorrectly.
- **WebUSB passthrough is separate (non-HID)**
  - WebUSB passthrough uses a host action/completion bridge and is tracked
    separately from the WebHID HID-device MVP. See:
    - [`docs/webusb-passthrough.md`](./webusb-passthrough.md)
    - [`docs/webusb.md`](./webusb.md)
  - WebUSB cannot access many common USB classes in Chromium (protected interface
    classes), so it is not a replacement for WebHID for HID peripherals.

## Testing strategy

### Browser-side (TypeScript)

- Tests should use a mocked `navigator.hid` + fake `HIDDevice` objects to cover:
  - attach/detach lifecycle (`open()`/`close()`) and disconnect handling
  - (when implemented) report forwarding semantics and output report execution
- Implementation reference: `web/src/platform/webhid_passthrough.test.ts`

### Device model (Rust)

- Unit tests for the passthrough HID device model:
  - descriptor generation (stable and spec-compliant enough for Win7)
  - input/output queue behavior (ordering, boundedness, snapshotability)
  - translation between WebHID report IDs and guest-visible USB transfers
- Implementation references:
  - `crates/emulator/src/io/usb/hid/passthrough.rs`
  - `crates/emulator/tests/uhci.rs`

## Implementation references (current code)

- Host-side WebHID attach/detach and debug UI: `web/src/platform/webhid_passthrough.ts`
- WebHID normalization (input to descriptor synthesis): `web/src/hid/webhid_normalize.ts`
- WebHID → HID report descriptor synthesis (Rust): `crates/emulator/src/io/usb/hid/webhid.rs`
- Generic USB HID passthrough device model (Rust): `crates/emulator/src/io/usb/hid/passthrough.rs`

## Related docs

- [`docs/08-input-devices.md`](./08-input-devices.md) — overall input strategy
- [`docs/usb-hid.md`](./usb-hid.md) — HID usages and report formats
- [`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md) — WebHID metadata → HID report descriptor bytes
- [`docs/webusb.md`](./webusb.md) — WebUSB constraints and troubleshooting
- [`docs/webusb-passthrough.md`](./webusb-passthrough.md) — WebUSB async passthrough design (UHCI + host actions/completions)
