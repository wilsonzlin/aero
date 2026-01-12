export const WRITE_POS_INDEX = 0;
export const READ_POS_INDEX = 1;
export const DROPPED_SAMPLES_INDEX = 2;
export const CAPACITY_SAMPLES_INDEX = 3;

export const HEADER_U32_LEN = 4;
export const HEADER_BYTES = HEADER_U32_LEN * 4;

// Keep this in sync with `aero_platform::audio::mic_bridge`'s internal cap.
//
// This prevents accidental multi-gigabyte `SharedArrayBuffer` allocations if a caller passes an
// absurd `capacitySamples` (for example, from untrusted UI/config).
const MAX_MIC_RING_CAPACITY_SAMPLES = 1_048_576; // 2^20 mono samples (~21s @ 48kHz)

export function samplesAvailable(readPos, writePos) {
  return (writePos - readPos) >>> 0;
}

export function samplesAvailableClamped(readPos, writePos, capacity) {
  return Math.min(samplesAvailable(readPos, writePos), capacity >>> 0);
}

export function samplesFree(readPos, writePos, capacity) {
  return (capacity - samplesAvailableClamped(readPos, writePos, capacity)) >>> 0;
}

export function createMicRingBuffer(capacitySamples) {
  if (!Number.isSafeInteger(capacitySamples) || capacitySamples <= 0) {
    throw new Error(`invalid mic ring buffer capacity: ${capacitySamples}`);
  }
  if (capacitySamples > MAX_MIC_RING_CAPACITY_SAMPLES) {
    throw new Error(
      `invalid mic ring buffer capacity: ${capacitySamples} (max ${MAX_MIC_RING_CAPACITY_SAMPLES})`,
    );
  }

  const cap = capacitySamples >>> 0;
  const sab = new SharedArrayBuffer(HEADER_BYTES + cap * 4);
  const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
  const data = new Float32Array(sab, HEADER_BYTES, cap);

  // Use Atomics for indices so the buffer can be shared with the emulator worker safely.
  Atomics.store(header, WRITE_POS_INDEX, 0);
  Atomics.store(header, READ_POS_INDEX, 0);
  Atomics.store(header, DROPPED_SAMPLES_INDEX, 0);
  Atomics.store(header, CAPACITY_SAMPLES_INDEX, cap); // constant

  return { sab, header, data, capacity: cap };
}

export function micRingBufferReadInto(rb, out) {
  const readPos = Atomics.load(rb.header, READ_POS_INDEX) >>> 0;
  const writePos = Atomics.load(rb.header, WRITE_POS_INDEX) >>> 0;
  const available = samplesAvailableClamped(readPos, writePos, rb.capacity);
  const toRead = Math.min(out.length, available);
  if (toRead === 0) return 0;

  const start = readPos % rb.capacity;
  const firstPart = Math.min(toRead, rb.capacity - start);
  out.set(rb.data.subarray(start, start + firstPart), 0);
  const remaining = toRead - firstPart;
  if (remaining) {
    out.set(rb.data.subarray(0, remaining), firstPart);
  }

  Atomics.store(rb.header, READ_POS_INDEX, (readPos + toRead) >>> 0);
  return toRead;
}

export function micRingBufferWrite(rb, samples) {
  if (samples.length === 0) return 0;

  let writePos = Atomics.load(rb.header, WRITE_POS_INDEX) >>> 0;
  const readPos = Atomics.load(rb.header, READ_POS_INDEX) >>> 0;

  const used = samplesAvailable(readPos, writePos);
  if (used > rb.capacity) {
    // Consumer fell behind far enough that we no longer know what's valid.
    // Drop this block to avoid making things worse.
    Atomics.add(rb.header, DROPPED_SAMPLES_INDEX, samples.length);
    return 0;
  }

  const free = rb.capacity - used;
  if (free === 0) {
    Atomics.add(rb.header, DROPPED_SAMPLES_INDEX, samples.length);
    return 0;
  }

  const toWrite = Math.min(samples.length, free);
  const dropped = samples.length - toWrite;
  if (dropped) Atomics.add(rb.header, DROPPED_SAMPLES_INDEX, dropped);

  // Keep the most recent part of the block if we have to drop.
  const slice = dropped ? samples.subarray(dropped) : samples;

  const start = writePos % rb.capacity;
  const firstPart = Math.min(toWrite, rb.capacity - start);
  rb.data.set(slice.subarray(0, firstPart), start);
  const remaining = toWrite - firstPart;
  if (remaining) rb.data.set(slice.subarray(firstPart), 0);

  writePos = (writePos + toWrite) >>> 0;
  Atomics.store(rb.header, WRITE_POS_INDEX, writePos);
  return toWrite;
}
