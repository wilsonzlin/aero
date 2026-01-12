# Workstream F: USB & Input

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns **input emulation** and **USB passthrough**: PS/2 keyboard/mouse, USB HID devices, UHCI/EHCI controllers, and browser-to-guest input event forwarding.

Input is essential for usability. Without keyboard/mouse, the emulator is unusable.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-usb/` | Canonical USB stack (ADR 0015) |
| `crates/aero-devices-input/` | PS/2 controller (i8042), keyboard, mouse |
| `web/src/usb/` | TypeScript USB broker and passthrough |
| `web/src/input/` | Browser input event capture |

---

## Essential Documentation

**Must read:**

- [`docs/08-input-devices.md`](../docs/08-input-devices.md) — Input architecture
- [`docs/usb-hid.md`](../docs/usb-hid.md) — USB HID usages and reports
- [`docs/adr/0015-canonical-usb-stack.md`](../docs/adr/0015-canonical-usb-stack.md) — USB stack design

**Reference:**

- [`docs/webhid-webusb-passthrough.md`](../docs/webhid-webusb-passthrough.md) — Passthrough architecture
- [`docs/webhid-hid-report-descriptor-synthesis.md`](../docs/webhid-hid-report-descriptor-synthesis.md) — HID descriptor synthesis
- [`docs/webusb-passthrough.md`](../docs/webusb-passthrough.md) — WebUSB passthrough
- [`docs/usb-hid-gamepad.md`](../docs/usb-hid-gamepad.md) — Gamepad support

---

## Tasks

### Input Device Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| IN-001 | PS/2 controller (i8042) | P0 | None | Medium |
| IN-002 | PS/2 keyboard | P0 | IN-001 | Medium |
| IN-003 | PS/2 mouse | P0 | IN-001 | Medium |
| IN-004 | Scancode translation | P0 | None | Medium |
| IN-005 | Browser event capture | P0 | None | Medium |
| IN-006 | Pointer Lock integration | P0 | IN-005 | Low |
| IN-007 | USB HID (keyboard) | P2 | None | Medium |
| IN-008 | USB HID (mouse) | P2 | None | Medium |
| IN-009 | Gamepad support | P2 | None | Medium |
| IN-010 | Input test suite | P0 | IN-001..IN-003 | Medium |
| IN-011 | Virtio-input device model | P1 | DM-008, VTP-002 | High |
| IN-012 | Windows 7 virtio-input driver | P1 | VIO-001..VIO-003 | Very High |
| IN-013 | HID report descriptor + mapping | P1 | IN-012 | High |
| IN-014 | Driver packaging/signing | P1 | IN-012, IN-013 | Medium |
| IN-015 | Browser → virtio-input events | P1 | IN-005, IN-011 | Medium |
| IN-016 | Virtio-input test plan | P1 | IN-011..IN-015 | Medium |

---

## Input Architecture

### PS/2 Path (Legacy, Always Works)

```
┌─────────────────────────────────────────────┐
│            Browser                           │
│                 │                            │
│    keydown/keyup/mousemove events           │
│                 │                            │
│                 ▼                            │
│        Scancode Translation                 │  ← IN-004
│                 │                            │
└─────────────────┼───────────────────────────┘
                  │ SharedArrayBuffer
                  ▼
┌─────────────────────────────────────────────┐
│            Emulator                          │
│                 │                            │
│         i8042 Controller                    │  ← IN-001
│                 │                            │
│         PS/2 Keyboard/Mouse                 │  ← IN-002, IN-003
│                 │                            │
│                 ▼                            │
│         Windows 7 Guest                      │
└─────────────────────────────────────────────┘
```

### Virtio-input Path (Paravirtualized, Faster)

```
Browser events → virtio-input device model → virtio-input driver → Windows HID stack
```

### USB Passthrough Path (Physical Devices)

```
Physical USB device → WebUSB/WebHID → UHCI emulation → Windows USB stack
```

---

## Scancode Translation

Browser `KeyboardEvent.code` → PS/2 Set 2 scancode:

```typescript
// Example mapping (see web/src/input/ for full implementation)
const scancodeMap: Record<string, number[]> = {
  'KeyA': [0x1C],           // Make code
  'KeyA_break': [0xF0, 0x1C], // Break code
  'Enter': [0x5A],
  'Escape': [0x76],
  // Extended keys use 0xE0 prefix
  'ArrowUp': [0xE0, 0x75],
  // ...
};
```

---

## USB Stack (aero-usb)

Per ADR 0015, `crates/aero-usb` is the **canonical USB stack**. It provides:

- UHCI controller emulation
- USB device enumeration
- HID class driver
- Passthrough bridge for WebUSB/WebHID

```rust
// Simplified USB stack interface
pub trait UsbController {
    fn attach_device(&mut self, device: Box<dyn UsbDevice>);
    fn detach_device(&mut self, port: u8);
    fn process_frame(&mut self);
}

pub trait UsbDevice {
    fn handle_control(&mut self, setup: SetupPacket) -> ControlResult;
    fn handle_bulk_in(&mut self, endpoint: u8) -> BulkResult;
    fn handle_bulk_out(&mut self, endpoint: u8, data: &[u8]) -> BulkResult;
}
```

---

## Browser Event Capture

Key considerations:

1. **Pointer Lock**: Essential for mouse capture in games
2. **Key repeat**: Browser handles repeat; emulator may need to filter
3. **Focus**: Events only captured when canvas is focused
4. **Modifier keys**: Track Ctrl/Alt/Shift state

```typescript
// Example event capture setup
canvas.addEventListener('keydown', (e) => {
  e.preventDefault();
  sendScancode(e.code, true); // make
});

canvas.addEventListener('keyup', (e) => {
  e.preventDefault();
  sendScancode(e.code, false); // break
});

canvas.addEventListener('click', () => {
  canvas.requestPointerLock();
});
```

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: i8042 registers accessed via `CpuBus::io_read/io_write`
- **Integration (H)**: USB controllers wired into PCI bus

### What Other Workstreams Need From You

- Working keyboard/mouse for all other testing
- USB HID for more complex input scenarios

---

## Testing

```bash
# Run input tests
./scripts/safe-run.sh cargo test -p aero-devices-input --locked
./scripts/safe-run.sh cargo test -p aero-usb --locked

# Run USB bridge tests
cd web
npm test -- --grep usb
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/08-input-devices.md`](../docs/08-input-devices.md)
4. ☐ Read [`docs/adr/0015-canonical-usb-stack.md`](../docs/adr/0015-canonical-usb-stack.md)
5. ☐ Explore `crates/aero-devices-input/src/` and `crates/aero-usb/src/`
6. ☐ Run existing tests to establish baseline
7. ☐ Pick a task from the tables above and begin

---

*Input makes the emulator interactive. Without it, you're just watching a movie.*
