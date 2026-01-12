# IRQ semantics (browser runtime)

This document defines the **single, unambiguous contract** for interrupt request (IRQ) delivery in Aero's browser worker runtime.

It exists to remove ambiguity between:

- **Edge-triggered** interrupt sources (e.g. the legacy i8042 PS/2 controller on ISA IRQ1/IRQ12)
- **Level-triggered** interrupt sources (e.g. PCI INTx devices like UHCI)

## What `raiseIrq()` / `lowerIrq()` mean

In `web/src`, `IrqSink` models **physical interrupt input line levels**:

- `raiseIrq(irq)` **asserts** the line
- `lowerIrq(irq)` **deasserts** the line

These calls manipulate the *wire* (line level). They do **not** mean "deliver an interrupt right now".

See also:

- `web/src/io/device_manager.ts` (`IrqSink`)
- `web/src/workers/io.worker.ts` (device IRQ wiring)
- `web/src/workers/cpu.worker.ts` (IRQ bitmap/refcount)

## Shared IRQ lines: refcounted wire-OR

Multiple devices may share an IRQ line (e.g. PCI INTx, legacy PIC inputs). Aero models this as a refcounted **wire-OR**:

- each `raiseIrq()` increments a per-line refcount
- each `lowerIrq()` decrements it
- the effective line level is **asserted while the refcount is > 0**

### Balanced usage

Repeated `raiseIrq()` calls without an intervening `lowerIrq()` are legal, but they must eventually be balanced:

```ts
raiseIrq(1);
raiseIrq(1); // refcount now 2
lowerIrq(1); // refcount now 1 (still asserted)
lowerIrq(1); // refcount now 0 (deasserted)
```

### Guardrails (underflow/overflow)

The worker runtime clamps common misuse patterns:

- **Underflow**: extra `lowerIrq()` calls when the refcount is already 0 are ignored (dev-time warning).
- **Overflow**: refcounts **saturate at `0xffff`** to avoid `Uint16Array` wraparound (dev-time warning).

The shared helper that defines this behaviour is:

- `web/src/io/irq_refcount.ts`

And the unit tests that lock in the contract are:

- `web/src/io/irq_refcount.test.ts`

## Edge-triggered sources

Edge-triggered sources MUST be represented as an explicit *pulse* (0→1→0):

1. `raiseIrq(irq)`
2. `lowerIrq(irq)` (immediately after; same turn/microtask is fine)

Notes:

- A rising edge cannot be observed if the line is already asserted (for example because another device is holding the line high). In that case the pulse is naturally suppressed by wire-OR refcounting.
  This matches real hardware: you cannot get a new rising edge on an already-high signal.

Example:

- **i8042 keyboard/mouse controller** (ISA IRQ1/IRQ12): pulse when a byte becomes available and interrupts are enabled.

## Level-triggered sources

Level-triggered devices assert their interrupt line while an interrupt condition remains pending, and deassert it once the condition is cleared/acknowledged.

Example:

- **PCI INTx** devices (e.g. UHCI): INTx is a shared, wired-OR, *level-triggered* signal in PCI.

## Worker transport (`irqRaise` / `irqLower`)

Between the I/O worker and CPU worker, IRQs are transported as discrete AIPC events:

- `irqRaise` (line asserted)
- `irqLower` (line deasserted)

These events are still *line levels*; edge-triggered interrupts are represented as explicit pulses.

Implementation note: some paths may coalesce nested assertions for efficiency (only emit `irqRaise` on 0→1 and `irqLower` on 1→0). This is compatible with the level/refcount contract.

## Future PIC/APIC behaviour

The current CPU worker publishes a **level bitmap** of asserted IRQ lines into shared memory for debugging/observability.

A level bitmap alone is not sufficient to faithfully represent edge-triggered interrupts (a short pulse can be missed by a sampler). When a real PIC/APIC model is implemented in the browser runtime, it should:

1. Track the **external line level** per IRQ input (driven by `irqRaise`/`irqLower`).
2. For **edge-triggered inputs**, latch rising edges into a pending register (e.g. PIC IRR) so they remain pending until acknowledged/EOI.
3. For **level-triggered inputs**, treat “line asserted” as “interrupt pending” until the device clears the condition and deasserts the line.
