import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_HDR_OFF_OPCODE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_HDR_SIZE,
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
  AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE,
  AEROGPU_CMD_SET_TEXTURE_SIZE,
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
  alignUp,
  decodeCmdStreamHeader,
} from "../aerogpu/aerogpu_cmd.ts";

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

test("alignUp handles values > 2^31 without signed 32-bit wrap", () => {
  const alignUpFn = (AerogpuCmdWriter as any)._alignUp as (v: number, a: number) => number;

  const v = 2 ** 31 + 1;
  const aligned = alignUpFn(v, 4);
  assert.ok(Number.isSafeInteger(aligned));
  assert.equal(aligned, 2 ** 31 + 4);
});
