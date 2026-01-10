import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import { encodeTcpMuxFrame, TcpMuxFrameParser, TCP_MUX_HEADER_BYTES, type TcpMuxFrame } from '../../src/protocol/tcpMux.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

function chunkBuffer(buf: Buffer, chunkSizes: readonly number[]): Buffer[] {
  if (buf.length === 0) return [Buffer.alloc(0)];
  const chunks: Buffer[] = [];
  let offset = 0;
  for (const size of chunkSizes) {
    if (offset >= buf.length) break;
    const next = Math.max(1, size);
    const end = Math.min(buf.length, offset + next);
    chunks.push(buf.subarray(offset, end));
    offset = end;
  }
  if (offset < buf.length) chunks.push(buf.subarray(offset));
  return chunks;
}

describe('tcp-mux frame codec (property)', () => {
  it(
    'valid frame streams round-trip encode -> parse across arbitrary chunking',
    { timeout: 10_000 },
    () => {
      const frameArb = fc
        .tuple(
          fc.integer({ min: 0, max: 255 }),
          fc.integer({ min: 0, max: 0xffffffff }),
          fc.array(fc.integer({ min: 0, max: 255 }), { minLength: 0, maxLength: 128 }),
        )
        .map(([msgType, streamId, payload]) => ({
          msgType,
          streamId,
          payload: Buffer.from(payload),
        }));

      fc.assert(
        fc.property(
          fc.array(frameArb, { minLength: 0, maxLength: 20 }),
          fc.array(fc.integer({ min: 1, max: 32 }), { minLength: 1, maxLength: 20 }),
          (frames, chunkSizes) => {
            const encodedStream = Buffer.concat(
              frames.map((frame) => encodeTcpMuxFrame(frame.msgType as any, frame.streamId, frame.payload)),
            );

            const parser = new TcpMuxFrameParser();
            const decoded: TcpMuxFrame[] = [];
            for (const chunk of chunkBuffer(encodedStream, chunkSizes)) {
              decoded.push(...parser.push(chunk));
            }

            assert.equal(parser.pendingBytes(), 0);
            assert.equal(decoded.length, frames.length);
            for (let i = 0; i < frames.length; i++) {
              const expected = frames[i]!;
              const actual = decoded[i]!;
              assert.equal(actual.msgType, expected.msgType);
              assert.equal(actual.streamId, expected.streamId >>> 0);
              assert.deepEqual(actual.payload, expected.payload);
            }
          },
        ),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'random byte sequences never throw and never grow the internal buffer beyond input',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(
          fc.array(fc.array(fc.integer({ min: 0, max: 255 }), { maxLength: 256 }), { minLength: 1, maxLength: 10 }),
          (chunksBytes) => {
            const parser = new TcpMuxFrameParser();
            let total = 0;

            for (const bytes of chunksBytes) {
              const chunk = Buffer.from(bytes);
              total += chunk.length;
              assert.doesNotThrow(() => parser.push(chunk));
              assert.ok(parser.pendingBytes() <= total);
            }

            for (const frame of parser.push(Buffer.alloc(0))) {
              assert.ok(frame.payload.length <= total);
              assert.ok(frame.payload.length >= 0);
              assert.ok(TCP_MUX_HEADER_BYTES + frame.payload.length <= total + TCP_MUX_HEADER_BYTES);
            }
          },
        ),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});
