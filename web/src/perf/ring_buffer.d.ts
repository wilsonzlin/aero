export const RING_BUFFER_MAGIC: number;
export const RING_BUFFER_VERSION: number;

export const OverflowStrategy: Readonly<{
  DropNewest: 0;
}>;

export const RING_BUFFER_HEADER_I32: number;
export const RING_BUFFER_HEADER_BYTES: number;

export function createSpscRingBufferSharedArrayBuffer(options: {
  capacity: number;
  recordSize: number;
  overflowStrategy?: number;
}): SharedArrayBuffer;

export class SpscRingBuffer {
  constructor(sharedArrayBuffer: SharedArrayBuffer, options?: { expectedRecordSize?: number });

  getDroppedCount(): number;
  getCapacity(): number;
  getRecordSize(): number;
  reset(): void;
  availableRead(): number;
  tryWriteRecord(encodeFn: (view: DataView, byteOffset: number) => void): boolean;
  tryReadRecord<T>(decodeFn: (view: DataView, byteOffset: number) => T): T | null;
  drain(maxRecords: number, onRecord: (view: DataView, byteOffset: number) => void): number;
}

