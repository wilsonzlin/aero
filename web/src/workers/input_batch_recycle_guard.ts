export const MAX_INPUT_BATCH_RECYCLE_BYTES = 4 * 1024 * 1024;

/**
 * Input batch recycling is a performance optimization: the worker can transfer the
 * `ArrayBuffer` back to the sender so it can be reused, avoiding allocations.
 *
 * Refuse to recycle extremely large buffers so a malicious or buggy sender cannot
 * force a recycle pool to retain unbounded memory.
 */
export function shouldRecycleInputBatchByteLength(byteLength: number, maxBytes = MAX_INPUT_BATCH_RECYCLE_BYTES): boolean {
  return (byteLength >>> 0) <= (maxBytes >>> 0);
}

export function shouldRecycleInputBatchBuffer(buffer: ArrayBuffer, maxBytes = MAX_INPUT_BATCH_RECYCLE_BYTES): boolean {
  return shouldRecycleInputBatchByteLength(buffer.byteLength, maxBytes);
}

