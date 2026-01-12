# 16 - Debugging & Introspection Tooling

This document describes Aero's developer-focused debugging surface area: serial console capture, VM state inspection, breakpoints/stepping, and trace export.

---

## Serial Console (16550 UART)

The emulator models a classic 16550-compatible UART to support BIOS/bootloader logging via COM ports:

| Port | Base I/O | IRQ |
| ---- | -------- | --- |
| COM1 | `0x3F8`  | 4   |
| COM2 | `0x2F8`  | 3   |
| COM3 | `0x3E8`  | 4   |
| COM4 | `0x2E8`  | 3   |

### Host capture

When the guest writes to the THR register (offset `+0`, DLAB=0), the UART invokes its transmit callback with the written bytes. The I/O worker can forward those bytes to the main thread via the binary IPC event:

- `aero_ipc::protocol::Event::SerialOutput { port, data }` (tag `0x1500`)

---

## Debug IPC

The core IPC queue format and the stable binary message tags are documented in [`docs/ipc-protocol.md`](./ipc-protocol.md).

### Runtime log events

Each worker can emit structured `log` events on its runtime event ring. The coordinator prints
these with a `[role]` prefix in the browser console and records `WARN`/`ERROR` logs in the nonfatal
event stream.

Example (L2 tunnel forwarder telemetry from the network worker):

```
[net] l2: open tx=... rx=... drop+{...} pending=...
```

Tunnel transport failures are surfaced as `ERROR` logs (e.g. `l2: error: ...`).

---

## Breakpoints / Stepping

The `aero-debug` crate provides a `Debugger` helper that the CPU execution loop can consult:

- `check_before_exec(rip)` for breakpoints / paused state
- `check_after_exec()` for single-step completion
- `check_watchpoint(addr, len, access)` for optional memory watchpoints

The CPU worker can surface pause/breakpoint state to the host UI either through shared memory state blocks or through IPC events (TBD).

---

## Tracing

The `aero-debug::Tracer` collects structured `TraceEvent`s with:

- configurable type filters (`TraceFilter`)
- simple sampling (`sample_rate`)
- bounded in-memory buffering (`max_events`)
- JSON export (`export_json()`)

The I/O bus can log port reads/writes, while the CPU core can log instructions and interrupts.

### Network traffic capture (PCAPNG)

For packet-level networking debugging (guest↔tunnel Ethernet frames, exportable to Wireshark),
see:

- [`07-networking.md`](./07-networking.md#network-tracing-pcappcapng-export) – **Network Tracing (PCAP/PCAPNG Export)**
  - Includes the browser runtime UI panel and the `window.aero.netTrace` automation API.

---

## Web Debug UI

`web/debug.html` is a lightweight debug UI page intended for development. It expects the host to:

1. send events to the UI by calling `window.aeroDebug.onEvent(event)` (or `postMessage`)
2. listen for `aero-debug-command` DOM events and forward them to the emulator

The serial console pane supports copy/save/clear actions and auto-scrolls on new output.
