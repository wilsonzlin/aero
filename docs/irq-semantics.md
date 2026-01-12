# IRQ semantics in the browser runtime

This document standardizes how Aero represents interrupt requests (IRQs) in the **browser
multi-worker runtime** (`web/src`). It exists to remove ambiguity between **edge-triggered**
sources (like the legacy i8042 PS/2 controller) and **level-triggered** sources (like PCI INTx
devices such as UHCI).

## Background / why this matters

- The Rust i8042 integration (`crates/devices/src/i8042.rs::PlatformIrqSink`) converts a
  byte-ready event into an **edge** by explicitly pulsing the line (`raise` then immediately
  `lower`).
- The browser runtime transports IRQs between the IO worker and CPU worker as **level
  transitions** (`irqRaise` / `irqLower`) and keeps per-line refcounts in both workers.
- Historically, the TypeScript i8042 model asserted IRQ1 while its output queue was non-empty,
  which is a *level* interpretation and can drop interrupts when multiple bytes are queued.

If i8042 is treated as level-triggered, an edge-triggered interrupt controller (8259 PIC in its
default mode) will only observe the first byte becoming available and may miss subsequent bytes
until the line is deasserted.

## Contract: what `IrqSink.raiseIrq` / `IrqSink.lowerIrq` mean

In the web runtime, `IrqSink` represents **physical interrupt input line levels**:

- `raiseIrq(irq)` **asserts** the line (logical active level).
- `lowerIrq(irq)` **deasserts** the line.

The transport between workers is event-based, but the meaning is still “line changed level”.
Devices must treat these calls as manipulating the *wire*, not as “deliver an interrupt right
now”.

The runtime models IRQ lines as **wire-OR**:

- A line is considered asserted if **any** device is asserting it.
- Calls must be balanced per source: every `raiseIrq()` must eventually be matched by
  a `lowerIrq()` (even if they occur back-to-back to form a pulse).

## Level-triggered sources (example: PCI INTx / UHCI)

**Level-triggered** devices assert their interrupt line while an interrupt condition remains
pending, and deassert it once the condition is cleared/acknowledged.

Examples:

- **PCI INTx** devices (e.g. UHCI): INTx is a shared, wired-OR, *level-triggered* signal in PCI.
  `UhciPciDevice` mirrors `bridge.irq_asserted()` by calling `raiseIrq()` on 0→1 and `lowerIrq()`
  on 1→0.

## Edge-triggered sources (example: ISA i8042 IRQ1/IRQ12)

**Edge-triggered** sources generate a *pulse* (a rising edge) rather than holding a level.

In Aero’s browser runtime, edge-triggered sources MUST be represented as an explicit pulse:

1. `raiseIrq(irq)`
2. `lowerIrq(irq)` (immediately after, in the same turn/microtask is fine)

This produces a 0→1→0 transition in the IO worker’s wire-OR refcounting and therefore a matching
`irqRaise` + `irqLower` event pair delivered to the CPU worker.

Notes:

- A rising edge cannot be observed if the line is already asserted (for example, because another
  device is holding the line high). In that case the pulse is naturally suppressed by wire-OR
  refcounting. This matches real hardware: you cannot get a new rising edge on an already-high
  signal.

Examples:

- **i8042 keyboard/mouse controller** (ISA IRQ1/IRQ12): pulse when a byte is loaded into the
  output buffer and interrupts are enabled.
  - Rust glue pulses the line to feed a level+edge-detect interrupt router.
  - The TS `I8042Controller` does the same (`raiseIrq(1)` + `lowerIrq(1)`) so one interrupt is
    observable per output byte.

## How the eventual PIC/APIC should consume these events

The current CPU worker exposes a **level bitmap** of asserted IRQ lines in shared memory. That is
useful for debugging and simple polling, but it is **not sufficient** to faithfully represent
edge-triggered interrupts (a short pulse could be missed by a sampler).

When a real interrupt controller model (PIC/APIC) is implemented for the browser runtime, it
should:

1. Track the **external line level** per IRQ input (driven by `irqRaise`/`irqLower` transitions).
2. For **edge-triggered inputs**: latch rising edges into a pending-request register (e.g. PIC
   IRR) so the interrupt remains pending until acknowledged/EOI, even if the line is lowered
   immediately after the edge.
3. For **level-triggered inputs**: treat “line asserted” as “interrupt pending” until the device
   clears the condition and deasserts the line.

## Summary (current sources in `web/src`)

- **i8042 (ISA IRQ1/IRQ12)**: edge-triggered → model as pulses (`raiseIrq` then `lowerIrq`).
- **PCI INTx devices** (e.g. **UHCI**, **E1000**): level-triggered → model as line assertion
  (`raiseIrq` while pending, `lowerIrq` when cleared / acknowledged).
