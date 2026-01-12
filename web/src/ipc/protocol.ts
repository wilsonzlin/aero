// Binary IPC protocol shared by coordinator and workers.
//
// This mirrors `crates/aero-ipc/src/protocol.rs`.

export type Command =
  | { kind: "nop"; seq: number }
  | { kind: "shutdown" }
  | { kind: "mmioRead"; id: number; addr: bigint; size: number }
  | { kind: "mmioWrite"; id: number; addr: bigint; data: Uint8Array }
  | { kind: "portRead"; id: number; port: number; size: number }
  | { kind: "portWrite"; id: number; port: number; size: number; value: number }
  | { kind: "diskRead"; id: number; diskOffset: bigint; len: number; guestOffset: bigint }
  | { kind: "diskWrite"; id: number; diskOffset: bigint; len: number; guestOffset: bigint };

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

export type Event =
  | { kind: "ack"; seq: number }
  | { kind: "mmioReadResp"; id: number; data: Uint8Array }
  | { kind: "portReadResp"; id: number; value: number }
  | { kind: "mmioWriteResp"; id: number }
  | { kind: "portWriteResp"; id: number }
  | { kind: "diskReadResp"; id: number; ok: boolean; bytes: number; errorCode?: number }
  | { kind: "diskWriteResp"; id: number; ok: boolean; bytes: number; errorCode?: number }
  | { kind: "frameReady"; frameId: bigint }
  // IRQ line level transitions (assert/deassert).
  //
  // Edge-triggered devices are represented as explicit pulses: `irqRaise` then `irqLower`.
  // See `docs/irq-semantics.md`.
  | { kind: "irqRaise"; irq: number }
  | { kind: "irqLower"; irq: number }
  | { kind: "a20Set"; enabled: boolean }
  | { kind: "resetRequest" }
  | { kind: "log"; level: LogLevel; message: string }
  | { kind: "serialOutput"; port: number; data: Uint8Array }
  | { kind: "panic"; message: string }
  | { kind: "tripleFault" };

const CMD_TAG_NOP = 0x0000;
const CMD_TAG_SHUTDOWN = 0x0001;
const CMD_TAG_MMIO_READ = 0x0100;
const CMD_TAG_MMIO_WRITE = 0x0101;
const CMD_TAG_PORT_READ = 0x0102;
const CMD_TAG_PORT_WRITE = 0x0103;
const CMD_TAG_DISK_READ = 0x0104;
const CMD_TAG_DISK_WRITE = 0x0105;

const EVT_TAG_ACK = 0x1000;
const EVT_TAG_MMIO_READ_RESP = 0x1100;
const EVT_TAG_PORT_READ_RESP = 0x1101;
const EVT_TAG_MMIO_WRITE_RESP = 0x1102;
const EVT_TAG_PORT_WRITE_RESP = 0x1103;
const EVT_TAG_DISK_READ_RESP = 0x1104;
const EVT_TAG_DISK_WRITE_RESP = 0x1105;
const EVT_TAG_FRAME_READY = 0x1200;
const EVT_TAG_IRQ_RAISE = 0x1300;
const EVT_TAG_IRQ_LOWER = 0x1301;
const EVT_TAG_A20_SET = 0x1302;
const EVT_TAG_RESET_REQUEST = 0x1303;
const EVT_TAG_LOG = 0x1400;
const EVT_TAG_SERIAL_OUTPUT = 0x1500;
const EVT_TAG_PANIC = 0x1ffe;
const EVT_TAG_TRIPLE_FAULT = 0x1fff;

export function encodeCommand(cmd: Command): Uint8Array {
  // Worst-case size is small; allocate growing buffer.
  const out: number[] = [];
  const pushU8 = (v: number) => out.push(v & 0xff);
  const pushU16 = (v: number) => {
    pushU8(v);
    pushU8(v >>> 8);
  };
  const pushU32 = (v: number) => {
    pushU8(v);
    pushU8(v >>> 8);
    pushU8(v >>> 16);
    pushU8(v >>> 24);
  };
  const pushU64 = (v: bigint) => {
    const lo = Number(v & 0xffff_ffffn);
    const hi = Number((v >> 32n) & 0xffff_ffffn);
    pushU32(lo);
    pushU32(hi);
  };

  switch (cmd.kind) {
    case "nop":
      pushU16(CMD_TAG_NOP);
      pushU32(cmd.seq);
      break;
    case "shutdown":
      pushU16(CMD_TAG_SHUTDOWN);
      break;
    case "mmioRead":
      pushU16(CMD_TAG_MMIO_READ);
      pushU32(cmd.id);
      pushU64(cmd.addr);
      pushU32(cmd.size);
      break;
    case "mmioWrite":
      pushU16(CMD_TAG_MMIO_WRITE);
      pushU32(cmd.id);
      pushU64(cmd.addr);
      pushU32(cmd.data.byteLength);
      for (const b of cmd.data) pushU8(b);
      break;
    case "portRead":
      pushU16(CMD_TAG_PORT_READ);
      pushU32(cmd.id);
      pushU16(cmd.port);
      pushU32(cmd.size);
      break;
    case "portWrite":
      pushU16(CMD_TAG_PORT_WRITE);
      pushU32(cmd.id);
      pushU16(cmd.port);
      pushU32(cmd.size);
      pushU32(cmd.value);
      break;
    case "diskRead":
      pushU16(CMD_TAG_DISK_READ);
      pushU32(cmd.id);
      pushU64(cmd.diskOffset);
      pushU32(cmd.len);
      pushU64(cmd.guestOffset);
      break;
    case "diskWrite":
      pushU16(CMD_TAG_DISK_WRITE);
      pushU32(cmd.id);
      pushU64(cmd.diskOffset);
      pushU32(cmd.len);
      pushU64(cmd.guestOffset);
      break;
  }
  return Uint8Array.from(out);
}

export function decodeCommand(bytes: Uint8Array): Command {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 0;
  const readU16 = () => {
    const v = view.getUint16(off, true);
    off += 2;
    return v;
  };
  const readU32 = () => {
    const v = view.getUint32(off, true);
    off += 4;
    return v;
  };
  const readU64 = () => {
    const lo = BigInt(readU32());
    const hi = BigInt(readU32());
    return lo | (hi << 32n);
  };

  const tag = readU16();
  let cmd: Command;
  switch (tag) {
    case CMD_TAG_NOP:
      cmd = { kind: "nop", seq: readU32() };
      break;
    case CMD_TAG_SHUTDOWN:
      cmd = { kind: "shutdown" };
      break;
    case CMD_TAG_MMIO_READ:
      cmd = { kind: "mmioRead", id: readU32(), addr: readU64(), size: readU32() };
      break;
    case CMD_TAG_MMIO_WRITE: {
      const id = readU32();
      const addr = readU64();
      const len = readU32();
      const data = bytes.slice(off, off + len);
      off += len;
      cmd = { kind: "mmioWrite", id, addr, data };
      break;
    }
    case CMD_TAG_PORT_READ:
      cmd = { kind: "portRead", id: readU32(), port: readU16(), size: readU32() };
      break;
    case CMD_TAG_PORT_WRITE:
      cmd = { kind: "portWrite", id: readU32(), port: readU16(), size: readU32(), value: readU32() };
      break;
    case CMD_TAG_DISK_READ:
      cmd = { kind: "diskRead", id: readU32(), diskOffset: readU64(), len: readU32(), guestOffset: readU64() };
      break;
    case CMD_TAG_DISK_WRITE:
      cmd = { kind: "diskWrite", id: readU32(), diskOffset: readU64(), len: readU32(), guestOffset: readU64() };
      break;
    default:
      throw new Error(`unknown command tag 0x${tag.toString(16)}`);
  }
  if (off !== bytes.byteLength) throw new Error("trailing bytes in command");
  return cmd;
}

export function encodeEvent(evt: Event): Uint8Array {
  const out: number[] = [];
  const pushU8 = (v: number) => out.push(v & 0xff);
  const pushU16 = (v: number) => {
    pushU8(v);
    pushU8(v >>> 8);
  };
  const pushU32 = (v: number) => {
    pushU8(v);
    pushU8(v >>> 8);
    pushU8(v >>> 16);
    pushU8(v >>> 24);
  };
  const pushU64 = (v: bigint) => {
    const lo = Number(v & 0xffff_ffffn);
    const hi = Number((v >> 32n) & 0xffff_ffffn);
    pushU32(lo);
    pushU32(hi);
  };

  const encoder = new TextEncoder();

  switch (evt.kind) {
    case "ack":
      pushU16(EVT_TAG_ACK);
      pushU32(evt.seq);
      break;
    case "mmioReadResp":
      pushU16(EVT_TAG_MMIO_READ_RESP);
      pushU32(evt.id);
      pushU32(evt.data.byteLength);
      for (const b of evt.data) pushU8(b);
      break;
    case "portReadResp":
      pushU16(EVT_TAG_PORT_READ_RESP);
      pushU32(evt.id);
      pushU32(evt.value);
      break;
    case "mmioWriteResp":
      pushU16(EVT_TAG_MMIO_WRITE_RESP);
      pushU32(evt.id);
      break;
    case "portWriteResp":
      pushU16(EVT_TAG_PORT_WRITE_RESP);
      pushU32(evt.id);
      break;
    case "diskReadResp":
      pushU16(EVT_TAG_DISK_READ_RESP);
      pushU32(evt.id);
      pushU8(evt.ok ? 1 : 0);
      pushU32(evt.bytes);
      if (!evt.ok) pushU32(evt.errorCode ?? 0);
      break;
    case "diskWriteResp":
      pushU16(EVT_TAG_DISK_WRITE_RESP);
      pushU32(evt.id);
      pushU8(evt.ok ? 1 : 0);
      pushU32(evt.bytes);
      if (!evt.ok) pushU32(evt.errorCode ?? 0);
      break;
    case "frameReady":
      pushU16(EVT_TAG_FRAME_READY);
      pushU64(evt.frameId);
      break;
    case "irqRaise":
      pushU16(EVT_TAG_IRQ_RAISE);
      pushU8(evt.irq);
      break;
    case "irqLower":
      pushU16(EVT_TAG_IRQ_LOWER);
      pushU8(evt.irq);
      break;
    case "a20Set":
      pushU16(EVT_TAG_A20_SET);
      pushU8(evt.enabled ? 1 : 0);
      break;
    case "resetRequest":
      pushU16(EVT_TAG_RESET_REQUEST);
      break;
    case "log": {
      pushU16(EVT_TAG_LOG);
      pushU8(logLevelToU8(evt.level));
      const msg = encoder.encode(evt.message);
      pushU32(msg.byteLength);
      for (const b of msg) pushU8(b);
      break;
    }
    case "serialOutput": {
      pushU16(EVT_TAG_SERIAL_OUTPUT);
      pushU16(evt.port);
      pushU32(evt.data.byteLength);
      for (const b of evt.data) pushU8(b);
      break;
    }
    case "panic": {
      pushU16(EVT_TAG_PANIC);
      const msg = encoder.encode(evt.message);
      pushU32(msg.byteLength);
      for (const b of msg) pushU8(b);
      break;
    }
    case "tripleFault":
      pushU16(EVT_TAG_TRIPLE_FAULT);
      break;
  }
  return Uint8Array.from(out);
}

export function decodeEvent(bytes: Uint8Array): Event {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 0;
  const readU8 = () => view.getUint8(off++);
  const readU16 = () => {
    const v = view.getUint16(off, true);
    off += 2;
    return v;
  };
  const readU32 = () => {
    const v = view.getUint32(off, true);
    off += 4;
    return v;
  };
  const readU64 = () => {
    const lo = BigInt(readU32());
    const hi = BigInt(readU32());
    return lo | (hi << 32n);
  };
  const decoder = new TextDecoder();

  const tag = readU16();
  let evt: Event;
  switch (tag) {
    case EVT_TAG_ACK:
      evt = { kind: "ack", seq: readU32() };
      break;
    case EVT_TAG_MMIO_READ_RESP: {
      const id = readU32();
      const len = readU32();
      const data = bytes.slice(off, off + len);
      off += len;
      evt = { kind: "mmioReadResp", id, data };
      break;
    }
    case EVT_TAG_PORT_READ_RESP:
      evt = { kind: "portReadResp", id: readU32(), value: readU32() };
      break;
    case EVT_TAG_MMIO_WRITE_RESP:
      evt = { kind: "mmioWriteResp", id: readU32() };
      break;
    case EVT_TAG_PORT_WRITE_RESP:
      evt = { kind: "portWriteResp", id: readU32() };
      break;
    case EVT_TAG_DISK_READ_RESP: {
      const id = readU32();
      const ok = readU8() !== 0;
      const bytesRead = readU32();
      if (ok) {
        evt = { kind: "diskReadResp", id, ok, bytes: bytesRead };
      } else {
        const errorCode = readU32();
        evt = { kind: "diskReadResp", id, ok, bytes: bytesRead, errorCode };
      }
      break;
    }
    case EVT_TAG_DISK_WRITE_RESP: {
      const id = readU32();
      const ok = readU8() !== 0;
      const bytesWritten = readU32();
      if (ok) {
        evt = { kind: "diskWriteResp", id, ok, bytes: bytesWritten };
      } else {
        const errorCode = readU32();
        evt = { kind: "diskWriteResp", id, ok, bytes: bytesWritten, errorCode };
      }
      break;
    }
    case EVT_TAG_FRAME_READY:
      evt = { kind: "frameReady", frameId: readU64() };
      break;
    case EVT_TAG_IRQ_RAISE:
      evt = { kind: "irqRaise", irq: readU8() };
      break;
    case EVT_TAG_IRQ_LOWER:
      evt = { kind: "irqLower", irq: readU8() };
      break;
    case EVT_TAG_A20_SET:
      evt = { kind: "a20Set", enabled: readU8() !== 0 };
      break;
    case EVT_TAG_RESET_REQUEST:
      evt = { kind: "resetRequest" };
      break;
    case EVT_TAG_LOG: {
      const level = logLevelFromU8(readU8());
      const len = readU32();
      const msg = decoder.decode(bytes.slice(off, off + len));
      off += len;
      evt = { kind: "log", level, message: msg };
      break;
    }
    case EVT_TAG_SERIAL_OUTPUT: {
      const port = readU16();
      const len = readU32();
      const data = bytes.slice(off, off + len);
      off += len;
      evt = { kind: "serialOutput", port, data };
      break;
    }
    case EVT_TAG_PANIC: {
      const len = readU32();
      const msg = decoder.decode(bytes.slice(off, off + len));
      off += len;
      evt = { kind: "panic", message: msg };
      break;
    }
    case EVT_TAG_TRIPLE_FAULT:
      evt = { kind: "tripleFault" };
      break;
    default:
      throw new Error(`unknown event tag 0x${tag.toString(16)}`);
  }
  if (off !== bytes.byteLength) throw new Error("trailing bytes in event");
  return evt;
}

function logLevelToU8(level: LogLevel): number {
  switch (level) {
    case "trace":
      return 0;
    case "debug":
      return 1;
    case "info":
      return 2;
    case "warn":
      return 3;
    case "error":
      return 4;
  }
}

function logLevelFromU8(v: number): LogLevel {
  switch (v) {
    case 0:
      return "trace";
    case 1:
      return "debug";
    case 2:
      return "info";
    case 3:
      return "warn";
    case 4:
      return "error";
    default:
      throw new Error(`invalid log level ${v}`);
  }
}
