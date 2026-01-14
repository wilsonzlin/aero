import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_BIND_SHADERS_SIZE,
  AEROGPU_CMD_BIND_SHADERS_EX_SIZE,
  AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE,
  AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE,
  AEROGPU_CMD_PRESENT_EX_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE,
  AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE,
  AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE,
  AEROGPU_CMD_SET_RENDER_STATE_SIZE,
  AEROGPU_CMD_SET_SAMPLER_STATE_SIZE,
  AEROGPU_CMD_CREATE_SAMPLER_SIZE,
  AEROGPU_CMD_DESTROY_SAMPLER_SIZE,
  AEROGPU_CMD_SET_SAMPLERS_SIZE,
  AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE,
  AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE,
  AEROGPU_CMD_SET_TEXTURE_SIZE,
  AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE,
  AerogpuCmdOpcode,
  AerogpuCmdWriter,
  AerogpuBlendFactor,
  AerogpuBlendOp,
  AerogpuCompareFunc,
  AerogpuCullMode,
  AerogpuFillMode,
  AerogpuSamplerAddressMode,
  AerogpuSamplerFilter,
  AerogpuShaderStage,
  AerogpuShaderStageEx,
  alignUp,
  decodeCmdBindShadersPayloadFromPacket,
  decodeCmdCreateShaderDxbcPayloadFromPacket,
  decodeCmdDispatchPayload,
  decodeCmdSetConstantBuffersPayload,
  decodeCmdSetSamplersPayload,
  decodeCmdSetShaderResourceBuffersPayload,
  decodeCmdSetShaderResourceBuffersPayloadFromPacket,
  decodeCmdSetUnorderedAccessBuffersPayload,
  decodeCmdSetUnorderedAccessBuffersPayloadFromPacket,
  decodeShaderStageEx,
  decodeCmdStreamView,
  decodeStageEx,
  decodeCmdStreamHeader,
  encodeStageEx,
  iterCmdStream,
  resolveShaderStageWithEx,
} from "../aerogpu/aerogpu_cmd.ts";

test("AerogpuCmdWriter emits aligned packets and updates stream header size", () => {
  const w = new AerogpuCmdWriter();

  w.createBuffer(1, 0xdeadbeef, 1024n, 0, 0);
  w.createShaderDxbc(2, AerogpuShaderStage.Vertex, new Uint8Array([0xaa, 0xbb, 0xcc]));
  w.createShaderDxbcEx(4, AerogpuShaderStageEx.Geometry, new Uint8Array([0xdd, 0xee]));
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
    [AerogpuCmdOpcode.CreateShaderDxbc, alignUp(24 + 2, 4)],
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

  // Validate CREATE_SHADER_DXBC reserved0 invariants and stage_ex encoding.
  const shader0Base = AEROGPU_CMD_STREAM_HEADER_SIZE + expected[0][1];
  // legacy CREATE_SHADER_DXBC leaves reserved0=0.
  assert.equal(view.getUint32(shader0Base + 20, true), 0);

  const shader1Base = shader0Base + expected[1][1];
  assert.equal(view.getUint32(shader1Base + 8, true), 4); // shader_handle
  assert.equal(view.getUint32(shader1Base + 12, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(shader1Base + 16, true), 2); // dxbc_size_bytes
  assert.equal(view.getUint32(shader1Base + 20, true), AerogpuShaderStageEx.Geometry);
  assert.deepEqual(Array.from(bytes.subarray(shader1Base + 24, shader1Base + 26)), [0xdd, 0xee]);
});

test("AerogpuCmdWriter.createShaderDxbcEx encodes stage in reserved0 and pads to 4-byte alignment", () => {
  const w = new AerogpuCmdWriter();
  const stageEx = 4;
  const dxbc = new Uint8Array([0xaa, 0xbb, 0xcc]);

  w.createShaderDxbcEx(7, stageEx, dxbc);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateShaderDxbc);
  const sizeBytes = view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  assert.equal(sizeBytes, alignUp(24 + dxbc.byteLength, 4));

  assert.equal(view.getUint32(pkt0 + 8, true), 7);
  assert.equal(view.getUint32(pkt0 + 12, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(pkt0 + 16, true), dxbc.byteLength);
  assert.equal(view.getUint32(pkt0 + 20, true), stageEx);

  assert.deepEqual(bytes.subarray(pkt0 + 24, pkt0 + 24 + dxbc.byteLength), dxbc);

  // Padding must be zero-filled.
  for (let i = pkt0 + 24 + dxbc.byteLength; i < pkt0 + sizeBytes; i++) {
    assert.equal(bytes[i], 0);
  }
});

test("AerogpuCmdWriter emits pipeline and binding packets", () => {
  const w = new AerogpuCmdWriter();

  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 4, new Float32Array([1, 2, 3, 4]));
  w.setShaderConstantsI(AerogpuShaderStage.Pixel, 1, new Int32Array([-1, 2, 3, 4]));
  w.setShaderConstantsB(AerogpuShaderStage.Pixel, 2, new Uint32Array([0, 1]));
  w.setTexture(AerogpuShaderStage.Pixel, 0, 99);
  w.setSamplerState(AerogpuShaderStage.Pixel, 0, 7, 42);
  w.setRenderState(10, 20);
  w.setBlendState(true, AerogpuBlendFactor.One, AerogpuBlendFactor.Zero, AerogpuBlendOp.Add, 0xf);
  w.setDepthStencilState(true, true, AerogpuCompareFunc.LessEqual, false, 0xaa, 0xbb);
  w.setRasterizerState(AerogpuFillMode.Solid, AerogpuCullMode.Back, false, true, -1);
  w.presentEx(0, 0, 0x12345678);
  w.exportSharedSurface(55, 0x0102030405060708n);
  w.importSharedSurface(56, 0x0102030405060708n);
  w.releaseSharedSurface(0x0102030405060708n);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const hdr = decodeCmdStreamHeader(view, 0);
  assert.equal(hdr.sizeBytes, bytes.byteLength);

  const expected: Array<[number, number]> = [
    [AerogpuCmdOpcode.SetShaderConstantsF, AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16],
    [AerogpuCmdOpcode.SetShaderConstantsI, AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + 16],
    [AerogpuCmdOpcode.SetShaderConstantsB, AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + 8],
    [AerogpuCmdOpcode.SetTexture, AEROGPU_CMD_SET_TEXTURE_SIZE],
    [AerogpuCmdOpcode.SetSamplerState, AEROGPU_CMD_SET_SAMPLER_STATE_SIZE],
    [AerogpuCmdOpcode.SetRenderState, AEROGPU_CMD_SET_RENDER_STATE_SIZE],
    [AerogpuCmdOpcode.SetBlendState, AEROGPU_CMD_SET_BLEND_STATE_SIZE],
    [AerogpuCmdOpcode.SetDepthStencilState, AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE],
    [AerogpuCmdOpcode.SetRasterizerState, AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE],
    [AerogpuCmdOpcode.PresentEx, AEROGPU_CMD_PRESENT_EX_SIZE],
    [AerogpuCmdOpcode.ExportSharedSurface, AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE],
    [AerogpuCmdOpcode.ImportSharedSurface, AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE],
    [AerogpuCmdOpcode.ReleaseSharedSurface, AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE],
    [AerogpuCmdOpcode.Flush, 16],
  ];

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;
  for (const [expectedOpcode, expectedSize] of expected) {
    const opcode = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true);
    const sizeBytes = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
    assert.equal(opcode, expectedOpcode);
    assert.equal(sizeBytes, expectedSize);
    cursor += expectedSize;
  }
  assert.equal(cursor, bytes.byteLength);

  // Validate variable-length constants packets.
  const pkt0Base = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0Base + 16, true), 1);
  assert.equal(view.getFloat32(pkt0Base + AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE, true), 1);

  const pkt1Base = pkt0Base + expected[0][1];
  assert.equal(view.getUint32(pkt1Base + 16, true), 1);
  assert.equal(view.getInt32(pkt1Base + AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE, true), -1);

  const pkt2Base = pkt1Base + expected[1][1];
  assert.equal(view.getUint32(pkt2Base + 16, true), 2);
  const bPayloadBase = pkt2Base + AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE;
  // Payload is scalar u32 values (0/1), one per bool register.
  assert.equal(view.getUint32(bPayloadBase + 0, true), 0);
  assert.equal(view.getUint32(bPayloadBase + 4, true), 1);

  // Validate byte-sized fields within nested state structs.
  const preBlendSize = expected
    .slice(0, 6)
    .reduce((acc, [, sizeBytes]) => acc + sizeBytes, 0);
  const blendBase = pkt0Base + preBlendSize;
  // `aerogpu_cmd_set_blend_state`:
  // - hdr @ 0
  // - state.color_write_mask @ 8 + 16
  assert.equal(view.getUint8(blendBase + 24), 0xf);
  // Separate-alpha defaults to the color component for the TS cmd writer helper.
  assert.equal(view.getUint32(blendBase + 28, true), AerogpuBlendFactor.One);
  assert.equal(view.getUint32(blendBase + 32, true), AerogpuBlendFactor.Zero);
  assert.equal(view.getUint32(blendBase + 36, true), AerogpuBlendOp.Add);
  // Blend constant defaults to 1.0 and sample mask defaults to 0xFFFF_FFFF.
  assert.equal(view.getFloat32(blendBase + 40, true), 1.0);
  assert.equal(view.getUint32(blendBase + 56, true), 0xffff_ffff);

  const depthBase = blendBase + expected[6][1];
  assert.equal(view.getUint8(depthBase + 24), 0xaa);
  assert.equal(view.getUint8(depthBase + 25), 0xbb);

  const rastBase = depthBase + expected[7][1];
  assert.equal(view.getInt32(rastBase + 24, true), -1);
});

test("AerogpuCmdWriter.setShaderConstantsI emits vec4-aligned int32 payload", () => {
  const w = new AerogpuCmdWriter();
  const data = new Int32Array([1, -2, 3, -4, 5, 6, -7, 8]);
  w.setShaderConstantsI(AerogpuShaderStage.Pixel, 5, data);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetShaderConstantsI);
  assert.equal(
    view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + data.length * 4,
  );
  assert.equal(view.getUint32(pkt0 + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(pkt0 + 12, true), 5);
  assert.equal(view.getUint32(pkt0 + 16, true), 2);
  assert.equal(view.getUint32(pkt0 + 20, true), 0);

  for (let i = 0; i < data.length; i++) {
    assert.equal(view.getInt32(pkt0 + AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + i * 4, true), data[i]);
  }
});

test("AerogpuCmdWriter.setShaderConstantsB encodes bool regs as scalar u32 values (0/1)", () => {
  const w = new AerogpuCmdWriter();
  const data = [false, true];
  w.setShaderConstantsB(AerogpuShaderStage.Vertex, 7, data);
  const boolCount = data.length;

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetShaderConstantsB);
  assert.equal(
    view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + boolCount * 4,
  );
  assert.equal(view.getUint32(pkt0 + 8, true), AerogpuShaderStage.Vertex);
  assert.equal(view.getUint32(pkt0 + 12, true), 7);
  assert.equal(view.getUint32(pkt0 + 16, true), boolCount);
  assert.equal(view.getUint32(pkt0 + 20, true), 0);

  // Register 0: false -> 0.
  const payloadBase = pkt0 + AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE;
  assert.equal(view.getUint32(payloadBase + 0, true), 0);

  // Register 1: true -> 1.
  assert.equal(view.getUint32(payloadBase + 4, true), 1);
});

test("AerogpuCmdWriter emits copy packets", () => {
  const w = new AerogpuCmdWriter();
  w.copyBuffer(1, 2, 3n, 4n, 5n, 1);
  w.copyTexture2d(10, 11, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  // COPY_BUFFER
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CopyBuffer);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 48);
  assert.equal(view.getUint32(pkt0 + 8, true), 1);
  assert.equal(view.getUint32(pkt0 + 12, true), 2);
  assert.equal(view.getBigUint64(pkt0 + 16, true), 3n);
  assert.equal(view.getBigUint64(pkt0 + 24, true), 4n);
  assert.equal(view.getBigUint64(pkt0 + 32, true), 5n);
  assert.equal(view.getUint32(pkt0 + 40, true), 1);

  // COPY_TEXTURE2D
  const pkt1 = pkt0 + 48;
  assert.equal(view.getUint32(pkt1 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CopyTexture2d);
  assert.equal(view.getUint32(pkt1 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 64);
  assert.equal(view.getUint32(pkt1 + 48, true), 7);
  assert.equal(view.getUint32(pkt1 + 52, true), 8);

  // FLUSH
  const pkt2 = pkt1 + 64;
  assert.equal(view.getUint32(pkt2 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Flush);
  assert.equal(view.getUint32(pkt2 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 16);
  assert.equal(pkt2 + 16, bytes.byteLength);
});

test("AerogpuCmdWriter emits GEOMETRY-stage binding packets (reserved0=0)", () => {
  const w = new AerogpuCmdWriter();

  w.setTexture(AerogpuShaderStage.Geometry, 7, 123);
  w.setSamplers(AerogpuShaderStage.Geometry, 2, [42, 43]);
  w.setConstantBuffers(AerogpuShaderStage.Geometry, 1, [{ buffer: 11, offsetBytes: 16, sizeBytes: 32 }]);
  w.setShaderResourceBuffers(AerogpuShaderStage.Geometry, 3, [{ buffer: 55, offsetBytes: 0, sizeBytes: 0 }]);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetTexture);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_SET_TEXTURE_SIZE);
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Geometry);
  assert.equal(view.getUint32(cursor + 12, true), 7);
  assert.equal(view.getUint32(cursor + 16, true), 123);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetSamplers);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_SET_SAMPLERS_SIZE + 8);
  const samplers = decodeCmdSetSamplersPayload(bytes, cursor);
  assert.equal(samplers.shaderStage, AerogpuShaderStage.Geometry);
  assert.equal(samplers.startSlot, 2);
  assert.equal(samplers.samplerCount, 2);
  assert.equal(samplers.reserved0, 0);
  assert.deepEqual(Array.from(samplers.samplers), [42, 43]);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 8;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetConstantBuffers);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16);
  const cbs = decodeCmdSetConstantBuffersPayload(bytes, cursor);
  assert.equal(cbs.shaderStage, AerogpuShaderStage.Geometry);
  assert.equal(cbs.startSlot, 1);
  assert.equal(cbs.bufferCount, 1);
  assert.equal(cbs.reserved0, 0);
  assert.equal(cbs.bindings.getUint32(0, true), 11);
  assert.equal(cbs.bindings.getUint32(4, true), 16);
  assert.equal(cbs.bindings.getUint32(8, true), 32);
  assert.equal(cbs.bindings.getUint32(12, true), 0);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetShaderResourceBuffers);
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16,
  );
  const srvs = decodeCmdSetShaderResourceBuffersPayload(bytes, cursor);
  assert.equal(srvs.shaderStage, AerogpuShaderStage.Geometry);
  assert.equal(srvs.startSlot, 3);
  assert.equal(srvs.bufferCount, 1);
  assert.equal(srvs.reserved0, 0);
  assert.equal(srvs.bindings.getUint32(0, true), 55);
  assert.equal(srvs.bindings.getUint32(4, true), 0);
  assert.equal(srvs.bindings.getUint32(8, true), 0);
  assert.equal(srvs.bindings.getUint32(12, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // FLUSH
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Flush);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 16);
  assert.equal(cursor + 16, bytes.byteLength);
});

test("AerogpuCmdWriter emits sampler binding table packets", () => {
  const w = new AerogpuCmdWriter();
  w.createSampler(
    1,
    AerogpuSamplerFilter.Linear,
    AerogpuSamplerAddressMode.Repeat,
    AerogpuSamplerAddressMode.ClampToEdge,
    AerogpuSamplerAddressMode.MirrorRepeat,
  );
  w.setSamplers(AerogpuShaderStage.Pixel, 2, new Uint32Array([10, 11, 12]));
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 0, [
    { buffer: 100, offsetBytes: 0, sizeBytes: 64 },
    { buffer: 101, offsetBytes: 16, sizeBytes: 128 },
  ]);
  w.destroySampler(1);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // CREATE_SAMPLER
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateSampler);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_CREATE_SAMPLER_SIZE);
  assert.equal(view.getUint32(cursor + 8, true), 1);
  assert.equal(view.getUint32(cursor + 12, true), AerogpuSamplerFilter.Linear);
  assert.equal(view.getUint32(cursor + 16, true), AerogpuSamplerAddressMode.Repeat);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuSamplerAddressMode.ClampToEdge);
  assert.equal(view.getUint32(cursor + 24, true), AerogpuSamplerAddressMode.MirrorRepeat);
  cursor += AEROGPU_CMD_CREATE_SAMPLER_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetSamplers);
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_SAMPLERS_SIZE + 3 * 4,
  );
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 12, true), 2);
  assert.equal(view.getUint32(cursor + 16, true), 3);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_SAMPLERS_SIZE + 0, true), 10);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_SAMPLERS_SIZE + 4, true), 11);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_SAMPLERS_SIZE + 8, true), 12);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 3 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetConstantBuffers);
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 2 * 16,
  );
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Vertex);
  assert.equal(view.getUint32(cursor + 12, true), 0);
  assert.equal(view.getUint32(cursor + 16, true), 2);
  // bindings[0]
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 0, true), 100);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 4, true), 0);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 8, true), 64);
  // bindings[1]
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16, true), 101);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 20, true), 16);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 24, true), 128);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 2 * 16;

  // DESTROY_SAMPLER
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.DestroySampler);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_DESTROY_SAMPLER_SIZE);
  assert.equal(view.getUint32(cursor + 8, true), 1);
  cursor += AEROGPU_CMD_DESTROY_SAMPLER_SIZE;

  // FLUSH
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Flush);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 16);
  cursor += 16;

  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuCmdWriter emits SRV/UAV binding table packets and DISPATCH", () => {
  const w = new AerogpuCmdWriter();
  w.setShaderResourceBuffers(AerogpuShaderStage.Pixel, 1, [
    { buffer: 10, offsetBytes: 0, sizeBytes: 64 },
    { buffer: 11, offsetBytes: 16, sizeBytes: 0 },
  ]);
  w.setUnorderedAccessBuffers(AerogpuShaderStage.Compute, 0, [
    { buffer: 20, offsetBytes: 4, sizeBytes: 128, initialCount: 0 },
  ]);
  w.dispatch(1, 2, 3);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true),
    AerogpuCmdOpcode.SetShaderResourceBuffers,
  );
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 2 * 16,
  );
  const srvs = decodeCmdSetShaderResourceBuffersPayload(bytes, cursor);
  assert.equal(srvs.shaderStage, AerogpuShaderStage.Pixel);
  assert.equal(srvs.startSlot, 1);
  assert.equal(srvs.bufferCount, 2);
  assert.equal(srvs.reserved0, 0);
  assert.equal(srvs.bindings.getUint32(0, true), 10);
  assert.equal(srvs.bindings.getUint32(4, true), 0);
  assert.equal(srvs.bindings.getUint32(8, true), 64);
  assert.equal(srvs.bindings.getUint32(16, true), 11);
  assert.equal(srvs.bindings.getUint32(20, true), 16);
  assert.equal(srvs.bindings.getUint32(24, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 2 * 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true),
    AerogpuCmdOpcode.SetUnorderedAccessBuffers,
  );
  assert.equal(
    view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16,
  );
  const uavs = decodeCmdSetUnorderedAccessBuffersPayload(bytes, cursor);
  assert.equal(uavs.shaderStage, AerogpuShaderStage.Compute);
  assert.equal(uavs.startSlot, 0);
  assert.equal(uavs.uavCount, 1);
  assert.equal(uavs.reserved0, 0);
  assert.equal(uavs.bindings.getUint32(0, true), 20);
  assert.equal(uavs.bindings.getUint32(4, true), 4);
  assert.equal(uavs.bindings.getUint32(8, true), 128);
  assert.equal(uavs.bindings.getUint32(12, true), 0);
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // DISPATCH
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Dispatch);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 24);
  const dispatch = decodeCmdDispatchPayload(bytes, cursor);
  assert.equal(dispatch.groupCountX, 1);
  assert.equal(dispatch.groupCountY, 2);
  assert.equal(dispatch.groupCountZ, 3);
  assert.equal(dispatch.reserved0, 0);
  cursor += 24;

  // FLUSH
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Flush);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 16);
  cursor += 16;

  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuShaderStage includes Geometry=3 and AerogpuCmdWriter accepts it", () => {
  assert.equal(AerogpuShaderStage.Geometry, 3);

  const w = new AerogpuCmdWriter();
  w.createShaderDxbc(1, AerogpuShaderStage.Geometry, new Uint8Array([0xaa]));
  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateShaderDxbc);
  // stage field @ +12
  assert.equal(view.getUint32(pkt0 + 12, true), AerogpuShaderStage.Geometry);
});

test("AerogpuCmdWriter.bindShadersWithGs writes gs handle at the reserved offset", () => {
  const w = new AerogpuCmdWriter();
  w.bindShadersWithGs(/* vs */ 10, /* gs */ 11, /* ps */ 12, /* cs */ 13);
  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_BIND_SHADERS_SIZE);
  // Layout: vs @ +8, ps @ +12, cs @ +16, (reserved0=gs) @ +20
  assert.equal(view.getUint32(pkt0 + 8, true), 10);
  assert.equal(view.getUint32(pkt0 + 12, true), 12);
  assert.equal(view.getUint32(pkt0 + 16, true), 13);
  assert.equal(view.getUint32(pkt0 + 20, true), 11);
});

test("AerogpuCmdWriter emits CREATE_SHADER_DXBC stage_ex encoding + padding", () => {
  const w = new AerogpuCmdWriter();

  // 5 bytes -> requires 3 bytes of 4-byte padding.
  const dxbc = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
  w.createShaderDxbcEx(7, AerogpuShaderStageEx.Geometry, dxbc);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateShaderDxbc);
  const sizeBytes = view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  assert.equal(sizeBytes, alignUp(AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE + dxbc.byteLength, 4));

  // `aerogpu_cmd_create_shader_dxbc`:
  // - stage @ 12
  // - dxbc_size_bytes @ 16
  // - reserved0 @ 20 (used for stage_ex)
  assert.equal(view.getUint32(pkt0 + 12, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(pkt0 + 16, true), dxbc.byteLength);
  assert.equal(view.getUint32(pkt0 + 20, true), AerogpuShaderStageEx.Geometry);

  const payloadStart = pkt0 + AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE;
  assert.deepEqual(Array.from(bytes.slice(payloadStart, payloadStart + dxbc.byteLength)), Array.from(dxbc));

  const paddedStart = payloadStart + dxbc.byteLength;
  const paddedEnd = pkt0 + sizeBytes;
  assert.equal(paddedEnd - paddedStart, 3);
  assert.ok(bytes.slice(paddedStart, paddedEnd).every((b) => b === 0));
});

test("AerogpuCmdWriter emits BIND_SHADERS extended packet with trailing gs/hs/ds", () => {
  const w = new AerogpuCmdWriter();
  w.bindShadersEx(1, 2, 3, 4, 5, 6);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  assert.equal(view.getUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, true), bytes.byteLength);
  assert.equal(bytes.byteLength % 4, 0, "stream must remain 4-byte aligned");
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_BIND_SHADERS_EX_SIZE);

  // `aerogpu_cmd_bind_shaders`: vs/ps/cs + reserved0
  assert.equal(view.getUint32(pkt0 + 8, true), 1);
  assert.equal(view.getUint32(pkt0 + 12, true), 2);
  assert.equal(view.getUint32(pkt0 + 16, true), 3);
  // Append-only extension keeps reserved0=0.
  assert.equal(view.getUint32(pkt0 + 20, true), 0);

  // Trailing handles: gs/hs/ds.
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 0, true), 4);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 4, true), 5);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 8, true), 6);

  const decoded = decodeCmdBindShadersPayloadFromPacket({
    opcode: AerogpuCmdOpcode.BindShaders,
    sizeBytes: view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    payload: bytes.subarray(pkt0 + AEROGPU_CMD_HDR_SIZE, pkt0 + AEROGPU_CMD_BIND_SHADERS_EX_SIZE),
  });
  assert.equal(decoded.vs, 1);
  assert.equal(decoded.ps, 2);
  assert.equal(decoded.cs, 3);
  assert.equal(decoded.reserved0, 0);
  assert.deepEqual(decoded.ex, { gs: 4, hs: 5, ds: 6 });
});

test("AerogpuCmdWriter.bindShadersEx can mirror GS into reserved0 for legacy compatibility", () => {
  const w = new AerogpuCmdWriter();
  w.bindShadersEx(1, 2, 3, { gs: 4, hs: 5, ds: 6 }, /*mirrorGsToReserved0=*/ true);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_BIND_SHADERS_EX_SIZE);
  assert.equal(view.getUint32(pkt0 + 20, true), 4);

  const decoded = decodeCmdBindShadersPayloadFromPacket({
    opcode: AerogpuCmdOpcode.BindShaders,
    sizeBytes: view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    payload: bytes.subarray(pkt0 + AEROGPU_CMD_HDR_SIZE, pkt0 + AEROGPU_CMD_BIND_SHADERS_EX_SIZE),
  });
  assert.equal(decoded.reserved0, 4);
  assert.deepEqual(decoded.ex, { gs: 4, hs: 5, ds: 6 });
});

test("AerogpuCmdWriter.bindShadersHsDs emits an extended packet and leaves VS/PS/CS/GS unbound", () => {
  const w = new AerogpuCmdWriter();
  w.bindShadersHsDs(55, 66);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_BIND_SHADERS_EX_SIZE);

  const decoded = decodeCmdBindShadersPayloadFromPacket({
    opcode: AerogpuCmdOpcode.BindShaders,
    sizeBytes: view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true),
    payload: bytes.subarray(pkt0 + AEROGPU_CMD_HDR_SIZE, pkt0 + AEROGPU_CMD_BIND_SHADERS_EX_SIZE),
  });
  assert.equal(decoded.vs, 0);
  assert.equal(decoded.ps, 0);
  assert.equal(decoded.cs, 0);
  assert.equal(decoded.reserved0, 0);
  assert.deepEqual(decoded.ex, { gs: 0, hs: 55, ds: 66 });
});

test("AerogpuCmdWriter emits stage_ex binding packets (SET_CONSTANT_BUFFERS)", () => {
  const w = new AerogpuCmdWriter();
  w.setConstantBuffersEx(AerogpuShaderStageEx.Geometry, 0, [{ buffer: 99, offsetBytes: 16, sizeBytes: 64 }]);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetConstantBuffers);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16);

  // `aerogpu_cmd_set_constant_buffers`:
  // - shader_stage @ 8
  // - reserved0 @ 20 (used for stage_ex)
  assert.equal(view.getUint32(pkt0 + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(pkt0 + 20, true), AerogpuShaderStageEx.Geometry);
});

test("alignUp handles values > 2^31 without signed 32-bit wrap", () => {
  const alignUpFn = (AerogpuCmdWriter as unknown as { _alignUp: (v: number, a: number) => number })._alignUp;

  const v = 2 ** 31 + 1;
  const aligned = alignUpFn(v, 4);
  assert.ok(Number.isSafeInteger(aligned));
  assert.equal(aligned, 2 ** 31 + 4);
});

test("stage_ex encode/decode helpers roundtrip", () => {
  const all = [
    AerogpuShaderStageEx.None,
    AerogpuShaderStageEx.Geometry,
    AerogpuShaderStageEx.Hull,
    AerogpuShaderStageEx.Domain,
  ];

  for (const stageEx of all) {
    const [shaderStage, reserved0] = encodeStageEx(stageEx);
    assert.equal(shaderStage, AerogpuShaderStage.Compute);
    assert.equal(reserved0, stageEx);
    assert.equal(decodeStageEx(shaderStage, reserved0), stageEx);
  }

  // Compute stage_ex is accepted as an alias but writers should canonicalize it to reserved0=0.
  {
    const [shaderStage, reserved0] = encodeStageEx(AerogpuShaderStageEx.Compute);
    assert.equal(shaderStage, AerogpuShaderStage.Compute);
    assert.equal(reserved0, 0);
    assert.equal(decodeStageEx(shaderStage, reserved0), AerogpuShaderStageEx.None);
    assert.equal(
      decodeStageEx(AerogpuShaderStage.Compute, AerogpuShaderStageEx.Compute),
      AerogpuShaderStageEx.Compute,
    );
  }
  assert.equal(decodeStageEx(AerogpuShaderStage.Vertex, AerogpuShaderStageEx.Geometry), undefined);
});

test("stage_ex shader-stage resolution helpers handle legacy Pixel/Vertex and invalid discriminators", () => {
  // `decodeShaderStageEx` is a strict conversion from legacy `(shaderStage, reserved0)` pairs into a
  // single `AerogpuShaderStageEx` enum. Pixel/Vertex cannot be represented in this enum.
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Vertex, 0), null);
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Pixel, 0), null);
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Geometry, 0), AerogpuShaderStageEx.Geometry);
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Geometry, 123), null);
  // For compute packets, reserved0==0 is legacy encoding (treated as Compute).
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Compute, 0), AerogpuShaderStageEx.Compute);
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Compute, AerogpuShaderStageEx.Domain), AerogpuShaderStageEx.Domain);
  // Stage_ex values that are not part of the protocol enum are rejected.
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Compute, 1), null);
  assert.equal(decodeShaderStageEx(AerogpuShaderStage.Compute, 42), null);

  // `resolveShaderStageWithEx` is the stage_ex-aware decode that can represent legacy Pixel/Vertex.
  assert.deepEqual(resolveShaderStageWithEx(AerogpuShaderStage.Pixel, 999), { kind: "Pixel" });
  assert.deepEqual(resolveShaderStageWithEx(AerogpuShaderStage.Vertex, 2), { kind: "Vertex" });
  assert.deepEqual(resolveShaderStageWithEx(AerogpuShaderStage.Compute, 0), { kind: "Compute" });
  assert.deepEqual(resolveShaderStageWithEx(AerogpuShaderStage.Compute, AerogpuShaderStageEx.Hull), { kind: "Hull" });
  // Unknown stage_ex discriminators are preserved for forward-compat.
  assert.deepEqual(resolveShaderStageWithEx(AerogpuShaderStage.Compute, 1), {
    kind: "Unknown",
    shaderStage: AerogpuShaderStage.Compute,
    stageEx: 1,
  });
});

test("AerogpuCmdWriter.createShaderDxbcEx rejects invalid stageEx=1 (Vertex program type) and does not emit a packet", () => {
  const w = new AerogpuCmdWriter();
  assert.throws(
    () => w.createShaderDxbcEx(1, 1 as unknown as AerogpuShaderStageEx, new Uint8Array([0xaa])),
    /invalid stage_ex value 1/,
  );

  const bytes = w.finish();
  assert.equal(bytes.byteLength, AEROGPU_CMD_STREAM_HEADER_SIZE);
});

test("AerogpuCmdWriter.createShaderDxbcEx rejects stageEx=0 (DXBC Pixel program type)", () => {
  const w = new AerogpuCmdWriter();
  const dxbc = new Uint8Array([0xaa]);
  // stageEx=0 is reserved for legacy compute (reserved0==0), so Pixel cannot be encoded via stage_ex.
  assert.throws(() => w.createShaderDxbcEx(1, 0 as unknown as AerogpuShaderStageEx, dxbc));
});

test("AerogpuCmdWriter stage_ex optional parameters reject stageEx=0 (DXBC Pixel program type)", () => {
  const zero = 0 as unknown as AerogpuShaderStageEx;
  assert.throws(() => new AerogpuCmdWriter().setTexture(AerogpuShaderStage.Vertex, 0, 99, zero));
  assert.throws(() => new AerogpuCmdWriter().setSamplers(AerogpuShaderStage.Vertex, 0, new Uint32Array([1]), zero));
  assert.throws(() =>
    new AerogpuCmdWriter().setConstantBuffers(AerogpuShaderStage.Vertex, 0, [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }], zero),
  );
  assert.throws(() =>
    new AerogpuCmdWriter().setShaderResourceBuffers(
      AerogpuShaderStage.Vertex,
      0,
      [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }],
      zero,
    ),
  );
  assert.throws(() =>
    new AerogpuCmdWriter().setUnorderedAccessBuffers(
      AerogpuShaderStage.Compute,
      1,
      [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }],
      zero,
    ),
  );
  assert.throws(() =>
    new AerogpuCmdWriter().setShaderConstantsF(AerogpuShaderStage.Vertex, 0, new Float32Array([1, 2, 3, 4]), zero),
  );
  assert.throws(() =>
    new AerogpuCmdWriter().setShaderConstantsI(AerogpuShaderStage.Vertex, 0, new Int32Array([1, 2, 3, 4]), zero),
  );
  assert.throws(() => new AerogpuCmdWriter().setShaderConstantsB(AerogpuShaderStage.Vertex, 0, [1], zero));
});

test("AerogpuCmdWriter stage_ex Ex helpers reject stageEx=0 and do not emit packets", () => {
  const w = new AerogpuCmdWriter();
  const zero = AerogpuShaderStageEx.None;

  assert.throws(() => w.setTextureEx(zero, 0, 99));
  assert.throws(() => w.setSamplersEx(zero, 0, new Uint32Array([1])));
  assert.throws(() => w.setConstantBuffersEx(zero, 0, [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }]));
  assert.throws(() => w.setShaderResourceBuffersEx(zero, 0, [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }]));
  assert.throws(() =>
    w.setUnorderedAccessBuffersEx(zero, 0, [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }]),
  );
  assert.throws(() => w.setShaderConstantsFEx(zero, 0, new Float32Array([1, 2, 3, 4])));
  assert.throws(() => w.setShaderConstantsIEx(zero, 0, new Int32Array([1, 2, 3, 4])));
  assert.throws(() => w.setShaderConstantsBEx(zero, 0, [1]));

  const bytes = w.finish();
  assert.equal(bytes.byteLength, AEROGPU_CMD_STREAM_HEADER_SIZE);
});

test("AerogpuCmdWriter stage_ex Ex helpers reject invalid stageEx=1 and do not emit packets", () => {
  const w = new AerogpuCmdWriter();
  // DXBC program type 1 is Vertex, but stage_ex intentionally forbids encoding Vertex via reserved0.
  const bad = 1 as unknown as AerogpuShaderStageEx;

  assert.throws(() => w.setTextureEx(bad, 0, 99), /invalid stage_ex value 1/);
  assert.throws(() => w.setSamplersEx(bad, 0, new Uint32Array([1])), /invalid stage_ex value 1/);
  assert.throws(
    () => w.setConstantBuffersEx(bad, 0, [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }]),
    /invalid stage_ex value 1/,
  );
  assert.throws(
    () => w.setShaderResourceBuffersEx(bad, 0, [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }]),
    /invalid stage_ex value 1/,
  );
  assert.throws(
    () => w.setUnorderedAccessBuffersEx(bad, 0, [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }]),
    /invalid stage_ex value 1/,
  );
  assert.throws(() => w.setShaderConstantsFEx(bad, 0, new Float32Array([1, 2, 3, 4])), /invalid stage_ex value 1/);
  assert.throws(() => w.setShaderConstantsIEx(bad, 0, new Int32Array([1, 2, 3, 4])), /invalid stage_ex value 1/);
  assert.throws(() => w.setShaderConstantsBEx(bad, 0, [1]), /invalid stage_ex value 1/);

  const bytes = w.finish();
  assert.equal(bytes.byteLength, AEROGPU_CMD_STREAM_HEADER_SIZE);
});

test("AerogpuCmdWriter.createShaderDxbc encodes stage=Pixel and keeps reserved0=0", () => {
  const w = new AerogpuCmdWriter();
  const dxbc = new Uint8Array([0xaa]);
  w.createShaderDxbc(1, AerogpuShaderStage.Pixel, dxbc);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateShaderDxbc);
  const sizeBytes = view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  assert.equal(sizeBytes, alignUp(24 + dxbc.byteLength, 4));

  assert.equal(view.getUint32(pkt0 + 8, true), 1); // shader_handle
  assert.equal(view.getUint32(pkt0 + 12, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(pkt0 + 16, true), dxbc.byteLength);
  assert.equal(view.getUint32(pkt0 + 20, true), 0); // reserved0
  assert.deepEqual(bytes.subarray(pkt0 + 24, pkt0 + 24 + dxbc.byteLength), dxbc);
});

test("AerogpuCmdWriter.createShaderDxbc rejects stageEx for non-compute stages without mutating the stream", () => {
  const w = new AerogpuCmdWriter();
  const dxbc = new Uint8Array([0xaa]);
  assert.throws(() => w.createShaderDxbc(1, AerogpuShaderStage.Pixel, dxbc, AerogpuShaderStageEx.Geometry));
  const bytes = w.finish();
  assert.equal(bytes.byteLength, AEROGPU_CMD_STREAM_HEADER_SIZE);
});

test("AerogpuCmdWriter stage_ex optional parameters override stage and encode (shaderStage=COMPUTE, reserved0=stageEx)", () => {
  const w = new AerogpuCmdWriter();
  const stageEx = AerogpuShaderStageEx.Geometry;

  w.setTexture(AerogpuShaderStage.Pixel, 0, 99, stageEx);
  w.setSamplers(AerogpuShaderStage.Pixel, 0, new Uint32Array([1]), stageEx);
  w.setConstantBuffers(
    AerogpuShaderStage.Pixel,
    0,
    [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }],
    stageEx,
  );
  w.setShaderResourceBuffers(
    AerogpuShaderStage.Pixel,
    0,
    [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }],
    stageEx,
  );
  w.setUnorderedAccessBuffers(
    AerogpuShaderStage.Pixel,
    1,
    [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }],
    stageEx,
  );
  w.setShaderConstantsF(AerogpuShaderStage.Pixel, 0, new Float32Array([1, 2, 3, 4]), stageEx);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 1 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // SET_SHADER_CONSTANTS_F
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), stageEx);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

  assert.equal(cursor, bytes.byteLength);
});
test("AerogpuCmdWriter legacy binding packets keep reserved0=0", () => {
  const w = new AerogpuCmdWriter();
  w.setTexture(AerogpuShaderStage.Pixel, 0, 99);
  w.setSamplers(AerogpuShaderStage.Vertex, 0, new Uint32Array([1, 2]));
  w.setConstantBuffers(AerogpuShaderStage.Pixel, 0, [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }]);
  w.setShaderResourceBuffers(AerogpuShaderStage.Pixel, 0, [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }]);
  w.setUnorderedAccessBuffers(AerogpuShaderStage.Compute, 1, [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }]);
  w.setShaderConstantsF(AerogpuShaderStage.Vertex, 0, new Float32Array([1, 2, 3, 4]));

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 2 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // SET_SHADER_CONSTANTS_F
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuCmdWriter stage_ex binding packets encode GS/HS/DS via (COMPUTE, reserved0=2/3/4)", () => {
  const w = new AerogpuCmdWriter();
  w.setTextureEx(AerogpuShaderStageEx.Geometry, 3, 44);
  w.setSamplersEx(AerogpuShaderStageEx.Hull, 0, new Uint32Array([1, 2, 3]));
  w.setConstantBuffersEx(AerogpuShaderStageEx.Domain, 1, [{ buffer: 7, offsetBytes: 0, sizeBytes: 16 }]);
  w.setShaderResourceBuffersEx(AerogpuShaderStageEx.Hull, 0, [{ buffer: 8, offsetBytes: 0, sizeBytes: 32 }]);
  w.setUnorderedAccessBuffersEx(AerogpuShaderStageEx.Domain, 1, [{ buffer: 9, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }]);
  w.setShaderConstantsFEx(AerogpuShaderStageEx.Compute, 0, new Float32Array([1, 2, 3, 4]));

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Geometry);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.Geometry,
  );
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.Hull,
  );
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 3 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Domain);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.Domain,
  );
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  assert.equal(decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)), AerogpuShaderStageEx.Hull);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Domain);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.Domain,
  );
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // SET_SHADER_CONSTANTS_F
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  // Compute stage is canonicalized to stage_ex=None (reserved0=0).
  assert.equal(view.getUint32(cursor + 20, true), 0);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.None,
  );
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuCmdWriter stage_ex SRV/UAV buffer binding packets decode via packet decoders", () => {
  const w = new AerogpuCmdWriter();
  w.setShaderResourceBuffersEx(AerogpuShaderStageEx.Hull, 1, [
    { buffer: 10, offsetBytes: 0, sizeBytes: 64 },
    { buffer: 11, offsetBytes: 16, sizeBytes: 128 },
  ]);
  w.setUnorderedAccessBuffersEx(AerogpuShaderStageEx.Domain, 2, [
    { buffer: 20, offsetBytes: 0, sizeBytes: 256, initialCount: 0xffff_ffff },
  ]);

  const { packets } = decodeCmdStreamView(w.finish());
  assert.equal(packets.length, 2);

  const srv = decodeCmdSetShaderResourceBuffersPayloadFromPacket(packets[0]!);
  assert.equal(srv.shaderStage, AerogpuShaderStage.Compute);
  assert.equal(decodeStageEx(srv.shaderStage, srv.reserved0), AerogpuShaderStageEx.Hull);

  const uav = decodeCmdSetUnorderedAccessBuffersPayloadFromPacket(packets[1]!);
  assert.equal(uav.shaderStage, AerogpuShaderStage.Compute);
  assert.equal(decodeStageEx(uav.shaderStage, uav.reserved0), AerogpuShaderStageEx.Domain);
});

test("AerogpuCmdWriter optional stageEx parameters encode (shaderStage=COMPUTE, reserved0=stageEx) for GS/HS/DS", () => {
  const w = new AerogpuCmdWriter();
  w.setTexture(AerogpuShaderStage.Pixel, 0, 99, AerogpuShaderStageEx.Geometry);
  w.setSamplers(AerogpuShaderStage.Pixel, 0, new Uint32Array([1, 2]), AerogpuShaderStageEx.Hull);
  w.setConstantBuffers(AerogpuShaderStage.Vertex, 1, [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }], AerogpuShaderStageEx.Domain);
  w.setShaderResourceBuffers(
    AerogpuShaderStage.Pixel,
    0,
    [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }],
    AerogpuShaderStageEx.Hull,
  );
  w.setUnorderedAccessBuffers(
    AerogpuShaderStage.Compute,
    1,
    [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }],
    AerogpuShaderStageEx.Domain,
  );
  w.setShaderConstantsF(AerogpuShaderStage.Vertex, 0, new Float32Array([1, 2, 3, 4]), AerogpuShaderStageEx.Compute);
  w.setShaderConstantsI(AerogpuShaderStage.Pixel, 0, new Int32Array([1, 2, 3, 4]), AerogpuShaderStageEx.Hull);
  w.setShaderConstantsB(AerogpuShaderStage.Vertex, 1, [true, false], AerogpuShaderStageEx.Domain);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Geometry);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 2 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Domain);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Domain);
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // SET_SHADER_CONSTANTS_F
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  // Compute stage_ex is canonicalized to stage_ex=None (reserved0=0).
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

  // SET_SHADER_CONSTANTS_I
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_I_SIZE + 16;

  // SET_SHADER_CONSTANTS_B
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Domain);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + 2 * 4;

  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuCmdWriter emits stage_ex packets and extended BindShaders encoding", () => {
  const w = new AerogpuCmdWriter();
  w.bindShadersEx(1, 2, 3, 4, 5, 6);
  w.createShaderDxbcEx(7, AerogpuShaderStageEx.Geometry, new Uint8Array([0xaa, 0xbb, 0xcc]));
  w.setTextureEx(AerogpuShaderStageEx.Hull, 9, 10);

  const bytes = w.finish();
  assert.equal(bytes.byteLength % 4, 0, "stream must remain 4-byte aligned");

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  assert.equal(view.getUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, true), bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // BIND_SHADERS_EX
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), 24 + 12);
  assert.equal(view.getUint32(cursor + 8, true), 1);
  assert.equal(view.getUint32(cursor + 12, true), 2);
  assert.equal(view.getUint32(cursor + 16, true), 3);
  // Append-only extension keeps reserved0=0.
  assert.equal(view.getUint32(cursor + 20, true), 0);
  // Trailing GS/HS/DS u32s.
  assert.equal(view.getUint32(cursor + 24, true), 4);
  assert.equal(view.getUint32(cursor + 28, true), 5);
  assert.equal(view.getUint32(cursor + 32, true), 6);
  cursor += 24 + 12;

  // CREATE_SHADER_DXBC_EX (stage_ex stored in reserved0; stage set to Compute for fwd-compat).
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.CreateShaderDxbc);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), alignUp(24 + 3, 4));
  assert.equal(view.getUint32(cursor + 12, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Geometry);
  cursor += alignUp(24 + 3, 4);

  // SET_TEXTURE_EX (shader_stage_ex stored in reserved0; shader_stage set to Compute for fwd-compat).
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetTexture);
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_SET_TEXTURE_SIZE);
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  assert.equal(cursor, bytes.byteLength);
});

test("decodeStageEx treats reserved0==0 as legacy compute (and decodes nonzero stageEx)", () => {
  const w = new AerogpuCmdWriter();

  // Legacy compute packets: shader_stage == COMPUTE and reserved0 == 0.
  w.setTexture(AerogpuShaderStage.Compute, 0, 99);
  w.setConstantBuffers(AerogpuShaderStage.Compute, 0, [{ buffer: 1, offsetBytes: 0, sizeBytes: 16 }]);

  // Extended stage example: shader_stage == COMPUTE and reserved0 != 0.
  w.setTexture(AerogpuShaderStage.Compute, 1, 100, AerogpuShaderStageEx.Geometry);
  w.flush();

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE (legacy compute)
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetTexture);
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.None,
  );
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_CONSTANT_BUFFERS (legacy compute)
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetConstantBuffers);
  const cbSizeBytes = view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.None,
  );
  cursor += cbSizeBytes;

  // SET_TEXTURE (stage_ex = GEOMETRY)
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.SetTexture);
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Geometry);
  assert.equal(
    decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)),
    AerogpuShaderStageEx.Geometry,
  );
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // FLUSH
  assert.equal(view.getUint32(cursor + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.Flush);
  cursor += 16;
  assert.equal(cursor, bytes.byteLength);
});

test("AerogpuCmdWriter.createShaderDxbcEx encodes (stage=COMPUTE, reserved0=stageEx) for HS/DS", () => {
  // These numeric values must match DXBC/D3D `D3D10_SB_PROGRAM_TYPE`.
  assert.equal(AerogpuShaderStageEx.Hull, 3);
  assert.equal(AerogpuShaderStageEx.Domain, 4);

  const w = new AerogpuCmdWriter();
  w.createShaderDxbcEx(1, AerogpuShaderStageEx.Hull, new Uint8Array([0xaa]));
  w.createShaderDxbcEx(2, AerogpuShaderStageEx.Domain, new Uint8Array([0xbb, 0xcc]));

  const packets = Array.from(iterCmdStream(w.finish()));
  assert.equal(packets.length, 2);

  const hs = decodeCmdCreateShaderDxbcPayloadFromPacket(packets[0]!);
  assert.equal(hs.shaderHandle, 1);
  assert.equal(hs.stage, AerogpuShaderStage.Compute);
  assert.equal(hs.reserved0, AerogpuShaderStageEx.Hull);
  assert.deepEqual(hs.dxbcBytes, new Uint8Array([0xaa]));

  const ds = decodeCmdCreateShaderDxbcPayloadFromPacket(packets[1]!);
  assert.equal(ds.shaderHandle, 2);
  assert.equal(ds.stage, AerogpuShaderStage.Compute);
  assert.equal(ds.reserved0, AerogpuShaderStageEx.Domain);
  assert.deepEqual(ds.dxbcBytes, new Uint8Array([0xbb, 0xcc]));
});
