export const READ_FRAME_INDEX: number;
export const WRITE_FRAME_INDEX: number;
export const UNDERRUN_COUNT_INDEX: number;
export const OVERRUN_COUNT_INDEX: number;

export const HEADER_U32_LEN: number;
export const HEADER_BYTES: number;

export function framesAvailable(readFrameIndex: number, writeFrameIndex: number): number;
export function framesAvailableClamped(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number;
export function framesFree(readFrameIndex: number, writeFrameIndex: number, capacityFrames: number): number;
export function getRingBufferLevelFrames(header: Uint32Array, capacityFrames: number): number;

