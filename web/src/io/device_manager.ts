import { MmioBus } from "./bus/mmio.ts";
import { PciBus } from "./bus/pci.ts";
import { PortIoBus } from "./bus/portio.ts";
import type { PciDevice } from "./bus/pci.ts";
import type { MmioHandler } from "./bus/mmio.ts";
import type { PortIoHandler } from "./bus/portio.ts";

export interface IrqSink {
  raiseIrq(irq: number): void;
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

  registerPciDevice(device: PciDevice): void {
    this.pciBus.registerDevice(device);
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

