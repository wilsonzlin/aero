import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdWriter,
  AerogpuShaderStage,
  decodeCmdCreateInputLayoutBlob,
  decodeCmdCreateShaderDxbcPayload,
  decodeCmdSetVertexBuffersBindings,
  decodeCmdUploadResourcePayload,
  iterCmdStream,
} from "../aerogpu/aerogpu_cmd.ts";

test("iterCmdStream yields packets and variable-payload decoders round-trip", () => {
  const w = new AerogpuCmdWriter();

  const dxbcBytes = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
  const blobBytes = new Uint8Array([0x11, 0x22, 0x33]);
  const uploadBytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
  const bindings = [
    { buffer: 10, strideBytes: 16, offsetBytes: 0 },
    { buffer: 11, strideBytes: 32, offsetBytes: 64 },
  ];

  w.createShaderDxbc(100, AerogpuShaderStage.Vertex, dxbcBytes);
  w.createInputLayout(200, blobBytes);
  w.setVertexBuffers(1, bindings);
  w.uploadResource(300, 0x1234n, uploadBytes);

  const stream = w.finish();

  // `cmd_stream.size_bytes` should define the stream boundary, not the backing buffer size.
  const extended = new Uint8Array(stream.byteLength + 16);
  extended.set(stream, 0);
  extended.fill(0xee, stream.byteLength);

  const packets = Array.from(iterCmdStream(extended));
  assert.deepEqual(
    packets.map((p) => p.opcode),
    [
      AerogpuCmdOpcode.CreateShaderDxbc,
      AerogpuCmdOpcode.CreateInputLayout,
      AerogpuCmdOpcode.SetVertexBuffers,
      AerogpuCmdOpcode.UploadResource,
    ],
  );

  let offset = AEROGPU_CMD_STREAM_HEADER_SIZE;

  {
    const decoded = decodeCmdCreateShaderDxbcPayload(extended, offset);
    assert.equal(decoded.shaderHandle, 100);
    assert.equal(decoded.stage, AerogpuShaderStage.Vertex);
    assert.deepEqual(decoded.dxbcBytes, dxbcBytes);
    offset += packets[0]!.sizeBytes;
  }

  {
    const decoded = decodeCmdCreateInputLayoutBlob(extended, offset);
    assert.equal(decoded.inputLayoutHandle, 200);
    assert.deepEqual(decoded.blobBytes, blobBytes);
    offset += packets[1]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetVertexBuffersBindings(extended, offset);
    assert.equal(decoded.startSlot, 1);
    assert.deepEqual(decoded.bindings, bindings);
    offset += packets[2]!.sizeBytes;
  }

  {
    const decoded = decodeCmdUploadResourcePayload(extended, offset);
    assert.equal(decoded.resourceHandle, 300);
    assert.equal(decoded.offsetBytes, 0x1234n);
    assert.equal(decoded.sizeBytes, BigInt(uploadBytes.byteLength));
    assert.deepEqual(decoded.dataBytes, uploadBytes);
    offset += packets[3]!.sizeBytes;
  }

  assert.equal(offset, stream.byteLength);
});

test("iterCmdStream rejects streams where a packet overruns cmd_stream.size_bytes", () => {
  const w = new AerogpuCmdWriter();
  w.createShaderDxbc(1, AerogpuShaderStage.Vertex, new Uint8Array([0xaa, 0xbb, 0xcc]));
  const stream = w.finish();

  const bad = stream.slice();
  const view = new DataView(bad.buffer, bad.byteOffset, bad.byteLength);
  view.setUint32(
    AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
    AEROGPU_CMD_STREAM_HEADER_SIZE + AEROGPU_CMD_HDR_SIZE,
    true,
  );

  assert.throws(() => Array.from(iterCmdStream(bad)), /overruns stream/);
});

