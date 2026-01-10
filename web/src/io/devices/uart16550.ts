import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PortIoHandler } from "../bus/portio.ts";

export interface SerialOutputSink {
  write(port: number, data: Uint8Array): void;
}

export interface UartConfig {
  basePort: number;
  irq: number;
}

export const UART_COM1: UartConfig = { basePort: 0x3f8, irq: 4 };
export const UART_COM2: UartConfig = { basePort: 0x2f8, irq: 3 };
export const UART_COM3: UartConfig = { basePort: 0x3e8, irq: 4 };
export const UART_COM4: UartConfig = { basePort: 0x2e8, irq: 3 };

/**
 * Minimal 16550 UART model (COM ports).
 *
 * This is intended for BIOS/bootloader serial logging and basic debugging.
 * It models the core register set and emits TX bytes via an injected sink.
 */
export class Uart16550 implements PortIoHandler {
  readonly #cfg: UartConfig;
  readonly #sink?: SerialOutputSink;

  #dll = 0x00;
  #dlm = 0x00;
  #ier = 0x00;
  #fcr = 0x00;
  #lcr = 0x00;
  #mcr = 0x00;
  #scr = 0x00;

  #rxQueue: number[] = [];

  constructor(cfg: UartConfig, sink?: SerialOutputSink) {
    this.#cfg = cfg;
    this.#sink = sink;
  }

  get basePort(): number {
    return this.#cfg.basePort;
  }

  injectRx(byte: number): void {
    this.#rxQueue.push(byte & 0xff);
  }

  portRead(port: number, size: number): number {
    const p = port & 0xffff;
    switch (size) {
      case 1:
        return this.#readU8(p);
      case 2:
        return (this.#readU8(p) | (this.#readU8((p + 1) & 0xffff) << 8)) >>> 0;
      case 4:
        return (
          this.#readU8(p) |
          (this.#readU8((p + 1) & 0xffff) << 8) |
          (this.#readU8((p + 2) & 0xffff) << 16) |
          (this.#readU8((p + 3) & 0xffff) << 24)
        ) >>> 0;
      default:
        return defaultReadValue(size);
    }
  }

  portWrite(port: number, size: number, value: number): void {
    const p = port & 0xffff;
    const v = value >>> 0;
    switch (size) {
      case 1:
        this.#writeU8(p, v & 0xff);
        break;
      case 2:
        this.#writeU8(p, v & 0xff);
        this.#writeU8((p + 1) & 0xffff, (v >>> 8) & 0xff);
        break;
      case 4:
        this.#writeU8(p, v & 0xff);
        this.#writeU8((p + 1) & 0xffff, (v >>> 8) & 0xff);
        this.#writeU8((p + 2) & 0xffff, (v >>> 16) & 0xff);
        this.#writeU8((p + 3) & 0xffff, (v >>> 24) & 0xff);
        break;
      default:
        break;
    }
  }

  #dlab(): boolean {
    return (this.#lcr & 0x80) !== 0;
  }

  #readU8(port: number): number {
    const offset = (port - this.#cfg.basePort) & 0xffff;
    switch (offset) {
      case 0: // RBR / DLL
        if (this.#dlab()) return this.#dll;
        return (this.#rxQueue.shift() ?? 0) & 0xff;
      case 1: // IER / DLM
        if (this.#dlab()) return this.#dlm;
        return this.#ier;
      case 2: {
        // IIR (we do not currently model interrupts; report "no interrupt pending")
        const fifoEnabled = (this.#fcr & 0x01) !== 0;
        const fifoBits = fifoEnabled ? 0xc0 : 0x00;
        return (fifoBits | 0x01) & 0xff;
      }
      case 3:
        return this.#lcr & 0xff;
      case 4:
        return this.#mcr & 0xff;
      case 5: {
        // LSR: bit0=DR, bit5=THRE, bit6=TEMT
        let lsr = 0x60;
        if (this.#rxQueue.length > 0) lsr |= 0x01;
        return lsr;
      }
      case 6:
        return 0x00;
      case 7:
        return this.#scr & 0xff;
      default:
        return 0x00;
    }
  }

  #writeU8(port: number, value: number): void {
    const offset = (port - this.#cfg.basePort) & 0xffff;
    const v = value & 0xff;

    switch (offset) {
      case 0: // THR / DLL
        if (this.#dlab()) {
          this.#dll = v;
        } else {
          this.#sink?.write(this.#cfg.basePort, Uint8Array.of(v));
        }
        break;
      case 1: // IER / DLM
        if (this.#dlab()) this.#dlm = v;
        else this.#ier = v;
        break;
      case 2: // FCR
        this.#fcr = v;
        if ((v & 0x02) !== 0) this.#rxQueue = [];
        break;
      case 3:
        this.#lcr = v;
        break;
      case 4:
        this.#mcr = v;
        break;
      case 7:
        this.#scr = v;
        break;
      default:
        break;
    }
  }
}

