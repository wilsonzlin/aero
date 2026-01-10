import { err, ok, Result, safeResult } from './result.js';

export interface MuxFrame {
  type: number;
  channelId: number;
  payload: Uint8Array;
}

export const MUX_FRAME_HEADER_BYTES = 9;

export function encodeMuxFrame(frame: MuxFrame): Uint8Array {
  const header = MUX_FRAME_HEADER_BYTES;
  const out = new Uint8Array(header + frame.payload.length);
  out[0] = frame.type & 0xff;

  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint32(1, frame.channelId >>> 0, false);
  view.setUint32(5, frame.payload.length >>> 0, false);

  out.set(frame.payload, header);
  return out;
}

export interface DecodeMuxFrameOptions {
  maxPayloadSize: number;
}

export function decodeMuxFrame(
  buf: Uint8Array,
  opts: DecodeMuxFrameOptions = { maxPayloadSize: 1024 * 1024 },
): Result<MuxFrame> {
  return safeResult(() => {
    if (buf.length < MUX_FRAME_HEADER_BYTES) {
      return err('FRAME_TRUNCATED', 'Frame header is truncated');
    }

    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const type = view.getUint8(0);
    const channelId = view.getUint32(1, false);
    const length = view.getUint32(5, false);

    if (length > opts.maxPayloadSize) {
      return err('FRAME_TOO_LARGE', 'Frame payload is too large');
    }

    const available = buf.length - MUX_FRAME_HEADER_BYTES;
    if (available < length) {
      return err('FRAME_TRUNCATED', 'Frame payload is truncated');
    }

    const payload = buf.subarray(MUX_FRAME_HEADER_BYTES, MUX_FRAME_HEADER_BYTES + length);
    return ok({ type, channelId, payload });
  });
}
