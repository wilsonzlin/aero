export const READ_FRAME_INDEX = 0;
export const WRITE_FRAME_INDEX = 1;
export const UNDERRUN_COUNT_INDEX = 2;
export const OVERRUN_COUNT_INDEX = 3;

export const HEADER_U32_LEN = 4;
export const HEADER_BYTES = HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;

export type AudioWorkletRingBufferViews = {
  header: Uint32Array;
  readIndex: Uint32Array;
  writeIndex: Uint32Array;
  underrunCount: Uint32Array;
  overrunCount: Uint32Array;
  samples: Float32Array;
};

export function framesAvailable(readFrameIndex: number, writeFrameIndex: number): number {
  return (writeFrameIndex - readFrameIndex) >>> 0;
}

export function framesAvailableClamped(
  readFrameIndex: number,
  writeFrameIndex: number,
  capacityFrames: number,
): number {
  return Math.min(framesAvailable(readFrameIndex, writeFrameIndex), capacityFrames);
}

export function framesFree(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number {
  return capacityFrames - framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
}

export function getRingBufferLevelFrames(header: Uint32Array, capacityFrames: number): number {
  const read = Atomics.load(header, READ_FRAME_INDEX) >>> 0;
  const write = Atomics.load(header, WRITE_FRAME_INDEX) >>> 0;
  return framesAvailableClamped(read, write, capacityFrames);
}

export function requiredBytes(capacityFrames: number, channelCount: number): number {
  const sampleCapacity = capacityFrames * channelCount;
  return HEADER_BYTES + sampleCapacity * Float32Array.BYTES_PER_ELEMENT;
}

export function wrapRingBuffer(
  sab: SharedArrayBuffer,
  capacityFrames: number,
  channelCount: number,
): AudioWorkletRingBufferViews {
  const sampleCapacity = capacityFrames * channelCount;
  const bytes = requiredBytes(capacityFrames, channelCount);
  if (sab.byteLength < bytes) {
    throw new Error(`Provided ring buffer is too small: need ${bytes} bytes, got ${sab.byteLength} bytes`);
  }

  const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
  const samples = new Float32Array(sab, HEADER_BYTES, sampleCapacity);

  return {
    header,
    readIndex: header.subarray(READ_FRAME_INDEX, READ_FRAME_INDEX + 1),
    writeIndex: header.subarray(WRITE_FRAME_INDEX, WRITE_FRAME_INDEX + 1),
    underrunCount: header.subarray(UNDERRUN_COUNT_INDEX, UNDERRUN_COUNT_INDEX + 1),
    overrunCount: header.subarray(OVERRUN_COUNT_INDEX, OVERRUN_COUNT_INDEX + 1),
    samples,
  };
}
