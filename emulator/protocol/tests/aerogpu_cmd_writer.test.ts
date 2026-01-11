import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdWriter,
  AerogpuShaderStage,
  decodeCmdStreamHeader,
} from "../aerogpu/aerogpu_cmd.ts";

function alignUp(v: number, a: number): number {
  return (v + (a - 1)) & ~(a - 1);
}

test("AerogpuCmdWriter emits aligned packets and updates stream header size", () => {
  const w = new AerogpuCmdWriter();

  w.createBuffer(1, 0xdeadbeef, 1024n, 0, 0);
  w.createShaderDxbc(2, AerogpuShaderStage.Vertex, new Uint8Array([0xaa, 0xbb, 0xcc]));
  w.createInputLayout(3, new Uint8Array([0x11]));
  w.uploadResource(1, 16n, new Uint8Array([1, 2, 3, 4, 5]));
  w.setVertexBuffers(0, [
    { buffer: 10, strideBytes: 16, offsetBytes: 0 },
    { buffer: 11, strideBytes: 32, offsetBytes: 64 },
  ]);
  w.draw(3, 1, 0, 0);
  w.flush();

  const bytes = w.finish();
  assert.ok(bytes.byteLength >= AEROGPU_CMD_STREAM_HEADER_SIZE);

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const hdr = decodeCmdStreamHeader(view, 0);
  assert.equal(hdr.sizeBytes, bytes.byteLength);
  assert.equal(view.getUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, true), bytes.byteLength);

  // Walk packets and validate alignment / bounds.
  const opcodes: number[] = [];
  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;
  while (cursor < bytes.byteLength) {
    assert.ok(bytes.byteLength - cursor >= AEROGPU_CMD_HDR_SIZE);
    const opcode = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true);
    const sizeBytes = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
    assert.ok(sizeBytes >= AEROGPU_CMD_HDR_SIZE);
    assert.equal(sizeBytes % 4, 0);
    assert.ok(cursor + sizeBytes <= bytes.byteLength);

    opcodes.push(opcode);
    cursor += sizeBytes;
  }
  assert.equal(cursor, bytes.byteLength);

  const expected: Array<[number, number]> = [
    [AerogpuCmdOpcode.CreateBuffer, 40],
    [AerogpuCmdOpcode.CreateShaderDxbc, alignUp(24 + 3, 4)],
    [AerogpuCmdOpcode.CreateInputLayout, alignUp(20 + 1, 4)],
    [AerogpuCmdOpcode.UploadResource, alignUp(32 + 5, 4)],
    [AerogpuCmdOpcode.SetVertexBuffers, 16 + 2 * 16],
    [AerogpuCmdOpcode.Draw, 24],
    [AerogpuCmdOpcode.Flush, 16],
  ];

  cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;
  for (const [expectedOpcode, expectedSize] of expected) {
    const opcode = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true);
    const sizeBytes = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
    assert.equal(opcode, expectedOpcode);
    assert.equal(sizeBytes, expectedSize);
    cursor += expectedSize;
  }
  assert.equal(cursor, bytes.byteLength);
  assert.deepEqual(
    opcodes,
    expected.map(([opcode]) => opcode),
  );
});

