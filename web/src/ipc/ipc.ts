import { IPC_MAGIC, IPC_VERSION, RECORD_ALIGN, alignUp, ipcHeader, queueDesc, ringCtrl } from "./layout.ts";
import { RingBuffer } from "./ring_buffer.ts";

export type IpcQueueSpec = Readonly<{
  kind: number;
  capacityBytes: number;
}>;

export type IpcQueueInfo = Readonly<{
  kind: number;
  offsetBytes: number;
  capacityBytes: number;
}>;

export type IpcInitResult = Readonly<{
  buffer: SharedArrayBuffer;
  queues: readonly IpcQueueInfo[];
}>;

export class IpcLayoutError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "IpcLayoutError";
  }
}

function writeU32(view: DataView, byteOffset: number, value: number): void {
  view.setUint32(byteOffset, value >>> 0, true);
}

function readU32(view: DataView, byteOffset: number): number {
  return view.getUint32(byteOffset, true) >>> 0;
}

function ensureU32(value: number, field: string): number {
  if (!Number.isFinite(value) || value < 0 || value > 0xffff_ffff) {
    throw new IpcLayoutError(`${field} must be a u32 (got ${value})`);
  }
  return value >>> 0;
}

function ensureAligned(value: number, align: number, field: string): void {
  if (value % align !== 0) {
    throw new IpcLayoutError(`${field} must be aligned to ${align} bytes (got ${value})`);
  }
}

export function createIpcBuffer(queueSpecs: readonly IpcQueueSpec[]): IpcInitResult {
  const queueCount = queueSpecs.length;
  ensureU32(queueCount, "queueCount");

  let offset = ipcHeader.BYTES + queueCount * queueDesc.BYTES;

  const queues: IpcQueueInfo[] = [];
  for (const spec of queueSpecs) {
    const kind = ensureU32(spec.kind, "queue.kind");
    const capacityBytes = ensureU32(spec.capacityBytes, "queue.capacityBytes");
    ensureAligned(capacityBytes, RECORD_ALIGN, "queue.capacityBytes");

    offset = alignUp(offset, RECORD_ALIGN);
    const ringOffset = offset;
    offset += ringCtrl.BYTES + capacityBytes;
    queues.push({ kind, offsetBytes: ringOffset, capacityBytes });
  }

  const totalBytes = ensureU32(offset, "totalBytes");

  const buffer = new SharedArrayBuffer(totalBytes);
  const view = new DataView(buffer);

  writeU32(view, ipcHeader.MAGIC * 4, IPC_MAGIC);
  writeU32(view, ipcHeader.VERSION * 4, IPC_VERSION);
  writeU32(view, ipcHeader.TOTAL_BYTES * 4, totalBytes);
  writeU32(view, ipcHeader.QUEUE_COUNT * 4, queueCount);

  for (let i = 0; i < queueCount; i++) {
    const q = queues[i]!;
    const base = ipcHeader.BYTES + i * queueDesc.BYTES;
    writeU32(view, base + queueDesc.KIND * 4, q.kind);
    writeU32(view, base + queueDesc.OFFSET_BYTES * 4, q.offsetBytes);
    writeU32(view, base + queueDesc.CAPACITY_BYTES * 4, q.capacityBytes);
    writeU32(view, base + queueDesc.RESERVED * 4, 0);

    // Initialize ring header.
    new Int32Array(buffer, q.offsetBytes, ringCtrl.WORDS).set([0, 0, 0, q.capacityBytes]);
  }

  return { buffer, queues };
}

export function parseIpcBuffer(buffer: SharedArrayBuffer): IpcInitResult {
  const view = new DataView(buffer);

  if (buffer.byteLength < ipcHeader.BYTES) {
    throw new IpcLayoutError("buffer too small for IPC header");
  }

  const magic = readU32(view, ipcHeader.MAGIC * 4);
  if (magic !== IPC_MAGIC) {
    throw new IpcLayoutError(`bad IPC magic (expected 0x${IPC_MAGIC.toString(16)}, got 0x${magic.toString(16)})`);
  }

  const version = readU32(view, ipcHeader.VERSION * 4);
  if (version !== IPC_VERSION) {
    throw new IpcLayoutError(`unsupported IPC version ${version} (expected ${IPC_VERSION})`);
  }

  const totalBytes = readU32(view, ipcHeader.TOTAL_BYTES * 4);
  if (totalBytes !== buffer.byteLength) {
    throw new IpcLayoutError(`buffer length mismatch (header=${totalBytes} actual=${buffer.byteLength})`);
  }

  const queueCount = readU32(view, ipcHeader.QUEUE_COUNT * 4);
  const descBytes = ipcHeader.BYTES + queueCount * queueDesc.BYTES;
  if (buffer.byteLength < descBytes) {
    throw new IpcLayoutError("buffer too small for queue descriptors");
  }

  const queues: IpcQueueInfo[] = [];
  for (let i = 0; i < queueCount; i++) {
    const base = ipcHeader.BYTES + i * queueDesc.BYTES;
    const kind = readU32(view, base + queueDesc.KIND * 4);
    const offsetBytes = readU32(view, base + queueDesc.OFFSET_BYTES * 4);
    const capacityBytes = readU32(view, base + queueDesc.CAPACITY_BYTES * 4);
    const reserved = readU32(view, base + queueDesc.RESERVED * 4);
    if (reserved !== 0) {
      throw new IpcLayoutError(`queue descriptor ${i} reserved field must be 0`);
    }
    ensureAligned(offsetBytes, RECORD_ALIGN, `queue[${i}].offsetBytes`);
    ensureAligned(capacityBytes, RECORD_ALIGN, `queue[${i}].capacityBytes`);
    if (offsetBytes + ringCtrl.BYTES + capacityBytes > buffer.byteLength) {
      throw new IpcLayoutError(`queue descriptor ${i} out of bounds`);
    }

    // Validate ring header capacity matches the descriptor.
    const ctrl = new Int32Array(buffer, offsetBytes, ringCtrl.WORDS);
    const ringCap = Atomics.load(ctrl, ringCtrl.CAPACITY) >>> 0;
    if (ringCap !== capacityBytes) {
      throw new IpcLayoutError(
        `queue descriptor ${i} capacity mismatch (desc=${capacityBytes} ringHeader=${ringCap})`,
      );
    }

    queues.push({ kind, offsetBytes, capacityBytes });
  }

  return { buffer, queues };
}

export function openRingByKind(
  buffer: SharedArrayBuffer,
  kind: number,
  nth = 0,
): RingBuffer {
  const { queues } = parseIpcBuffer(buffer);
  let seen = 0;
  for (const q of queues) {
    if (q.kind !== kind) continue;
    if (seen === nth) return new RingBuffer(buffer, q.offsetBytes);
    seen++;
  }
  throw new IpcLayoutError(`queue kind ${kind} not found`);
}
