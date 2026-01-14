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

When the guest writes to the THR register (offset `+0`, DLAB=0), the UART invokes its transmit callback with the written bytes. The
owning worker forwards those bytes to the main thread via the binary IPC event:

- `vmRuntime="legacy"`: I/O worker owns the UART device model.
- `vmRuntime="machine"`: machine CPU worker (`api.Machine`) owns the UART device model.

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

In `aero_machine::Machine`, port `0xE9` is available when `MachineConfig::enable_debugcon=true`
(default) and bytes written to it are captured in a host-visible buffer:

- `Machine::take_debugcon_output() -> Vec<u8>`
- `Machine::debugcon_output_len() -> u64`

To disable the device (and leave the port unmapped), set `MachineConfig::enable_debugcon=false`.

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

The CLI requires at least one of:

- `--disk` (primary HDD image)
- `--install-iso` (ATAPI CD-ROM install/recovery media)

When `--disk` is used, the CLI opens the disk image as a native file-backed disk. By default it is **writable** (guest writes can modify the image). Use `--disk-ro` or a copy if you want a read-only base.

To keep a base image immutable while still allowing guest writes, use a copy-on-write overlay:

```bash
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk /path/to/base.img \
    --disk-overlay /tmp/overlay.aerospar \
    --max-insts 100000 \
    --serial-out stdout
```

Install media (ATAPI CD-ROM) and boot policy:

```bash
# Attach a Win7 install ISO and boot from CD once, then allow the guest to reboot into the HDD.
# (The CLI automatically disables the firmware CD-first policy after the first guest reset if the
# active boot device was the CD-ROM, to avoid setup looping back into the installer.)
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk /path/to/win7.img \
    --install-iso /path/to/win7.iso \
    --boot cd-first \
    --max-ms 60000 \
    --serial-out stdout

# Force boot from CD every time (even after guest resets).
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --disk /path/to/win7.img \
    --install-iso /path/to/win7.iso \
    --boot cdrom \
    --max-ms 60000

# ISO-only boot (no HDD attached).
bash ./scripts/safe-run.sh \
  cargo run -p aero-machine-cli -- \
    --install-iso /path/to/recovery.iso \
    --boot cdrom \
    --max-ms 60000
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

## Browser Automation Debug API (`window.aero.debug`)

The browser runtime installs a small set of automation-friendly helpers under `window.aero.debug`.
This is intended for tests/harnesses that need a stable interface for introspecting VM runtime
state without reaching into private coordinator fields.

See:

- `web/src/runtime/boot_device_backend.ts` (implementation)
- `shared/aero_api.ts` (`AeroDebugApi` types)

### Boot disk selection vs active boot device

Aero distinguishes between:

- **Selected boot policy** – what the host requested for the next reset (e.g. "boot CD first").
- **Active boot device** – what firmware actually booted from in the current boot session (CD vs
  HDD), which can differ when fallback policies are enabled.

The relevant helpers are:

- `window.aero.debug.getBootDisks() -> { mounts: {hddId?, cdId?}, bootDevice? } | null`
  - Returns the current boot disk selection snapshot from the main-thread coordinator.
  - `mounts.*Id` are DiskManager mount IDs (opaque strings).
  - `bootDevice` is the requested policy (`"hdd"` / `"cdrom"`), which may differ from the active
    boot source when firmware falls back.
- `window.aero.debug.getMachineCpuActiveBootDevice() -> "hdd" | "cdrom" | null`
  - Returns the active boot device reported by the machine CPU worker (requires
    `vmRuntime="machine"`).
  - `null` means unknown/unavailable (e.g. workers not running, older builds without reporting, or
    a reboot/disk-reattach transition where the next boot session has not reported yet).
- `window.aero.debug.getMachineCpuBootConfig() -> { bootDrive: number, cdBootDrive: number, bootFromCdIfPresent: boolean } | null`
  - Returns the machine CPU worker's firmware boot configuration snapshot:
    - `bootDrive` – BIOS boot drive number (`DL`), typically `0x80` (HDD0) or `0xE0` (CD0).
    - `cdBootDrive` – BIOS CD-ROM drive number used by the "CD-first when present" policy (`0xE0..=0xEF`).
    - `bootFromCdIfPresent` – whether the firmware CD-first fallback policy is enabled.
  - `null` means unknown/unavailable (e.g. older WASM builds without boot-config exports, or a
    reboot/disk-reattach transition where the CPU worker has not re-reported yet).

---

## Web Debug UI

`web/debug.html` is a lightweight debug UI page intended for development. It expects the host to:

1. send events to the UI by calling `window.aeroDebug.onEvent(event)` (or `postMessage`)
2. listen for `aero-debug-command` DOM events and forward them to the emulator

The serial console pane supports copy/save/clear actions and auto-scrolls on new output.
