import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
  AEROGPU_CMD_BIND_SHADERS_SIZE,
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
  decodeCmdBindShadersPayload,
  decodeCmdDispatchPayload,
  decodeCmdSetShaderResourceBuffersPayload,
  decodeCmdSetUnorderedAccessBuffersPayload,
  decodeStageEx,
  decodeCmdStreamHeader,
  encodeStageEx,
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

  // Validate variable-length constants packet.
  const pkt0Base = AEROGPU_CMD_STREAM_HEADER_SIZE;
  assert.equal(view.getUint32(pkt0Base + 16, true), 1);
  assert.equal(view.getFloat32(pkt0Base + AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE, true), 1);

  // Validate byte-sized fields within nested state structs.
  const blendBase = pkt0Base + expected[0][1] + expected[1][1] + expected[2][1] + expected[3][1];
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

  const depthBase = blendBase + expected[4][1];
  assert.equal(view.getUint8(depthBase + 24), 0xaa);
  assert.equal(view.getUint8(depthBase + 25), 0xbb);

  const rastBase = depthBase + expected[5][1];
  assert.equal(view.getInt32(rastBase + 24, true), -1);
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
  const pkt0 = AEROGPU_CMD_STREAM_HEADER_SIZE;

  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_OPCODE, true), AerogpuCmdOpcode.BindShaders);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true), AEROGPU_CMD_BIND_SHADERS_SIZE + 12);

  // `aerogpu_cmd_bind_shaders`: vs/ps/cs + reserved0
  assert.equal(view.getUint32(pkt0 + 8, true), 1);
  assert.equal(view.getUint32(pkt0 + 12, true), 2);
  assert.equal(view.getUint32(pkt0 + 16, true), 3);
  // Legacy compatibility: keep GS in `reserved0` so older decoders can still bind it.
  assert.equal(view.getUint32(pkt0 + 20, true), 4);

  // Trailing handles: gs/hs/ds.
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 0, true), 4);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 4, true), 5);
  assert.equal(view.getUint32(pkt0 + AEROGPU_CMD_BIND_SHADERS_SIZE + 8, true), 6);

  const decoded = decodeCmdBindShadersPayload(bytes, pkt0);
  assert.equal(decoded.vs, 1);
  assert.equal(decoded.ps, 2);
  assert.equal(decoded.cs, 3);
  assert.deepEqual(decoded.ex, { gs: 4, hs: 5, ds: 6 });
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
  const alignUpFn = (AerogpuCmdWriter as any)._alignUp as (v: number, a: number) => number;

  const v = 2 ** 31 + 1;
  const aligned = alignUpFn(v, 4);
  assert.ok(Number.isSafeInteger(aligned));
  assert.equal(aligned, 2 ** 31 + 4);
});

test("stage_ex encode/decode helpers follow the reserved0==0 legacy rule", () => {
  // Legacy stages (VS/PS/CS): stage_ex is not used and reserved0 stays 0.
  assert.deepEqual(encodeStageEx(AerogpuShaderStageEx.Pixel), [AerogpuShaderStage.Pixel, 0]);
  assert.deepEqual(encodeStageEx(AerogpuShaderStageEx.Vertex), [AerogpuShaderStage.Vertex, 0]);
  assert.deepEqual(encodeStageEx(AerogpuShaderStageEx.Compute), [AerogpuShaderStage.Compute, 0]);

  // Extended stages (GS/HS/DS): encoded as (shaderStage=COMPUTE, reserved0=2/3/4).
  for (const stageEx of [AerogpuShaderStageEx.Geometry, AerogpuShaderStageEx.Hull, AerogpuShaderStageEx.Domain]) {
    const [shaderStage, reserved0] = encodeStageEx(stageEx);
    assert.equal(shaderStage, AerogpuShaderStage.Compute);
    assert.equal(decodeStageEx(shaderStage, reserved0), stageEx);
  }

  // Regression: legacy compute binding (COMPUTE, reserved0=0) must not decode as Pixel.
  assert.equal(decodeStageEx(AerogpuShaderStage.Compute, 0), undefined);

  // Non-compute legacy stage never uses stage_ex.
  assert.equal(decodeStageEx(AerogpuShaderStage.Vertex, AerogpuShaderStageEx.Geometry), undefined);
});

test("AerogpuCmdWriter.createShaderDxbcEx encodes stageEx=Pixel using the legacy stage field", () => {
  const w = new AerogpuCmdWriter();
  const dxbc = new Uint8Array([0xaa]);
  w.createShaderDxbcEx(1, AerogpuShaderStageEx.Pixel, dxbc);

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

test("AerogpuCmdWriter stage_ex optional parameters encode Pixel via legacy shaderStage field", () => {
  const w = new AerogpuCmdWriter();

  // stageEx=Pixel should not produce the ambiguous encoding (shaderStage=COMPUTE, reserved0=0);
  // it must use the legacy Pixel shaderStage field with reserved0=0.
  w.setTexture(AerogpuShaderStage.Vertex, 0, 99, AerogpuShaderStageEx.Pixel);
  w.setSamplers(AerogpuShaderStage.Vertex, 0, new Uint32Array([1]), AerogpuShaderStageEx.Pixel);
  w.setConstantBuffers(
    AerogpuShaderStage.Vertex,
    0,
    [{ buffer: 3, offsetBytes: 0, sizeBytes: 16 }],
    AerogpuShaderStageEx.Pixel,
  );
  w.setShaderResourceBuffers(
    AerogpuShaderStage.Vertex,
    0,
    [{ buffer: 4, offsetBytes: 0, sizeBytes: 32 }],
    AerogpuShaderStageEx.Pixel,
  );
  w.setUnorderedAccessBuffers(
    AerogpuShaderStage.Compute,
    1,
    [{ buffer: 5, offsetBytes: 4, sizeBytes: 16, initialCount: 0 }],
    AerogpuShaderStageEx.Pixel,
  );
  w.setShaderConstantsF(AerogpuShaderStage.Vertex, 0, new Float32Array([1, 2, 3, 4]), AerogpuShaderStageEx.Pixel);

  const bytes = w.finish();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  let cursor = AEROGPU_CMD_STREAM_HEADER_SIZE;

  // SET_TEXTURE
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SAMPLERS_SIZE + 1 * 4;

  // SET_CONSTANT_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE + 16;

  // SET_SHADER_RESOURCE_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + 16;

  // SET_UNORDERED_ACCESS_BUFFERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + 16;

  // SET_SHADER_CONSTANTS_F
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Pixel);
  assert.equal(view.getUint32(cursor + 20, true), 0);
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
  assert.equal(decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)), AerogpuShaderStageEx.Geometry);
  cursor += AEROGPU_CMD_SET_TEXTURE_SIZE;

  // SET_SAMPLERS
  assert.equal(view.getUint32(cursor + 8, true), AerogpuShaderStage.Compute);
  assert.equal(view.getUint32(cursor + 20, true), AerogpuShaderStageEx.Hull);
  assert.equal(decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)), AerogpuShaderStageEx.Hull);
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
  // Compute bindings remain the legacy encoding: reserved0 stays 0 (not 5).
  assert.equal(view.getUint32(cursor + 20, true), 0);
  assert.equal(decodeStageEx(view.getUint32(cursor + 8, true), view.getUint32(cursor + 20, true)), undefined);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

  assert.equal(cursor, bytes.byteLength);
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
  // Compute bindings remain the legacy encoding: reserved0 stays 0 (not 5).
  assert.equal(view.getUint32(cursor + 20, true), 0);
  cursor += AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE + 16;

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
  // Legacy compatibility: keep GS in `reserved0` so older decoders can still bind it.
  assert.equal(view.getUint32(cursor + 20, true), 4);
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
