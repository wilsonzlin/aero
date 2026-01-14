import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION,
  AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS,
  AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_PRESENT_FLAG_VSYNC,
  AerogpuCmdOpcode,
  AerogpuCmdWriter,
  cmdPacketHasVsyncPresent,
  cmdStreamHasVsyncPresent,
  decodeCmdStreamHeader,
  iterCmdStream,
} from "../aerogpu/aerogpu_cmd.ts";
import { AEROGPU_ABI_VERSION_U32 } from "../aerogpu/aerogpu_pci.ts";

function buildTruncatedPresentStream(): Uint8Array {
  // Stream header + PRESENT packet containing only scanout_id (missing flags).
  const streamSizeBytes = AEROGPU_CMD_STREAM_HEADER_SIZE + 12;
  const bytes = new Uint8Array(streamSizeBytes);
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, 0x444d4341, true); // "ACMD"
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, streamSizeBytes, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, 0, true);

  const off = AEROGPU_CMD_STREAM_HEADER_SIZE;
  dv.setUint32(off + 0, AerogpuCmdOpcode.Present, true);
  dv.setUint32(off + 4, 12, true); // cmd.size_bytes (hdr + scanout_id)
  dv.setUint32(off + 8, 0, true); // scanout_id
  return bytes;
}

test("cmdStreamHasVsyncPresent detects PRESENT flags", () => {
  const w = new AerogpuCmdWriter();
  w.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
  assert.equal(cmdStreamHasVsyncPresent(w.finish()), true);

  w.reset();
  w.present(0, 0);
  assert.equal(cmdStreamHasVsyncPresent(w.finish()), false);

  w.reset();
  w.presentEx(0, AEROGPU_PRESENT_FLAG_VSYNC, 0);
  assert.equal(cmdStreamHasVsyncPresent(w.finish()), true);
});

test("cmdStreamHasVsyncPresent accepts PRESENT packets extended with trailing fields", () => {
  const w = new AerogpuCmdWriter();
  w.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
  const base = w.finish();

  // Extend the packet by appending 4 bytes after flags and updating both size headers.
  const extraBytes = 4;
  const extended = new Uint8Array(base.byteLength + extraBytes);
  extended.set(base);

  const dv = new DataView(extended.buffer, extended.byteOffset, extended.byteLength);

  // Patch stream_header.size_bytes.
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, extended.byteLength, true);

  // Patch cmd_hdr.size_bytes of the first packet.
  const cmdHdrOff = AEROGPU_CMD_STREAM_HEADER_SIZE;
  const oldSize = dv.getUint32(cmdHdrOff + 4, true);
  dv.setUint32(cmdHdrOff + 4, oldSize + extraBytes, true);

  assert.equal(cmdStreamHasVsyncPresent(extended), true);
});

test("cmdPacketHasVsyncPresent rejects truncated PRESENT packets", () => {
  const bytes = buildTruncatedPresentStream();
  // Ensure the stream header is valid first (otherwise we'd be testing the iterator).
  assert.equal(decodeCmdStreamHeader(new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)).sizeBytes, bytes.byteLength);

  const [pkt] = [...iterCmdStream(bytes)];
  assert(pkt);
  assert.throws(() => cmdPacketHasVsyncPresent(pkt), /too small to contain flags/);
  assert.throws(() => cmdStreamHasVsyncPresent(bytes), /too small to contain flags/);
});
