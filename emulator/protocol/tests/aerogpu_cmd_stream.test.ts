import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdStreamIter,
  AerogpuCmdWriter,
} from "../aerogpu/aerogpu_cmd.ts";

test("AerogpuCmdStreamIter walks packets and exposes payload slices", () => {
  const w = new AerogpuCmdWriter();
  w.createBuffer(1, 0, 1024n, 0, 0);
  w.draw(3, 1, 0, 0);
  w.flush();

  const bytes = w.finish();
  const iter = new AerogpuCmdStreamIter(bytes);
  assert.equal(iter.header.sizeBytes, bytes.byteLength);

  const packets = [...iter];
  assert.deepEqual(
    packets.map((p) => p.hdr.opcode),
    [AerogpuCmdOpcode.CreateBuffer, AerogpuCmdOpcode.Draw, AerogpuCmdOpcode.Flush],
  );

  // First packet starts immediately after the stream header.
  assert.equal(packets[0]!.offsetBytes, AEROGPU_CMD_STREAM_HEADER_SIZE);
  assert.equal(packets[0]!.endBytes, AEROGPU_CMD_STREAM_HEADER_SIZE + packets[0]!.hdr.sizeBytes);
  assert.equal(packets[0]!.payload.byteLength, packets[0]!.hdr.sizeBytes - AEROGPU_CMD_HDR_SIZE);
});

test("AerogpuCmdStreamIter preserves unknown opcodes", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_SIZE + AEROGPU_CMD_HDR_OFF_OPCODE, 0xdeadbeef, true);

  const packets = [...new AerogpuCmdStreamIter(bytes)];
  assert.equal(packets.length, 1);
  assert.equal(packets[0]!.hdr.opcode, 0xdeadbeef >>> 0);
});

test("AerogpuCmdStreamIter rejects misaligned packet size_bytes", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_SIZE + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, 10, true);

  assert.throws(() => {
    // Exhaust the iterator to force header decode.
    [...new AerogpuCmdStreamIter(bytes)];
  }, /not 4-byte aligned/);
});

test("AerogpuCmdStreamIter rejects misaligned cmd_stream.size_bytes", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, bytes.byteLength - 1, true);

  assert.throws(() => new AerogpuCmdStreamIter(bytes), /cmd_stream\.size_bytes is not 4-byte aligned/);
});

test("AerogpuCmdStreamIter rejects packets that overrun the declared stream size", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();

  // Inflate the first packet's size so it would extend past stream_header.size_bytes.
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(AEROGPU_CMD_STREAM_HEADER_SIZE + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, bytes.byteLength, true);

  assert.throws(() => {
    [...new AerogpuCmdStreamIter(bytes)];
  }, /overruns stream/);
});

test("AerogpuCmdStreamIter rejects buffers shorter than stream_header.size_bytes", () => {
  const w = new AerogpuCmdWriter();
  w.flush();
  const bytes = w.finish();

  const truncated = bytes.subarray(0, bytes.byteLength - 1);
  assert.throws(() => new AerogpuCmdStreamIter(truncated), /Buffer too small/);
});
