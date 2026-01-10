import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import { decodeMuxFrame, encodeMuxFrame, MUX_FRAME_HEADER_BYTES } from '../../src/muxFrame.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

function setLengthField(buf: Uint8Array, length: number): Uint8Array {
  const out = buf.slice();
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint32(5, length >>> 0, false);
  return out;
}

describe('ws mux frame codec (property)', () => {
  it(
    'valid frames round-trip encode -> decode',
    { timeout: 10_000 },
    () => {
      const frameArb = fc
        .tuple(
          fc.integer({ min: 0, max: 255 }),
          fc.integer({ min: 0, max: 0xffffffff }),
          fc.array(fc.integer({ min: 0, max: 255 }), { minLength: 0, maxLength: 128 }),
        )
        .map(([type, channelId, payload]) => ({ type, channelId, payload: Uint8Array.from(payload) }));

      fc.assert(
        fc.property(frameArb, (frame) => {
          const encoded = encodeMuxFrame(frame);
          const decoded = decodeMuxFrame(encoded, { maxPayloadSize: 1024 });
          assert.equal(decoded.ok, true);
          if (!decoded.ok) return;
          assert.equal(decoded.value.type, frame.type & 0xff);
          assert.equal(decoded.value.channelId, frame.channelId >>> 0);
          assert.deepEqual(Array.from(decoded.value.payload), Array.from(frame.payload));
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'invalid frames fail safely and enforce maxPayloadSize',
    { timeout: 10_000 },
    () => {
      const frameArb = fc
        .tuple(
          fc.integer({ min: 0, max: 255 }),
          fc.integer({ min: 0, max: 0xffffffff }),
          fc.array(fc.integer({ min: 0, max: 255 }), { minLength: 0, maxLength: 128 }),
        )
        .map(([type, channelId, payload]) => ({ type, channelId, payload: Uint8Array.from(payload) }));

      fc.assert(
        fc.property(frameArb, fc.integer({ min: 1, max: 256 }), (frame, maxPayloadSize) => {
          const encoded = encodeMuxFrame(frame);
          const tampered = setLengthField(encoded, maxPayloadSize + 1);
          const res = decodeMuxFrame(tampered, { maxPayloadSize });
          assert.equal(res.ok, false);
          if (!res.ok) assert.equal(res.code, 'FRAME_TOO_LARGE');
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'random byte sequences never throw and never claim unbounded payloads',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.array(fc.integer({ min: 0, max: 255 }), { maxLength: 256 }), (bytes) => {
          const buf = Uint8Array.from(bytes);
          const res = decodeMuxFrame(buf, { maxPayloadSize: 1024 });
          assert.equal(typeof res.ok, 'boolean');
          if (res.ok) {
            assert.ok(res.value.payload.length <= 1024);
            assert.ok(buf.length >= MUX_FRAME_HEADER_BYTES + res.value.payload.length);
          }
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});
