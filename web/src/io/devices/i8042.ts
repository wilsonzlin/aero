import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "../bus/portio.ts";
import type { IrqSink } from "../device_manager.ts";

const STATUS_OBF = 0x01; // Output Buffer Full
const STATUS_SYS = 0x04; // System flag

type OutputSource = "controller" | "keyboard";

interface OutputByte {
  value: number;
  source: OutputSource;
}

/**
 * Minimal i8042 PS/2 controller model sufficient for early boot and tests.
 *
 * Implemented:
 * - Ports 0x60 (data) and 0x64 (status/command)
 * - Controller commands: 0x20 (read command byte), 0x60 (write command byte),
 *   0xAA (self test)
 * - Keyboard command: 0xFF (reset) -> 0xFA, 0xAA
 * - IRQ1 level signalling when keyboard data is pending and interrupts enabled.
 */
export class I8042Controller implements PortIoHandler {
  readonly #irq: IrqSink;

  #status = STATUS_SYS;
  #commandByte = 0x00;
  #pendingCommand: number | null = null;

  #outQueue: OutputByte[] = [];
  #irq1Asserted = false;

  constructor(irq: IrqSink) {
    this.#irq = irq;
  }

  portRead(port: number, size: number): number {
    if (size !== 1) return defaultReadValue(size);

    switch (port & 0xffff) {
      case 0x0060: {
        const item = this.#outQueue.shift() ?? null;
        this.#syncStatusAndIrq();
        return item ? item.value & 0xff : 0x00;
      }
      case 0x0064:
        return this.#status & 0xff;
      default:
        return defaultReadValue(size);
    }
  }

  portWrite(port: number, size: number, value: number): void {
    if (size !== 1) return;
    const v = value & 0xff;

    switch (port & 0xffff) {
      case 0x0064:
        this.#writeCommand(v);
        return;
      case 0x0060:
        this.#writeData(v);
        return;
      default:
        return;
    }
  }

  #writeCommand(cmd: number): void {
    switch (cmd & 0xff) {
      case 0x20: // Read command byte
        this.#enqueue(this.#commandByte, "controller");
        return;
      case 0x60: // Write command byte (next data byte)
        this.#pendingCommand = 0x60;
        return;
      case 0xaa: // Self test
        this.#enqueue(0x55, "controller");
        return;
      default:
        // Unknown/unimplemented controller command.
        return;
    }
  }

  #writeData(data: number): void {
    if (this.#pendingCommand === 0x60) {
      this.#pendingCommand = null;
      this.#commandByte = data & 0xff;
      this.#syncStatusAndIrq();
      return;
    }

    // Send byte to PS/2 keyboard.
    const replies = this.#keyboardHandleCommand(data);
    for (const b of replies) this.#enqueue(b, "keyboard");
  }

  #keyboardHandleCommand(cmd: number): number[] {
    switch (cmd & 0xff) {
      case 0xff: // Reset
        return [0xfa, 0xaa];
      default:
        // Always ACK unknown commands for now.
        return [0xfa];
    }
  }

  #enqueue(value: number, source: OutputSource): void {
    this.#outQueue.push({ value: value & 0xff, source });
    this.#syncStatusAndIrq();
  }

  #syncStatusAndIrq(): void {
    if (this.#outQueue.length > 0) this.#status |= STATUS_OBF;
    else this.#status &= ~STATUS_OBF;

    const irqEnabled = (this.#commandByte & 0x01) !== 0;
    const headSource = this.#outQueue[0]?.source ?? null;
    const shouldAssert = irqEnabled && headSource === "keyboard";

    if (shouldAssert && !this.#irq1Asserted) {
      this.#irq.raiseIrq(1);
      this.#irq1Asserted = true;
    } else if (!shouldAssert && this.#irq1Asserted) {
      this.#irq.lowerIrq(1);
      this.#irq1Asserted = false;
    }
  }
}

