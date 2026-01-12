export const IO_OP_PORT_READ = 1;
export const IO_OP_PORT_WRITE = 2;
export const IO_OP_MMIO_READ = 3;
export const IO_OP_MMIO_WRITE = 4;

export const IO_OP_RESP = 5;
// IRQ line level transitions (assert/deassert).
//
// Edge-triggered devices are represented as explicit pulses: a raise immediately followed by
// a lower. See `docs/irq-semantics.md`.
export const IO_OP_IRQ_RAISE = 6;
export const IO_OP_IRQ_LOWER = 7;
export const IO_OP_A20_SET = 8;
export const IO_OP_RESET_REQUEST = 9;
// Asynchronous serial output from devices (e.g. 16550 UART).
//
// Encoding:
// - addrLo: port (u16)
// - size: byte count (1-4)
// - value: packed little-endian bytes (LSB = first byte)
export const IO_OP_SERIAL_OUT = 10;

export const IO_MESSAGE_STRIDE_U32 = 6;

export interface IoMessage {
  type: number;
  id: number;
  addrLo: number;
  addrHi: number;
  size: number;
  value: number;
}

export function decodeU64(addrLo: number, addrHi: number): bigint {
  return (BigInt(addrHi >>> 0) << 32n) | BigInt(addrLo >>> 0);
}

export function encodeU64(value: bigint): { addrLo: number; addrHi: number } {
  const addrLo = Number(value & 0xffff_ffffn) >>> 0;
  const addrHi = Number((value >> 32n) & 0xffff_ffffn) >>> 0;
  return { addrLo, addrHi };
}

export function writeIoMessage(out: Uint32Array, msg: IoMessage): void {
  if (out.length !== IO_MESSAGE_STRIDE_U32) {
    throw new Error(`writeIoMessage out length ${out.length} != ${IO_MESSAGE_STRIDE_U32}`);
  }
  out[0] = msg.type >>> 0;
  out[1] = msg.id >>> 0;
  out[2] = msg.addrLo >>> 0;
  out[3] = msg.addrHi >>> 0;
  out[4] = msg.size >>> 0;
  out[5] = msg.value >>> 0;
}

export function readIoMessage(buf: Uint32Array): IoMessage {
  if (buf.length !== IO_MESSAGE_STRIDE_U32) {
    throw new Error(`readIoMessage buf length ${buf.length} != ${IO_MESSAGE_STRIDE_U32}`);
  }
  return {
    type: buf[0]!,
    id: buf[1]!,
    addrLo: buf[2]!,
    addrHi: buf[3]!,
    size: buf[4]!,
    value: buf[5]!,
  };
}

export function defaultReadValue(size: number): number {
  // Unmapped ports/MMIO typically read as all-ones.
  if (size === 1) return 0xff;
  if (size === 2) return 0xffff;
  return 0xffff_ffff;
}
