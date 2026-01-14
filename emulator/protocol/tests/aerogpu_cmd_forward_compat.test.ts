import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_BIND_SHADERS_SIZE,
  AEROGPU_CMD_BIND_SHADERS_EX_SIZE,
  AEROGPU_CMD_COPY_BUFFER_SIZE,
  AEROGPU_CMD_COPY_TEXTURE2D_SIZE,
  AEROGPU_CMD_HDR_OFF_SIZE_BYTES,
  AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE,
  AEROGPU_CMD_DISPATCH_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE,
  AEROGPU_CMD_SET_BLEND_STATE_SIZE_MIN,
  AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE,
  AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE,
  AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE,
  AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE,
  AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES,
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_CMD_STREAM_MAGIC,
  AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE,
  AerogpuBlendFactor,
  AerogpuBlendOp,
  AerogpuCmdOpcode,
  decodeCmdBindShadersPayload,
  decodeCmdBindShadersPayloadFromPacket,
  decodeCmdCopyBufferPayload,
  decodeCmdCopyTexture2dPayload,
  decodeCmdCreateShaderDxbcPayload,
  decodeCmdDispatchPayload,
  decodeCmdStreamView,
  decodeCmdSetBlendState,
  decodeCmdSetShaderConstantsBPayload,
  decodeCmdSetShaderResourceBuffersPayload,
  decodeCmdSetUnorderedAccessBuffersPayload,
} from "../aerogpu/aerogpu_cmd.ts";
import { AEROGPU_ABI_VERSION_U32 } from "../aerogpu/aerogpu_pci.ts";

function pushU32(out: number[], v: number): void {
  out.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
}

function pushU64(out: number[], v: bigint): void {
  const lo = Number(v & 0xffff_ffffn);
  const hi = Number((v >> 32n) & 0xffff_ffffn);
  pushU32(out, lo);
  pushU32(out, hi);
}

function buildStream(withTrailing: boolean): Uint8Array {
  const bytes: number[] = [];

  // Stream header.
  pushU32(bytes, AEROGPU_CMD_STREAM_MAGIC);
  pushU32(bytes, AEROGPU_ABI_VERSION_U32);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 0); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0); // reserved1

  // Unknown opcode packet (skipped via size_bytes).
  pushU32(bytes, 0xdead_beef);
  pushU32(bytes, 12);
  pushU32(bytes, 0x1234_5678);

  // SET_BLEND_STATE packet.
  const cmdOffset = bytes.length;
  pushU32(bytes, AerogpuCmdOpcode.SetBlendState);
  pushU32(bytes, 0); // size_bytes (patched later)

  pushU32(bytes, 1); // enable
  pushU32(bytes, AerogpuBlendFactor.One);
  pushU32(bytes, AerogpuBlendFactor.Zero);
  pushU32(bytes, AerogpuBlendOp.Add);

  bytes.push(0xf); // color_write_mask
  bytes.push(0, 0, 0); // reserved0[3]

  // Extended blend state fields added in newer ABI minor versions.
  pushU32(bytes, AerogpuBlendFactor.One); // src_factor_alpha
  pushU32(bytes, AerogpuBlendFactor.Zero); // dst_factor_alpha
  pushU32(bytes, AerogpuBlendOp.Add); // blend_op_alpha
  // blend_constant_rgba_f32
  pushU32(bytes, 0);
  pushU32(bytes, 0);
  pushU32(bytes, 0);
  pushU32(bytes, 0);
  pushU32(bytes, 0xffff_ffff); // sample_mask

  if (withTrailing) {
    // Forward-compatible extension (ignored by old decoders).
    pushU32(bytes, 0xdead_beef);
  }

  const out = new Uint8Array(bytes);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);

  dv.setUint32(cmdOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, out.byteLength - cmdOffset, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, out.byteLength, true);
  assert.equal(out.byteLength % 4, 0, "stream not 4-byte aligned");
  assert.equal(cmdOffset, AEROGPU_CMD_STREAM_HEADER_SIZE + 12);

  return out;
}

test("AeroGPU command stream decoders accept trailing bytes in fixed-size packets", () => {
  const base = buildStream(false);
  const extended = buildStream(true);

  const packetsBase = decodeCmdStreamView(base).packets;
  const packetsExt = decodeCmdStreamView(extended).packets;

  assert.equal(packetsBase.length, 2);
  assert.equal(packetsExt.length, 2);

  assert.equal(packetsBase[0]!.hdr.opcode >>> 0, 0xdead_beef);
  assert.equal(packetsExt[0]!.hdr.opcode >>> 0, 0xdead_beef);

  assert.equal(packetsBase[1]!.hdr.opcode, AerogpuCmdOpcode.SetBlendState);
  assert.equal(packetsExt[1]!.hdr.opcode, AerogpuCmdOpcode.SetBlendState);
  assert.equal(packetsBase[1]!.hdr.sizeBytes, AEROGPU_CMD_SET_BLEND_STATE_SIZE);
  assert.equal(packetsExt[1]!.hdr.sizeBytes, AEROGPU_CMD_SET_BLEND_STATE_SIZE + 4);

  const viewBase = new DataView(base.buffer, base.byteOffset, base.byteLength);
  const viewExt = new DataView(extended.buffer, extended.byteOffset, extended.byteLength);

  const decodedBase = decodeCmdSetBlendState(viewBase, packetsBase[1]!.offsetBytes);
  const decodedExt = decodeCmdSetBlendState(viewExt, packetsExt[1]!.offsetBytes);

  assert.deepEqual(decodedExt, decodedBase);
});

test("variable-payload decoders accept trailing bytes in cmd.size_bytes", () => {
  const bytes: number[] = [];

  // Stream header.
  pushU32(bytes, AEROGPU_CMD_STREAM_MAGIC);
  pushU32(bytes, AEROGPU_ABI_VERSION_U32);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 0); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0); // reserved1

  const cmdOffset = bytes.length;
  pushU32(bytes, AerogpuCmdOpcode.CreateShaderDxbc);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 7); // shader_handle
  pushU32(bytes, 0); // stage (vertex)
  pushU32(bytes, 8); // dxbc_size_bytes
  pushU32(bytes, 0); // reserved0
  // 8 bytes of DXBC payload (already 4-byte aligned, so no padding required).
  bytes.push(1, 2, 3, 4, 5, 6, 7, 8);
  // Forward-compatible extension (ignored by old decoders).
  pushU32(bytes, 0xdead_beef);

  const out = new Uint8Array(bytes);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  dv.setUint32(cmdOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, out.byteLength - cmdOffset, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, out.byteLength, true);

  assert.equal(out.byteLength % 4, 0);
  assert.equal(out.byteLength - cmdOffset, AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE + 8 + 4);

  const decoded = decodeCmdCreateShaderDxbcPayload(out, cmdOffset);
  assert.equal(decoded.shaderHandle, 7);
  assert.equal(decoded.stage, 0);
  assert.equal(decoded.dxbcSizeBytes, 8);
  assert.deepEqual(Array.from(decoded.dxbcBytes), [1, 2, 3, 4, 5, 6, 7, 8]);
});

test("DISPATCH decoder accepts trailing bytes in cmd.size_bytes", () => {
  const bytes: number[] = [];
  pushU32(bytes, AerogpuCmdOpcode.Dispatch);
  pushU32(bytes, AEROGPU_CMD_DISPATCH_SIZE + 4);
  pushU32(bytes, 1);
  pushU32(bytes, 2);
  pushU32(bytes, 3);
  pushU32(bytes, 0);
  pushU32(bytes, 0xdead_beef);

  const out = new Uint8Array(bytes);
  const decoded = decodeCmdDispatchPayload(out, 0);
  assert.equal(decoded.groupCountX, 1);
  assert.equal(decoded.groupCountY, 2);
  assert.equal(decoded.groupCountZ, 3);
  assert.equal(decoded.reserved0, 0);
});

test("COPY_BUFFER decoder accepts trailing bytes in cmd.size_bytes", () => {
  const bytes: number[] = [];
  pushU32(bytes, AerogpuCmdOpcode.CopyBuffer);
  pushU32(bytes, AEROGPU_CMD_COPY_BUFFER_SIZE + 4);
  pushU32(bytes, 1); // dst_buffer
  pushU32(bytes, 2); // src_buffer
  pushU64(bytes, 3n); // dst_offset_bytes
  pushU64(bytes, 4n); // src_offset_bytes
  pushU64(bytes, 5n); // size_bytes
  pushU32(bytes, 6); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0xdead_beef);

  const out = new Uint8Array(bytes);
  const decoded = decodeCmdCopyBufferPayload(out, 0);
  assert.equal(decoded.dstBuffer, 1);
  assert.equal(decoded.srcBuffer, 2);
  assert.equal(decoded.dstOffsetBytes, 3n);
  assert.equal(decoded.srcOffsetBytes, 4n);
  assert.equal(decoded.sizeBytes, 5n);
  assert.equal(decoded.flags, 6);
  assert.equal(decoded.reserved0, 0);
});

test("COPY_TEXTURE2D decoder accepts trailing bytes in cmd.size_bytes", () => {
  const bytes: number[] = [];
  pushU32(bytes, AerogpuCmdOpcode.CopyTexture2d);
  pushU32(bytes, AEROGPU_CMD_COPY_TEXTURE2D_SIZE + 4);
  pushU32(bytes, 10); // dst_texture
  pushU32(bytes, 11); // src_texture
  pushU32(bytes, 0); // dst_mip_level
  pushU32(bytes, 0); // dst_array_layer
  pushU32(bytes, 1); // src_mip_level
  pushU32(bytes, 2); // src_array_layer
  pushU32(bytes, 3); // dst_x
  pushU32(bytes, 4); // dst_y
  pushU32(bytes, 5); // src_x
  pushU32(bytes, 6); // src_y
  pushU32(bytes, 7); // width
  pushU32(bytes, 8); // height
  pushU32(bytes, 9); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0xdead_beef);

  const out = new Uint8Array(bytes);
  const decoded = decodeCmdCopyTexture2dPayload(out, 0);
  assert.equal(decoded.dstTexture, 10);
  assert.equal(decoded.srcTexture, 11);
  assert.equal(decoded.dstMipLevel, 0);
  assert.equal(decoded.dstArrayLayer, 0);
  assert.equal(decoded.srcMipLevel, 1);
  assert.equal(decoded.srcArrayLayer, 2);
  assert.equal(decoded.dstX, 3);
  assert.equal(decoded.dstY, 4);
  assert.equal(decoded.srcX, 5);
  assert.equal(decoded.srcY, 6);
  assert.equal(decoded.width, 7);
  assert.equal(decoded.height, 8);
  assert.equal(decoded.flags, 9);
  assert.equal(decoded.reserved0, 0);
});

test("SET_SHADER_CONSTANTS_B decoder accepts trailing bytes in cmd.size_bytes", () => {
  const bytes: number[] = [];
  pushU32(bytes, AerogpuCmdOpcode.SetShaderConstantsB);
  // Base packet + bool_count * 4 + trailing u32.
  pushU32(bytes, AEROGPU_CMD_SET_SHADER_CONSTANTS_B_SIZE + 2 * 4 + 4);
  pushU32(bytes, 1); // stage=Pixel
  pushU32(bytes, 0); // start_register
  pushU32(bytes, 2); // bool_count
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0); // b0 = false
  pushU32(bytes, 1); // b1 = true
  pushU32(bytes, 0xdead_beef); // trailing extension

  const out = new Uint8Array(bytes);
  const decoded = decodeCmdSetShaderConstantsBPayload(out, 0);
  assert.equal(decoded.stage, 1);
  assert.equal(decoded.startRegister, 0);
  assert.equal(decoded.boolCount, 2);
  assert.deepEqual(Array.from(decoded.data), [0, 1]);
});

test("SRV/UAV binding table decoders accept trailing bytes in cmd.size_bytes", () => {
  {
    const bytes: number[] = [];
    pushU32(bytes, AerogpuCmdOpcode.SetShaderResourceBuffers);
    pushU32(bytes, AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS_SIZE + AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE + 4);
    pushU32(bytes, 1); // shader_stage (pixel)
    pushU32(bytes, 0); // start_slot
    pushU32(bytes, 1); // buffer_count
    pushU32(bytes, 0); // reserved0
    // binding[0]
    pushU32(bytes, 10); // buffer
    pushU32(bytes, 0); // offset_bytes
    pushU32(bytes, 64); // size_bytes
    pushU32(bytes, 0); // reserved0
    pushU32(bytes, 0xdead_beef); // trailing extension

    const out = new Uint8Array(bytes);
    const decoded = decodeCmdSetShaderResourceBuffersPayload(out, 0);
    assert.equal(decoded.shaderStage, 1);
    assert.equal(decoded.startSlot, 0);
    assert.equal(decoded.bufferCount, 1);
    assert.equal(decoded.reserved0, 0);
    assert.equal(decoded.bindings.byteLength, AEROGPU_SHADER_RESOURCE_BUFFER_BINDING_SIZE);
    assert.equal(decoded.bindings.getUint32(0, true), 10);
    assert.equal(decoded.bindings.getUint32(4, true), 0);
    assert.equal(decoded.bindings.getUint32(8, true), 64);
  }

  {
    const bytes: number[] = [];
    pushU32(bytes, AerogpuCmdOpcode.SetUnorderedAccessBuffers);
    pushU32(bytes, AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS_SIZE + AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE + 4);
    pushU32(bytes, 2); // shader_stage (compute)
    pushU32(bytes, 0); // start_slot
    pushU32(bytes, 1); // uav_count
    pushU32(bytes, 0); // reserved0
    // binding[0]
    pushU32(bytes, 20); // buffer
    pushU32(bytes, 4); // offset_bytes
    pushU32(bytes, 128); // size_bytes
    pushU32(bytes, 0); // initial_count
    pushU32(bytes, 0xdead_beef); // trailing extension

    const out = new Uint8Array(bytes);
    const decoded = decodeCmdSetUnorderedAccessBuffersPayload(out, 0);
    assert.equal(decoded.shaderStage, 2);
    assert.equal(decoded.startSlot, 0);
    assert.equal(decoded.uavCount, 1);
    assert.equal(decoded.reserved0, 0);
    assert.equal(decoded.bindings.byteLength, AEROGPU_UNORDERED_ACCESS_BUFFER_BINDING_SIZE);
    assert.equal(decoded.bindings.getUint32(0, true), 20);
    assert.equal(decoded.bindings.getUint32(4, true), 4);
    assert.equal(decoded.bindings.getUint32(8, true), 128);
    assert.equal(decoded.bindings.getUint32(12, true), 0);
  }
});

test("SET_BLEND_STATE decoder accepts legacy 28-byte packets", () => {
  const bytes: number[] = [];
  pushU32(bytes, AEROGPU_CMD_STREAM_MAGIC);
  pushU32(bytes, AEROGPU_ABI_VERSION_U32);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 0); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0); // reserved1

  const cmdOffset = bytes.length;
  pushU32(bytes, AerogpuCmdOpcode.SetBlendState);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 1); // enable
  pushU32(bytes, AerogpuBlendFactor.One);
  pushU32(bytes, AerogpuBlendFactor.Zero);
  pushU32(bytes, AerogpuBlendOp.Add);
  bytes.push(0xf, 0, 0, 0); // write mask + padding

  const out = new Uint8Array(bytes);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  dv.setUint32(cmdOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, AEROGPU_CMD_SET_BLEND_STATE_SIZE_MIN, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, out.byteLength, true);
  assert.equal(out.byteLength % 4, 0);

  const packets = decodeCmdStreamView(out).packets;
  assert.equal(packets.length, 1);

  const decoded = decodeCmdSetBlendState(new DataView(out.buffer, out.byteOffset, out.byteLength), packets[0]!.offsetBytes);
  assert.equal(decoded.enable, true);
  assert.equal(decoded.srcFactor, AerogpuBlendFactor.One);
  assert.equal(decoded.dstFactor, AerogpuBlendFactor.Zero);
  assert.equal(decoded.blendOp, AerogpuBlendOp.Add);
  assert.equal(decoded.colorWriteMask, 0xf);

  assert.equal(decoded.srcFactorAlpha, decoded.srcFactor);
  assert.equal(decoded.dstFactorAlpha, decoded.dstFactor);
  assert.equal(decoded.blendOpAlpha, decoded.blendOp);
  assert.deepEqual(decoded.blendConstantRgba, [1, 1, 1, 1]);
  assert.equal(decoded.sampleMask >>> 0, 0xffff_ffff);
});

function buildBindShadersStream(extended: boolean, withExtraTrailing: boolean): Uint8Array {
  const bytes: number[] = [];
  pushU32(bytes, AEROGPU_CMD_STREAM_MAGIC);
  pushU32(bytes, AEROGPU_ABI_VERSION_U32);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 0); // flags
  pushU32(bytes, 0); // reserved0
  pushU32(bytes, 0); // reserved1

  const cmdOffset = bytes.length;
  pushU32(bytes, AerogpuCmdOpcode.BindShaders);
  pushU32(bytes, 0); // size_bytes (patched later)
  pushU32(bytes, 1); // vs
  pushU32(bytes, 2); // ps
  pushU32(bytes, 3); // cs
  pushU32(bytes, 0); // reserved0
  if (extended) {
    pushU32(bytes, 4); // gs
    pushU32(bytes, 5); // hs
    pushU32(bytes, 6); // ds
  }
  if (withExtraTrailing) {
    // Forward-compatible extension beyond known fields (ignored by current decoders).
    pushU32(bytes, 0xdead_beef);
  }

  const out = new Uint8Array(bytes);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  dv.setUint32(cmdOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, out.byteLength - cmdOffset, true);
  dv.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, out.byteLength, true);
  assert.equal(out.byteLength % 4, 0);
  assert.equal(cmdOffset, AEROGPU_CMD_STREAM_HEADER_SIZE);
  return out;
}

test("BIND_SHADERS decoders accept append-only extensions for additional stages", () => {
  const base = buildBindShadersStream(false, false);
  const baseWithTrailing = buildBindShadersStream(false, true);
  const extended = buildBindShadersStream(true, false);
  const extendedWithTrailing = buildBindShadersStream(true, true);

  const packetsBase = decodeCmdStreamView(base).packets;
  const packetsBaseTrailing = decodeCmdStreamView(baseWithTrailing).packets;
  const packetsExt = decodeCmdStreamView(extended).packets;
  const packetsExtTrailing = decodeCmdStreamView(extendedWithTrailing).packets;
  assert.equal(packetsBase.length, 1);
  assert.equal(packetsBaseTrailing.length, 1);
  assert.equal(packetsExt.length, 1);
  assert.equal(packetsExtTrailing.length, 1);

  assert.equal(packetsBase[0]!.hdr.opcode, AerogpuCmdOpcode.BindShaders);
  assert.equal(packetsBaseTrailing[0]!.hdr.opcode, AerogpuCmdOpcode.BindShaders);
  assert.equal(packetsExt[0]!.hdr.opcode, AerogpuCmdOpcode.BindShaders);
  assert.equal(packetsExtTrailing[0]!.hdr.opcode, AerogpuCmdOpcode.BindShaders);
  assert.equal(packetsBase[0]!.hdr.sizeBytes, AEROGPU_CMD_BIND_SHADERS_SIZE);
  assert.equal(packetsBaseTrailing[0]!.hdr.sizeBytes, AEROGPU_CMD_BIND_SHADERS_SIZE + 4);
  assert.equal(packetsExt[0]!.hdr.sizeBytes, AEROGPU_CMD_BIND_SHADERS_EX_SIZE);
  assert.equal(packetsExtTrailing[0]!.hdr.sizeBytes, AEROGPU_CMD_BIND_SHADERS_EX_SIZE + 4);

  const decodedBase = decodeCmdBindShadersPayload(base, packetsBase[0]!.offsetBytes);
  const decodedBaseTrailing = decodeCmdBindShadersPayload(baseWithTrailing, packetsBaseTrailing[0]!.offsetBytes);
  const decodedExt = decodeCmdBindShadersPayload(extended, packetsExt[0]!.offsetBytes);
  const decodedExtTrailing = decodeCmdBindShadersPayload(extendedWithTrailing, packetsExtTrailing[0]!.offsetBytes);

  // Packet-based decoders should agree with the byte+offset helpers.
  assert.deepEqual(decodeCmdBindShadersPayloadFromPacket(packetsBase[0]!), decodedBase);
  assert.deepEqual(decodeCmdBindShadersPayloadFromPacket(packetsBaseTrailing[0]!), decodedBaseTrailing);
  assert.deepEqual(decodeCmdBindShadersPayloadFromPacket(packetsExt[0]!), decodedExt);
  assert.deepEqual(decodeCmdBindShadersPayloadFromPacket(packetsExtTrailing[0]!), decodedExtTrailing);

  // Simulated legacy decoder: reads only the original VS/PS/CS fields (ignores any appended bytes).
  const decodeLegacy = (bytes: Uint8Array, cmdOffset: number) => {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return {
      vs: view.getUint32(cmdOffset + 8, true),
      ps: view.getUint32(cmdOffset + 12, true),
      cs: view.getUint32(cmdOffset + 16, true),
      reserved0: view.getUint32(cmdOffset + 20, true),
    };
  };
  const legacyBase = decodeLegacy(base, packetsBase[0]!.offsetBytes);
  const legacyBaseTrailing = decodeLegacy(baseWithTrailing, packetsBaseTrailing[0]!.offsetBytes);
  const legacyExt = decodeLegacy(extended, packetsExt[0]!.offsetBytes);
  const legacyExtTrailing = decodeLegacy(extendedWithTrailing, packetsExtTrailing[0]!.offsetBytes);

  // Legacy decode: original VS/PS/CS fields remain stable even when newer guests append fields.
  assert.deepEqual(
    { vs: decodedBase.vs, ps: decodedBase.ps, cs: decodedBase.cs, reserved0: decodedBase.reserved0 },
    { vs: decodedExt.vs, ps: decodedExt.ps, cs: decodedExt.cs, reserved0: decodedExt.reserved0 },
  );
  assert.deepEqual(legacyBaseTrailing, legacyBase);
  assert.deepEqual(
    { vs: decodedBaseTrailing.vs, ps: decodedBaseTrailing.ps, cs: decodedBaseTrailing.cs, reserved0: decodedBaseTrailing.reserved0 },
    { vs: decodedBase.vs, ps: decodedBase.ps, cs: decodedBase.cs, reserved0: decodedBase.reserved0 },
  );
  assert.equal(decodedBaseTrailing.ex, undefined);
  assert.deepEqual(legacyBase, legacyExt);
  assert.deepEqual(legacyExtTrailing, legacyExt);
  assert.deepEqual(
    { vs: decodedBase.vs, ps: decodedBase.ps, cs: decodedBase.cs, reserved0: decodedBase.reserved0 },
    { vs: 1, ps: 2, cs: 3, reserved0: 0 },
  );
  assert.deepEqual(legacyBase, { vs: 1, ps: 2, cs: 3, reserved0: 0 });
  assert.equal(decodedBase.ex, undefined);

  // Extended decode: appended GS/HS/DS handles are available to decoders that understand them.
  assert.deepEqual(decodedExt.ex, { gs: 4, hs: 5, ds: 6 });
  assert.deepEqual(decodedExtTrailing.ex, { gs: 4, hs: 5, ds: 6 });
});
