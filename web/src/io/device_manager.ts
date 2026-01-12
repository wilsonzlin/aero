import { MmioBus } from "./bus/mmio.ts";
import { PciBus } from "./bus/pci.ts";
import { PortIoBus } from "./bus/portio.ts";
import type { PciAddress, PciDevice } from "./bus/pci.ts";
import type { MmioHandler } from "./bus/mmio.ts";
import type { PortIoHandler } from "./bus/portio.ts";

export interface IrqSink {
  /**
   * Assert an IRQ line.
   *
   * In the browser runtime, IRQs are modeled as *physical line levels*
   * (asserted vs deasserted) and transported between workers as discrete
   * `irqRaise`/`irqLower` events (see `docs/irq-semantics.md`).
   *
   * Shared lines are treated as refcounted wire-OR levels:
   * - each `raiseIrq()` increments a per-line refcount
   * - each `lowerIrq()` decrements it
   * - the line is considered asserted while the refcount is > 0
   *
   * Repeated `raiseIrq()` calls without an intervening `lowerIrq()` are legal,
   * but must eventually be balanced.
   *
   * Edge-triggered sources (e.g. ISA i8042 IRQ1/IRQ12) are represented as an
   * explicit pulse: call `raiseIrq()` immediately followed by `lowerIrq()` to
   * generate a 0→1→0 transition.
   */
  raiseIrq(irq: number): void;
  /**
   * Deassert an IRQ line.
   *
   * See `raiseIrq()` / `docs/irq-semantics.md` for the contract. Extra
   * `lowerIrq()` calls when the refcount is already 0 are ignored (and may warn
   * in dev builds).
   */
  lowerIrq(irq: number): void;
}

export interface TickableDevice {
  tick(nowMs: number): void;
}

export class DeviceManager {
  readonly portBus = new PortIoBus();
  readonly mmioBus = new MmioBus();
  readonly pciBus = new PciBus(this.portBus, this.mmioBus);

  readonly #irqSink: IrqSink;
  readonly #tickables: TickableDevice[] = [];

  constructor(irqSink: IrqSink) {
    this.#irqSink = irqSink;
    this.pciBus.registerToPortBus();
  }

  get irqSink(): IrqSink {
    return this.#irqSink;
  }

  registerPortIo(startPort: number, endPort: number, handler: PortIoHandler): void {
    this.portBus.registerRange(startPort, endPort, handler);
  }

  registerMmio(base: bigint, size: bigint, handler: MmioHandler): void {
    this.mmioBus.mapRange(base, size, handler);
  }

  registerPciDevice(device: PciDevice, addr?: Partial<PciAddress>): PciAddress {
    return this.pciBus.registerDevice(device, addr);
  }

  addTickable(device: TickableDevice): void {
    this.#tickables.push(device);
  }

  tick(nowMs: number): void {
    for (const dev of this.#tickables) dev.tick(nowMs);
  }

  portRead(port: number, size: number): number {
    return this.portBus.read(port, size);
  }

  portWrite(port: number, size: number, value: number): void {
    this.portBus.write(port, size, value);
  }

  mmioRead(addr: bigint, size: number): number {
    return this.mmioBus.read(addr, size);
  }

  mmioWrite(addr: bigint, size: number, value: number): void {
    this.mmioBus.write(addr, size, value);
  }
}
