import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdWriter,
  AerogpuShaderStage,
  decodeCmdDebugMarkerPayload,
  decodeCmdDebugMarkerPayloadFromPacket,
  decodeCmdCreateInputLayoutBlob,
  decodeCmdCreateInputLayoutBlobFromPacket,
  decodeCmdCreateShaderDxbcPayload,
  decodeCmdCreateShaderDxbcPayloadFromPacket,
  decodeCmdSetShaderConstantsFPayload,
  decodeCmdSetShaderConstantsFPayloadFromPacket,
  decodeCmdSetVertexBuffersBindings,
  decodeCmdSetVertexBuffersBindingsFromPacket,
  decodeCmdUploadResourcePayload,
  decodeCmdUploadResourcePayloadFromPacket,
  decodeCmdStreamView,
  iterCmdStream,
} from "../aerogpu/aerogpu_cmd.ts";

test("iterCmdStream yields packets and variable-payload decoders round-trip", () => {
  const w = new AerogpuCmdWriter();

  const dxbcBytes = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
  const blobBytes = new Uint8Array([0x11, 0x22, 0x33]);
  const uploadBytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
  const shaderConstants = new Float32Array([1.25, -2.5, 0, 42, 0.5, 1000, -0.25, 3.14]);
  const bindings = [
    { buffer: 10, strideBytes: 16, offsetBytes: 0 },
    { buffer: 11, strideBytes: 32, offsetBytes: 64 },
  ];

  w.createShaderDxbc(100, AerogpuShaderStage.Vertex, dxbcBytes);
  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 5, shaderConstants);
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
      AerogpuCmdOpcode.SetShaderConstantsF,
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
    const decoded = decodeCmdSetShaderConstantsFPayload(extended, offset);
    assert.equal(decoded.stage, AerogpuShaderStage.Pixel);
    assert.equal(decoded.startRegister, 5);
    assert.equal(decoded.vec4Count, shaderConstants.length / 4);
    assert.deepEqual(Array.from(decoded.data), Array.from(shaderConstants));
    offset += packets[1]!.sizeBytes;
  }

  {
    const decoded = decodeCmdCreateInputLayoutBlob(extended, offset);
    assert.equal(decoded.inputLayoutHandle, 200);
    assert.deepEqual(decoded.blobBytes, blobBytes);
    offset += packets[2]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetVertexBuffersBindings(extended, offset);
    assert.equal(decoded.startSlot, 1);
    assert.deepEqual(decoded.bindings, bindings);
    offset += packets[3]!.sizeBytes;
  }

  {
    const decoded = decodeCmdUploadResourcePayload(extended, offset);
    assert.equal(decoded.resourceHandle, 300);
    assert.equal(decoded.offsetBytes, 0x1234n);
    assert.equal(decoded.sizeBytes, BigInt(uploadBytes.byteLength));
    assert.deepEqual(decoded.dataBytes, uploadBytes);
    offset += packets[4]!.sizeBytes;
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

test("decodeCmdDebugMarkerPayload trims padding and decodes UTF-8", () => {
  const w = new AerogpuCmdWriter();
  w.debugMarker("hello");
  const stream = w.finish();

  const packets = Array.from(iterCmdStream(stream));
  assert.equal(packets.length, 1);
  assert.equal(packets[0]!.opcode, AerogpuCmdOpcode.DebugMarker);

  const decoded = decodeCmdDebugMarkerPayload(stream, AEROGPU_CMD_STREAM_HEADER_SIZE);
  assert.equal(decoded.marker, "hello");
  assert.deepEqual(decoded.markerBytes, new TextEncoder().encode("hello"));
});

test("variable-payload decoders reject size/count fields that would overrun packet size_bytes", () => {
  const packetOffset = AEROGPU_CMD_STREAM_HEADER_SIZE;

  {
    const w = new AerogpuCmdWriter();
    w.createShaderDxbc(1, AerogpuShaderStage.Vertex, new Uint8Array([1, 2, 3, 4]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // dxbc_size_bytes @ +16
    view.setUint32(packetOffset + 16, 5, true); // claims larger than packet provides
    assert.throws(() => decodeCmdCreateShaderDxbcPayload(bytes, packetOffset), /size mismatch/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.createInputLayout(1, new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // blob_size_bytes @ +12
    view.setUint32(packetOffset + 12, 5, true);
    assert.throws(() => decodeCmdCreateInputLayoutBlob(bytes, packetOffset), /size mismatch/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.uploadResource(1, 0n, new Uint8Array([1, 2, 3, 4]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // size_bytes (u64) @ +24
    view.setBigUint64(packetOffset + 24, 5n, true);
    assert.throws(() => decodeCmdUploadResourcePayload(bytes, packetOffset), /size mismatch/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setVertexBuffers(0, [{ buffer: 1, strideBytes: 4, offsetBytes: 0 }]);
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // buffer_count @ +12
    view.setUint32(packetOffset + 12, 2, true);
    assert.throws(() => decodeCmdSetVertexBuffersBindings(bytes, packetOffset), /size mismatch/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setShaderConstantsF(AerogpuShaderStage.Pixel, 0, new Float32Array([1, 2, 3, 4]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // vec4_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetShaderConstantsFPayload(bytes, packetOffset), /too small/);
  }
});

test("decodeCmdStreamView collects header + packets", () => {
  const w = new AerogpuCmdWriter();
  w.createBuffer(1, 0, 16n, 0, 0);
  w.draw(3, 1, 0, 0);
  w.flush();

  const bytes = w.finish();
  const view = decodeCmdStreamView(bytes);

  assert.equal(view.header.sizeBytes, bytes.byteLength);
  assert.deepEqual(
    view.packets.map((p) => p.opcode),
    [AerogpuCmdOpcode.CreateBuffer, AerogpuCmdOpcode.Draw, AerogpuCmdOpcode.Flush],
  );
});

test("variable-payload decoders accept AerogpuCmdPacket from iterCmdStream", () => {
  const w = new AerogpuCmdWriter();
  const dxbcBytes = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]);
  const blobBytes = new Uint8Array([0x11, 0x22]);
  const uploadBytes = new Uint8Array([1, 2, 3, 4]);
  const shaderConstants = new Float32Array([1, 2, 3, 4]);
  const bindings = [{ buffer: 10, strideBytes: 16, offsetBytes: 0 }];

  w.createShaderDxbc(100, AerogpuShaderStage.Vertex, dxbcBytes);
  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 5, shaderConstants);
  w.createInputLayout(200, blobBytes);
  w.setVertexBuffers(1, bindings);
  w.uploadResource(300, 0x1234n, uploadBytes);
  w.debugMarker("hello");

  const packets = Array.from(iterCmdStream(w.finish()));
  assert.equal(packets.length, 6);

  assert.deepEqual(decodeCmdCreateShaderDxbcPayloadFromPacket(packets[0]!).dxbcBytes, dxbcBytes);
  assert.deepEqual(
    Array.from(decodeCmdSetShaderConstantsFPayloadFromPacket(packets[1]!).data),
    Array.from(shaderConstants),
  );
  assert.deepEqual(decodeCmdCreateInputLayoutBlobFromPacket(packets[2]!).blobBytes, blobBytes);
  assert.deepEqual(decodeCmdSetVertexBuffersBindingsFromPacket(packets[3]!).bindings, bindings);
  assert.deepEqual(decodeCmdUploadResourcePayloadFromPacket(packets[4]!).dataBytes, uploadBytes);
  assert.equal(decodeCmdDebugMarkerPayloadFromPacket(packets[5]!).marker, "hello");
});

test("packet-based decoders validate cmd.size_bytes invariants", () => {
  assert.throws(
    () =>
      decodeCmdDebugMarkerPayloadFromPacket({
        opcode: AerogpuCmdOpcode.DebugMarker,
        // Not 4-byte aligned.
        sizeBytes: 10,
        payload: new Uint8Array([0x61, 0x62]), // "ab"
      }),
    /not 4-byte aligned/,
  );
});
