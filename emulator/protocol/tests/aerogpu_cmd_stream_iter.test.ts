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
  decodeCmdSetShaderConstantsIPayload,
  decodeCmdSetShaderConstantsIPayloadFromPacket,
  decodeCmdSetShaderConstantsBPayload,
  decodeCmdSetShaderConstantsBPayloadFromPacket,
  decodeCmdSetSamplersPayload,
  decodeCmdSetSamplersPayloadFromPacket,
  decodeCmdSetConstantBuffersPayload,
  decodeCmdSetConstantBuffersPayloadFromPacket,
  decodeCmdSetShaderResourceBuffersPayload,
  decodeCmdSetUnorderedAccessBuffersPayload,
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
  const shaderConstantsI = new Int32Array([-1, 2, 3, 4]);
  const shaderConstantsB = new Uint32Array([0, 1]);
  const bindings = [
    { buffer: 10, strideBytes: 16, offsetBytes: 0 },
    { buffer: 11, strideBytes: 32, offsetBytes: 64 },
  ];
  const samplers = new Uint32Array([10, 20, 30]);
  const constantBuffers = [
    { buffer: 100, offsetBytes: 0, sizeBytes: 64 },
    { buffer: 101, offsetBytes: 16, sizeBytes: 128 },
  ];

  w.createShaderDxbc(100, AerogpuShaderStage.Vertex, dxbcBytes);
  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 5, shaderConstants);
  w.setShaderConstantsI(AerogpuShaderStage.Pixel, 1, shaderConstantsI);
  w.setShaderConstantsB(AerogpuShaderStage.Pixel, 2, shaderConstantsB);
  w.createInputLayout(200, blobBytes);
  w.setVertexBuffers(1, bindings);
  w.setSamplers(AerogpuShaderStage.Pixel, 3, samplers);
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 1, constantBuffers);
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
      AerogpuCmdOpcode.SetShaderConstantsI,
      AerogpuCmdOpcode.SetShaderConstantsB,
      AerogpuCmdOpcode.CreateInputLayout,
      AerogpuCmdOpcode.SetVertexBuffers,
      AerogpuCmdOpcode.SetSamplers,
      AerogpuCmdOpcode.SetConstantBuffers,
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
    const decoded = decodeCmdSetShaderConstantsIPayload(extended, offset);
    assert.equal(decoded.stage, AerogpuShaderStage.Pixel);
    assert.equal(decoded.startRegister, 1);
    assert.equal(decoded.vec4Count, shaderConstantsI.length / 4);
    assert.deepEqual(Array.from(decoded.data), Array.from(shaderConstantsI));
    offset += packets[2]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetShaderConstantsBPayload(extended, offset);
    assert.equal(decoded.stage, AerogpuShaderStage.Pixel);
    assert.equal(decoded.startRegister, 2);
    assert.equal(decoded.boolCount, shaderConstantsB.length);
    // Bool constants are encoded as `u32[bool_count]` (one scalar per register).
    assert.deepEqual(Array.from(decoded.data), Array.from(shaderConstantsB));
    offset += packets[3]!.sizeBytes;
  }

  {
    const decoded = decodeCmdCreateInputLayoutBlob(extended, offset);
    assert.equal(decoded.inputLayoutHandle, 200);
    assert.deepEqual(decoded.blobBytes, blobBytes);
    offset += packets[4]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetVertexBuffersBindings(extended, offset);
    assert.equal(decoded.startSlot, 1);
    assert.deepEqual(decoded.bindings, bindings);
    offset += packets[5]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetSamplersPayload(extended, offset);
    assert.equal(decoded.shaderStage, AerogpuShaderStage.Pixel);
    assert.equal(decoded.startSlot, 3);
    assert.equal(decoded.samplerCount, samplers.length);
    assert.deepEqual(decoded.samplers, samplers);
    offset += packets[6]!.sizeBytes;
  }

  {
    const decoded = decodeCmdSetConstantBuffersPayload(extended, offset);
    assert.equal(decoded.shaderStage, AerogpuShaderStage.Vertex);
    assert.equal(decoded.startSlot, 1);
    assert.equal(decoded.bufferCount, constantBuffers.length);
    for (let i = 0; i < constantBuffers.length; i++) {
      const off = i * 16;
      assert.deepEqual(
        {
          buffer: decoded.bindings.getUint32(off + 0, true),
          offsetBytes: decoded.bindings.getUint32(off + 4, true),
          sizeBytes: decoded.bindings.getUint32(off + 8, true),
        },
        constantBuffers[i],
      );
    }
    offset += packets[7]!.sizeBytes;
  }

  {
    const decoded = decodeCmdUploadResourcePayload(extended, offset);
    assert.equal(decoded.resourceHandle, 300);
    assert.equal(decoded.offsetBytes, 0x1234n);
    assert.equal(decoded.sizeBytes, BigInt(uploadBytes.byteLength));
    assert.deepEqual(decoded.dataBytes, uploadBytes);
    offset += packets[8]!.sizeBytes;
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

  {
    const w = new AerogpuCmdWriter();
    w.setShaderConstantsI(AerogpuShaderStage.Pixel, 0, new Int32Array([1, 2, 3, 4]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // vec4_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetShaderConstantsIPayload(bytes, packetOffset), /too small/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setShaderConstantsB(AerogpuShaderStage.Pixel, 0, new Uint32Array([1]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // bool_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetShaderConstantsBPayload(bytes, packetOffset), /too small/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setSamplers(AerogpuShaderStage.Pixel, 0, new Uint32Array([1]));
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // sampler_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetSamplersPayload(bytes, packetOffset), /too small/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setConstantBuffers(AerogpuShaderStage.Vertex, 0, [{ buffer: 1, offsetBytes: 0, sizeBytes: 16 }]);
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // buffer_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetConstantBuffersPayload(bytes, packetOffset), /too small/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setShaderResourceBuffers(AerogpuShaderStage.Pixel, 0, [{ buffer: 1, offsetBytes: 0, sizeBytes: 16 }]);
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // buffer_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetShaderResourceBuffersPayload(bytes, packetOffset), /too small/);
  }

  {
    const w = new AerogpuCmdWriter();
    w.setUnorderedAccessBuffers(AerogpuShaderStage.Compute, 0, [
      { buffer: 1, offsetBytes: 0, sizeBytes: 16, initialCount: 0 },
    ]);
    const bytes = w.finish();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    // uav_count @ +16
    view.setUint32(packetOffset + 16, 2, true);
    assert.throws(() => decodeCmdSetUnorderedAccessBuffersPayload(bytes, packetOffset), /too small/);
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
  const shaderConstantsI = new Int32Array([-1, 2, 3, 4]);
  const shaderConstantsB = new Uint32Array([0, 1]);
  const bindings = [{ buffer: 10, strideBytes: 16, offsetBytes: 0 }];
  const samplers = new Uint32Array([10, 20]);
  const constantBuffers = [{ buffer: 100, offsetBytes: 0, sizeBytes: 64 }];

  w.createShaderDxbc(100, AerogpuShaderStage.Vertex, dxbcBytes);
  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 5, shaderConstants);
  w.setShaderConstantsI(AerogpuShaderStage.Pixel, 1, shaderConstantsI);
  w.setShaderConstantsB(AerogpuShaderStage.Pixel, 2, shaderConstantsB);
  w.createInputLayout(200, blobBytes);
  w.setVertexBuffers(1, bindings);
  w.setSamplers(AerogpuShaderStage.Pixel, 3, samplers);
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 1, constantBuffers);
  w.uploadResource(300, 0x1234n, uploadBytes);
  w.debugMarker("hello");

  const packets = Array.from(iterCmdStream(w.finish()));
  assert.equal(packets.length, 10);

  assert.deepEqual(decodeCmdCreateShaderDxbcPayloadFromPacket(packets[0]!).dxbcBytes, dxbcBytes);
  assert.deepEqual(
    Array.from(decodeCmdSetShaderConstantsFPayloadFromPacket(packets[1]!).data),
    Array.from(shaderConstants),
  );
  assert.deepEqual(
    Array.from(decodeCmdSetShaderConstantsIPayloadFromPacket(packets[2]!).data),
    Array.from(shaderConstantsI),
  );
  // Bool constants are encoded as `u32[bool_count]` (one scalar per register).
  assert.deepEqual(
    Array.from(decodeCmdSetShaderConstantsBPayloadFromPacket(packets[3]!).data),
    Array.from(shaderConstantsB),
  );
  assert.deepEqual(decodeCmdCreateInputLayoutBlobFromPacket(packets[4]!).blobBytes, blobBytes);
  assert.deepEqual(decodeCmdSetVertexBuffersBindingsFromPacket(packets[5]!).bindings, bindings);
  assert.deepEqual(decodeCmdSetSamplersPayloadFromPacket(packets[6]!).samplers, samplers);
  {
    const decoded = decodeCmdSetConstantBuffersPayloadFromPacket(packets[7]!);
    assert.equal(decoded.bufferCount, constantBuffers.length);
    assert.equal(decoded.bindings.getUint32(0, true), 100);
    assert.equal(decoded.bindings.getUint32(4, true), 0);
    assert.equal(decoded.bindings.getUint32(8, true), 64);
  }
  assert.deepEqual(decodeCmdUploadResourcePayloadFromPacket(packets[8]!).dataBytes, uploadBytes);
  assert.equal(decodeCmdDebugMarkerPayloadFromPacket(packets[9]!).marker, "hello");
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
