# WebHID/WebUSB passthrough (physical device access)

This document describes the intended architecture and security model for
passing a **real, host-connected device** through a browser page into the guest
VM as a USB peripheral.

> Source of truth: [ADR 0015](./adr/0015-canonical-usb-stack.md) defines the canonical USB stack
> selection for the browser runtime (`aero-usb` + `aero-wasm` + `web/`).

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

### WebUSB (experimental)

WebUSB is a general USB API that can expose non-HID devices and arbitrary USB
transfers (control/bulk/interrupt, multiple interfaces, etc).

It is out of scope for the first implementation because it requires a much
larger passthrough bridge:

- endpoint scheduling and transfer completion semantics
- error mapping, timeouts, cancellation
- device reset/configuration state tracking

Implementation status note:

- The repo already contains early scaffolding for WebUSB passthrough:
  - Rust-side `UsbPassthroughDevice` (`crates/aero-usb/src/passthrough.rs`), exported to JS as
    `UsbPassthroughBridge` (`crates/aero-wasm/src/lib.rs`)
  - host-side WebUSB broker/executor + RPC (`web/src/usb/*`)
  - descriptor fixups (where possible)
  - a small end-to-end demo driver (`UsbPassthroughDemo` + `usb.demoResult`) that queues
    GET_DESCRIPTOR requests via the broker to validate the action↔completion wiring (rerun via
    `usb.demo.run` in the Web UI)
    (`crates/aero-wasm/src/lib.rs`, `web/src/usb/usb_passthrough_demo_runtime.ts`, `web/src/main.ts`)
  - Guest-visible WebUSB passthrough is now wired through the canonical UHCI controller
    (`UhciControllerBridge`) with a passthrough device attached on **UHCI root port 1**.
    The I/O worker hotplugs it based on `usb.selected` broadcasts from the main-thread broker.
  - …but it is still not treated as the MVP path compared to WebHID.

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

## Current status

The repo has the core building blocks for passthrough, and the “main thread owns
`HIDDevice`, I/O worker owns the USB device model” split is wired end-to-end for
**main↔worker report forwarding** in the web runtime via a small broker/proxy protocol.

Already implemented:

- **Rust device models (`aero-usb`)**
  - `UsbHidPassthrough` (generic USB HID device with bounded input/output report queues)
  - WebHID metadata → HID report descriptor synthesis (`aero_usb::hid::webhid`)
- **WASM exports (`aero-wasm`)**
  - `WebHidPassthroughBridge` (wraps `UsbHidPassthrough` for JS/WASM interop)
  - `UsbPassthroughBridge` (wraps `UsbPassthroughDevice` for WebUSB host action/completion RPC)
- **Main-thread WebHID UX / bookkeeping (TypeScript)**
  - `WebHidPassthroughManager` + the debug panel UI
- **Main-thread ↔ I/O worker WebHID broker (TypeScript)**
  - `WebHidBroker` (`web/src/hid/webhid_broker.ts`) + protocol (`web/src/hid/hid_proxy_protocol.ts`)
    forward report traffic:
    - Preferred fast path (when `crossOriginIsolated`): SharedArrayBuffer ring buffers negotiated by
      `hid.ringAttach` (see [Forwarding mechanism](#forwarding-mechanism)).
    - Fallback/legacy path: `postMessage` forwarding (`hid.inputReport` / `hid.sendReport`).
- **Worker-side WASM bridge (TypeScript)**
  - `web/src/workers/io.worker.ts` creates a WASM `WebHidPassthroughBridge` per attached device and
    drains output reports back to the broker.
- **Guest-visible UHCI controller + topology wiring (TypeScript + WASM)**
  - `web/src/io/devices/uhci.ts` exposes a guest-visible UHCI PCI function backed by the WASM
    `UhciControllerBridge` export.
  - `web/src/hid/uhci_hid_topology.ts` wires WebHID passthrough bridges into the UHCI USB topology
    (including attaching an external hub when a `guestPath` requires it).

Dev-only scaffolding (useful for tests / manual bring-up, but **not** the target architecture):

- `WebHidPassthroughRuntime` runs on the **main thread** and directly wires `HIDDevice` events into
  a WASM `WebHidPassthroughBridge` instance (bypasses the broker/worker split).

Still missing / in progress (guest-visible USB integration):

- Snapshot/restore integration for passthrough device state (queued reports, USB configuration
  state, etc).

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

In the current TypeScript runtime this role is split between:

- `WebHidPassthroughManager` (`web/src/platform/webhid_passthrough.ts`) for user-driven selection,
  open/close lifecycle, and guest-path allocation bookkeeping.
- `WebHidBroker` (`web/src/hid/webhid_broker.ts`) for attaching to the I/O worker port and proxying
  input/output report traffic.

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

The WebHID handle is main-thread-only, so input/output report traffic is forwarded across the
main-thread ↔ worker boundary using one of two mechanisms (selected at runtime):

#### Default / legacy path: `postMessage` + transferred `ArrayBuffer`s

When SharedArrayBuffer is unavailable, forwarding uses `postMessage` with typed payloads and
transfers the underlying `ArrayBuffer` for report bytes (so the common case is zero-copy), e.g.
`{ type: "hid.inputReport", deviceId, reportId, data: Uint8Array }`.

Protocol schema + validators:

- `web/src/hid/hid_proxy_protocol.ts` (`hid.inputReport`, `hid.sendReport`)

#### Fast path: SharedArrayBuffer ring buffers (`hid.ringAttach`)

When `globalThis.crossOriginIsolated === true` (COOP/COEP enabled) and `SharedArrayBuffer`/`Atomics`
are available, `WebHidBroker` allocates two SharedArrayBuffers and sends them to the worker via
`{ type: "hid.ringAttach", inputRing, outputRing }`:

- **`inputRing` (main thread → worker):**
  - main thread writes WebHID `inputreport` events into the ring as `(deviceId, reportId, bytes)`.
  - the I/O worker drains the ring once per tick and forwards records into the guest-side HID device
    model (`push_input_report` in the WASM bridge).
- **`outputRing` (worker → main thread):**
  - the I/O worker writes output/feature report requests into the ring as
    `(deviceId, reportType, reportId, bytes)`.
  - the main thread periodically drains the ring and executes the corresponding WebHID call
    (`device.sendReport(...)` / `device.sendFeatureReport(...)`).

The ring implementation is a bounded, single-producer/single-consumer, variable-length record ring
buffer with an Atomics-managed control header; it is designed to avoid per-report allocations and
reduce `postMessage` overhead on high-frequency devices.

Implementation pointers:

- Ring buffer: `web/src/usb/hid_report_ring.ts` (`HidReportRing`)
- Message schema: `web/src/hid/hid_proxy_protocol.ts` (`hid.ringAttach`)
- Main thread setup + drain:
  - `web/src/hid/webhid_broker.ts` (`#attachRings`, `#drainOutputRing`, input-report forwarding)
- Worker-side attach + drain:
  - `web/src/workers/io.worker.ts` (`attachHidRings`, `drainHidInputRing`, `hidHostSink.sendReport`)

Note: `SharedArrayBuffer` requires cross-origin isolation (COOP/COEP) in modern browsers. When the
page is not `crossOriginIsolated`, the runtime automatically falls back to the `postMessage` path.
See [`docs/11-browser-apis.md`](./11-browser-apis.md).

## Guest-side model (UHCI + generic HID passthrough device)

### Emulated topology

The guest-visible device is modeled as:

- **UHCI host controller** (USB 1.1)
- **root hub** (2 ports)
- (optional) **external USB hub device** (USB class `0x09`) attached behind a root port to
  provide additional downstream ports
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

- There is no **portable** JS API to revoke permission across browsers.
- Some Chromium builds expose `HIDDevice.forget()` / `USBDevice.forget()` which can
  revoke the permission in-app when available.
- Otherwise, the user must revoke via browser UI (site settings / device permissions).
  The app should provide a help link/instructions in its settings panel and keep
  the fallback UX available even when `forget()` is supported.

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
  - The browser/WASM USB stack includes an external USB hub device model (`UsbHubDevice`, USB class
    `0x09`) that can be attached behind a root port to expose additional downstream ports.
    - Implementation: `crates/aero-usb/src/hub/device.rs`
    - UHCI integration tests: `crates/aero-usb/tests/uhci_external_hub.rs`
  - Current host-side WebHID UI assumes an external hub is attached on UHCI root port 0 and allocates
    passthrough devices behind it using guest paths like `0.3`.
    - UHCI root port 1 is reserved for the guest-visible WebUSB passthrough device.
    - Implementation: `web/src/platform/webhid_passthrough.ts` (guest path allocator + UI hint)
- **No low-speed modeling**
  - Low-speed (1.5 Mbps) USB devices are not modeled correctly yet.
  - Expect some HID peripherals to fail enumeration or behave incorrectly.
- **Guest-visible WebUSB passthrough is experimental**
  - The canonical UHCI controller (`UhciControllerBridge`) exposes a guest-visible WebUSB passthrough
    device on root port 1.
  - The I/O worker runs `WebUsbPassthroughRuntime` to proxy host actions/completions between the
    WASM device model and the main-thread WebUSB broker.
  - WebUSB cannot access many common USB classes in Chromium (protected interface classes), so it is
    not a replacement for WebHID for HID peripherals.
  - See:
    - [`docs/webusb-passthrough.md`](./webusb-passthrough.md)
    - [`docs/webusb.md`](./webusb.md)

## Testing strategy

### Browser-side (TypeScript)

- Tests should use a mocked `navigator.hid` + fake `HIDDevice` objects to cover:
  - attach/detach lifecycle (`open()`/`close()`) and disconnect handling
  - report forwarding semantics and output/feature report execution
- Implementation references:
  - `web/src/platform/webhid_passthrough.test.ts` (manager + debug UI)
  - `web/src/hid/webhid_broker.test.ts` (main↔worker report forwarding)
  - `web/src/hid/hid_proxy_protocol.test.ts` (message schema validators)
  - `web/src/usb/webhid_passthrough_runtime.test.ts` (dev-only main-thread runtime wiring)

### Device model (Rust)

- Unit tests for the passthrough HID device model:
  - descriptor generation (stable and spec-compliant enough for Win7)
  - input/output queue behavior (ordering, boundedness, snapshotability)
  - translation between WebHID report IDs and guest-visible USB transfers
- Implementation references:
  - `crates/aero-usb/src/hid/passthrough.rs`
  - `crates/aero-usb/src/hid/webhid.rs`
  - `crates/aero-usb/tests/webhid_passthrough.rs`

## Implementation references (current code)

- **WASM exports (browser build)**
  - `crates/aero-wasm/src/lib.rs`
    - `WebHidPassthroughBridge`
    - `UsbPassthroughBridge`
    - `UhciControllerBridge` (guest-visible UHCI controller; also exposes the WebUSB passthrough device lifecycle)
- **Rust device models**
  - WebHID → HID report descriptor synthesis: `crates/aero-usb/src/hid/webhid.rs`
  - Generic USB HID passthrough device model: `crates/aero-usb/src/hid/passthrough.rs`
  - WebUSB host action/completion queue: `crates/aero-usb/src/passthrough.rs` (`UsbPassthroughDevice`)
- **Host-side (TypeScript)**
  - WebHID attach/detach + debug UI: `web/src/platform/webhid_passthrough.ts`
  - Main↔worker report proxying broker: `web/src/hid/webhid_broker.ts`
  - Main↔worker report proxying protocol: `web/src/hid/hid_proxy_protocol.ts`
  - SharedArrayBuffer report ring: `web/src/usb/hid_report_ring.ts`
  - Guest USB attachment path schema (UHCI root port + downstream hub ports): `web/src/platform/hid_passthrough_protocol.ts`
  - WebHID normalization (input to descriptor synthesis): `web/src/hid/webhid_normalize.ts`
  - Dev-only main-thread runtime wiring WebHID ↔ WASM bridge: `web/src/usb/webhid_passthrough_runtime.ts`
  - I/O worker wiring point (guest-visible UHCI controller + USB topology + passthrough runtimes):
    `web/src/workers/io.worker.ts`

## Related docs

- [`docs/08-input-devices.md`](./08-input-devices.md) — overall input strategy
- [`docs/usb-hid.md`](./usb-hid.md) — HID usages and report formats
- [`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md) — WebHID metadata → HID report descriptor bytes
- [`docs/webusb.md`](./webusb.md) — WebUSB constraints and troubleshooting
- [`docs/webusb-passthrough.md`](./webusb-passthrough.md) — WebUSB async passthrough design (UHCI + host actions/completions)
