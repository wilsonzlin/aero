import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "../bus/portio.ts";
import type { IrqSink } from "../device_manager.ts";

const STATUS_OBF = 0x01; // Output Buffer Full
const STATUS_SYS = 0x04; // System flag

const OUTPUT_PORT_RESET = 0x01; // Bit 0 (active-low reset line)
const OUTPUT_PORT_A20 = 0x02; // Bit 1

type OutputSource = "controller" | "keyboard";

interface OutputByte {
  value: number;
  source: OutputSource;
}

export interface I8042SystemControlSink {
  setA20(enabled: boolean): void;
  requestReset(): void;
}

export interface I8042ControllerOptions {
  systemControl?: I8042SystemControlSink;
}

/**
 * Minimal i8042 PS/2 controller model sufficient for early boot and tests.
 *
 * Implemented:
 * - Ports 0x60 (data) and 0x64 (status/command)
 * - Controller commands: 0x20 (read command byte), 0x60 (write command byte),
 *   0xAA (self test), 0xD0/0xD1 (output port), 0xFE (reset pulse)
 * - Keyboard command: 0xFF (reset) -> 0xFA, 0xAA
 * - IRQ1 level signalling when keyboard data is pending and interrupts enabled.
 */
export class I8042Controller implements PortIoHandler {
  readonly #irq: IrqSink;
  readonly #sysCtrl?: I8042SystemControlSink;

  #status = STATUS_SYS;
  #commandByte = 0x00;
  #pendingCommand: number | null = null;

  #outQueue: OutputByte[] = [];
  #irq1Asserted = false;

  #outputPort = OUTPUT_PORT_RESET;

  constructor(irq: IrqSink, opts: I8042ControllerOptions = {}) {
    this.#irq = irq;
    this.#sysCtrl = opts.systemControl;
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

  /**
   * Inject host keyboard scancode bytes into the controller output buffer.
   *
   * Bytes injected via this path are treated as coming from the keyboard device
   * (as opposed to controller replies), so IRQ1 signalling follows the command
   * byte interrupt-enable bit.
   */
  injectKeyboardBytes(bytes: Uint8Array): void {
    for (const b of bytes) this.#enqueue(b, "keyboard");
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
      case 0xd0: // Read output port
        this.#enqueue(this.#outputPort, "controller");
        return;
      case 0xd1: // Write output port (next data byte)
        this.#pendingCommand = 0xd1;
        return;
      case 0xdd: // Non-standard: disable A20 gate
        this.#setOutputPort(this.#outputPort & ~OUTPUT_PORT_A20);
        return;
      case 0xdf: // Non-standard: enable A20 gate
        this.#setOutputPort(this.#outputPort | OUTPUT_PORT_A20);
        return;
      case 0xfe: // Pulse output port bit 0 low (system reset)
        this.#sysCtrl?.requestReset();
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

    if (this.#pendingCommand === 0xd1) {
      this.#pendingCommand = null;
      this.#setOutputPort(data);
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

  #setOutputPort(value: number): void {
    const next = value & 0xff;
    const prev = this.#outputPort;
    this.#outputPort = next;

    const prevA20 = (prev & OUTPUT_PORT_A20) !== 0;
    const nextA20 = (next & OUTPUT_PORT_A20) !== 0;
    if (prevA20 !== nextA20) {
      this.#sysCtrl?.setA20(nextA20);
    }

    // Bit 0 is active-low: transitioning from 1 -> 0 asserts reset.
    const prevResetDeasserted = (prev & OUTPUT_PORT_RESET) !== 0;
    const nextResetDeasserted = (next & OUTPUT_PORT_RESET) !== 0;
    if (prevResetDeasserted && !nextResetDeasserted) {
      this.#sysCtrl?.requestReset();
    }
  }
}
