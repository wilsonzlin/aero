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

## DebugCon (I/O port `0xE9`)

Many emulators (Bochs/QEMU) provide a "debug console" byte sink at I/O port `0xE9`. This is a
convenient early-boot logging channel because it does not require UART initialization.

Guest code can print a byte with a single `OUT`:

```text
mov al, 'A'
out 0xE9, al
```

In `aero_machine::Machine`, port `0xE9` is always available and bytes written to it are captured in
a host-visible buffer:

- `Machine::take_debugcon_output() -> Vec<u8>`
- `Machine::debugcon_output_len() -> u64`

In the browser runtime (`crates/aero-wasm`), the JS-facing `Machine` wrapper exposes the same log:

- `Machine.debugcon_output() -> Uint8Array`
- `Machine.debugcon_output_len() -> number`

---

## Native CLI runner (`aero-machine`)

For quick boot/integration debugging without the browser runtime, the repo includes a small native CLI tool that runs the canonical [`aero_machine::Machine`] directly.

Build + run (recommended via `scripts/safe-run.sh` to apply time/memory limits when running untrusted disk images):

```bash
# Boot a tiny fixture disk image and print COM1 output to stdout.
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk tests/fixtures/boot/boot_vga_serial_8s.img \
    --ram 64 \
    --max-insts 100000 \
    --serial-out stdout \
    --debugcon-out stdout
```

Note: the CLI opens the disk image as a native file-backed disk. By default it is **writable** (guest writes can modify the image). Use `--disk-ro` or a copy if you want a read-only base.

To keep a base image immutable while still allowing guest writes, use a copy-on-write overlay:

```bash
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk /path/to/base.img \
    --disk-overlay /tmp/overlay.aerospar \
    --max-insts 100000 \
    --serial-out stdout
```

Optional outputs:

```bash
# Save a snapshot and dump the last VGA framebuffer to a PNG on exit.
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk tests/fixtures/boot/boot_vga_serial_8s.img \
    --ram 64 \
    --max-insts 100000 \
    --serial-out stdout \
    --snapshot-save /tmp/aero.snap \
    --vga-png /tmp/aero.png

# Restore a snapshot (disk bytes are still provided externally via --disk).
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk tests/fixtures/boot/boot_vga_serial_8s.img \
    --ram 64 \
    --max-insts 100000 \
    --serial-out stdout \
    --snapshot-load /tmp/aero.snap
```

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
