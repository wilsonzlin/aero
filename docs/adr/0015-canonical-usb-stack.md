# ADR 0015: Canonical USB stack (browser runtime: `aero-usb`)

## Context

The repository historically accumulated multiple overlapping USB/UHCI implementations:

- **Browser/WASM path (active):**
  - Rust USB device models + UHCI: `crates/aero-usb`
  - WASM exports: `crates/aero-wasm`
  - Host integration (WebHID/WebUSB broker/executor, worker proxying): `web/`
- **Repo-root WebUSB demo RPC (parallel TypeScript surface):**
  - Legacy/demo main-thread broker + worker client (deprecated/quarantined): `src/platform/legacy/webusb_{broker,client,protocol}.ts`
- **Native emulator integration (consumer):**
  - PCI/PortIO wiring + compatibility re-exports: `crates/emulator` (`emulator::io::usb` module)
- **Legacy prototype (duplicate wire contract; removed):**
  - Early WebUSB passthrough bridge/types previously lived in `crates/aero-wasm/src/usb_passthrough.rs`
    (now deleted; do not reintroduce).

This split makes it easy to accidentally introduce a *third* “USB stack” (new UHCI model, new
wire protocol, new TS runtime, etc.), and it forces duplicated work whenever a bug fix or feature
must be applied in two places.

### Scope

This ADR is specifically about **USB for the browser runtime** (WASM + web worker architecture).
It also clarifies how the legacy/native USB code fits into the repo going forward.

## Decision

### 1) Canonical USB stack for the browser runtime

For the in-browser runtime, the canonical stack is:

- **USB device models + UHCI (Rust, deterministic, wasm32-friendly):** `crates/aero-usb`
  - UHCI controller: `aero_usb::uhci::UhciController`
  - Guest-visible USB device models (HID, hubs, passthrough wrappers): `aero_usb::*`
- **WASM-facing exports (thin wrappers around `aero-usb`):** `crates/aero-wasm`
- **Host integration (TypeScript):** `web/src/usb/*`
  - WebUSB executor/broker (main thread): `web/src/usb/webusb_backend.ts`, `web/src/usb/usb_broker.ts`
  - Worker-side runtime pump: `web/src/usb/webusb_passthrough_runtime.ts`

Note: the canonical browser host entrypoint is the repo-root Vite app, but it imports shared runtime
modules from `web/src/*`. The `web/` directory’s own `web/index.html` entrypoint is legacy, but the
USB host integration under `web/src/usb/*` is the maintained implementation.

The browser runtime must **not** implement a parallel USB/UHCI stack in `crates/emulator` or in
TypeScript.

### 2) Canonical ownership of the WebUSB passthrough wire contract

The Rust↔TypeScript WebUSB passthrough “host action/completion” contract is owned by:

- **Rust source of truth:** `crates/aero-usb/src/passthrough.rs`
- **Cross-language fixture:** `docs/fixtures/webusb_passthrough_wire.json`
- **TypeScript mirror types:** `web/src/usb/usb_passthrough_types.ts`

Any change to the wire contract must update **all three** in a single change set and keep both the
Rust and TS tests passing.

### 3) Status of the emulator USB module (`crates/emulator`, `emulator::io::usb`)

`crates/emulator` exposes `emulator::io::usb` primarily for:

- **PCI/PortIO device integration** (UHCI as a PCI device in the native emulator)
- **Backwards-compatible import paths** for tests and callers

The underlying USB/UHCI implementation is owned by `crates/aero-usb`. The emulator should **not**
reintroduce an independent USB stack.

### 4) Where UHCI lives long-term, and how it connects to `aero_machine::Machine`

- **UHCI emulation lives in Rust** (`crates/aero-usb`) and runs inside the WASM worker that owns
   the VM state. This keeps device behavior deterministic and testable, and avoids re-implementing
   low-level scheduling/state machines in TypeScript.
- **TypeScript does not emulate UHCI.** It is responsible for host-only concerns:
  - WebUSB/WebHID handles and permission UX (user gesture requirement)
  - async execution of host transfers
  - main thread ↔ worker proxying (default: `postMessage` with transferred `ArrayBuffer`s).
    When `globalThis.crossOriginIsolated === true` and `SharedArrayBuffer`/`Atomics` are available:
    - WebHID passthrough uses SAB ring buffers to avoid per-report messaging overhead:
      - **Input reports (main → worker):** IPC `RingBuffer` initialized via `hid.ring.init` and filled
        with compact, versioned `"HIDR"` input report records.
      - **Output/feature reports (worker → main):** SPSC `HidReportRing` wired via `hid.ringAttach`.
      - Implementation:
        - `web/src/ipc/ring_buffer.ts` (`RingBuffer`)
        - `web/src/hid/hid_input_report_ring.ts` (record codec + writer)
        - `web/src/hid/hid_proxy_protocol.ts` (`hid.ring.init`, `hid.ringAttach`)
        - `web/src/hid/webhid_broker.ts` (runtime selection + producers)
        - `web/src/workers/io_hid_input_ring.ts` (worker drain helper)
    - WebUSB passthrough supports a SAB ring-buffer fast path for host action/completion proxying,
      negotiated by `usb.ringAttach` (and can be disabled via `usb.ringDetach` on ring corruption):
      - **Actions (worker → main):** `UsbHostAction` records.
      - **Completions (main → worker):** `UsbHostCompletion` records.
      - Implementation:
        - `web/src/usb/usb_proxy_ring.ts`
        - `web/src/usb/usb_proxy_protocol.ts` (`usb.ringAttach`, `usb.ringAttachRequest`, `usb.ringDetach`)
        - `web/src/usb/usb_broker.ts`
        - `web/src/usb/webusb_passthrough_runtime.ts`
        - `web/src/usb/usb_proxy_ring_dispatcher.ts` (completion-ring fan-out when multiple runtimes subscribe)
- Long-term, the UHCI controller should be integrated into the canonical VM wiring described by
  [ADR 0014](./0014-canonical-machine-stack.md):
  - `aero_machine::Machine` (in the I/O worker) owns the UHCI controller device model.
  - Passthrough devices attach to the UHCI bus inside the worker.
  - The main thread broker/executor performs WebUSB/WebHID I/O and returns completions to the
    worker via the `UsbHostAction`/`UsbHostCompletion` protocol.

## Alternatives considered

1. **Make `crates/emulator` USB the canonical implementation**
   - Pros: existing implementation surface; native-friendly.
   - Cons: not the browser runtime path; would require moving the web runtime off `aero-usb` and
     would keep the browser stack “second-class”.

2. **Keep multiple USB stacks active**
   - Pros: less short-term churn.
   - Cons: guarantees divergence (UHCI behavior, descriptor quirks, wire formats) and slows down
     progress by multiplying test and maintenance effort.

3. **Implement UHCI (or a USB scheduler) in TypeScript**
   - Pros: easier to iterate in the browser debugger.
   - Cons: splits critical device model correctness across languages; undermines determinism and
     makes Rust-side testing less meaningful.

## Consequences

- New work has a single default place to land: `crates/aero-usb` (and `web/src/usb/*` for host
  integration).
- Duplicate implementations become explicit legacy/deprecation targets, instead of “also valid”
  options.
- The WebUSB wire contract has a single owner and a cross-language fixture, reducing accidental
  drift.
- `crates/aero-usb` device models can participate in VM snapshot/restore via the repo-standard
  `aero-io-snapshot` TLV encoding (`aero_io_snapshot::io::state::IoSnapshot`). This keeps device
  state deterministic and versioned; higher-level VM snapshot wiring stores the resulting TLV blob in
  the `DEVICES` section (see `docs/16-snapshots.md`).

### Migration plan (incremental; includes deletion targets)

1. **Docs**
   - Treat this ADR as the source of truth for USB stack selection.
   - Update related docs to link to this ADR and avoid implying the legacy `crates/emulator` USB stack
     is the primary path for browser USB.

2. **Keep the legacy `aero-wasm` prototype deleted**
   - Do not reintroduce `crates/aero-wasm/src/usb_passthrough.rs` (it duplicated the passthrough
     wire contract).
   - Ensure there is exactly one `UsbPassthroughBridge` surface in `crates/aero-wasm`, backed by
     `aero_usb::passthrough::UsbPassthroughDevice`.

3. **Consolidate TypeScript WebUSB host integration**
   - Treat `web/src/usb/*` as the canonical WebUSB passthrough host integration for the
      `UsbHostAction`/`UsbHostCompletion` contract.
    - The repo-root `src/platform/legacy/webusb_{broker,client,protocol}.ts` stack is a **generic WebUSB demo
      RPC** (direct `navigator.usb` operations), and must not grow a second passthrough wire contract.
    - Deletion target (once demos migrate or become redundant): `src/platform/legacy/webusb_{broker,client,protocol}.ts`.

4. **Converge native on shared code**
   - The native emulator consumes `aero-usb` for USB device models + UHCI behavior.
   - Keep `emulator::io::usb` as a thin integration/re-export layer; do not add new standalone USB
     implementations under `crates/emulator`.

### Testing strategy

**Rust (automated):**

- Keep the USB passthrough and UHCI behavior covered by `crates/aero-usb` tests:
  - Unit tests in `crates/aero-usb/src/passthrough.rs`
  - UHCI + passthrough integration tests in `crates/aero-usb/tests/webusb_passthrough_uhci.rs`
  - WebHID descriptor synthesis/passthrough tests in `crates/aero-usb/tests/webhid_passthrough.rs`
- Keep the wire contract fixture stable:
  - `docs/fixtures/webusb_passthrough_wire.json` must round-trip with Rust types.

**Web (manual smoke panels):**

- WebUSB in-app diagnostics panel: `web/src/usb/webusb_panel.ts` (rendered from `web/src/main.ts`)
- WebUSB standalone diagnostics page: `/webusb_diagnostics.html` (`web/src/webusb_diagnostics.ts`)
- WebUSB passthrough broker panel: `web/src/usb/usb_broker_panel.ts` (rendered from `web/src/main.ts`)
