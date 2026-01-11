// AeroGPU command stream layouts.
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.

import { AEROGPU_ABI_VERSION_U32, parseAndValidateAbiVersionU32 } from "./aerogpu_pci.ts";

export type AerogpuHandle = number;

export const AEROGPU_CMD_STREAM_MAGIC = 0x444d4341; // "ACMD" LE
export const AEROGPU_CMD_STREAM_FLAG_NONE = 0;

export const AEROGPU_CMD_STREAM_HEADER_SIZE = 24;
export const AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC = 0;
export const AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION = 4;
export const AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES = 8;
export const AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS = 12;

export interface AerogpuCmdStreamHeader {
  abiVersion: number;
  sizeBytes: number;
  flags: number;
}

export function decodeCmdStreamHeader(view: DataView, byteOffset = 0): AerogpuCmdStreamHeader {
  if (view.byteLength < byteOffset + AEROGPU_CMD_STREAM_HEADER_SIZE) {
    throw new Error("Buffer too small for aerogpu_cmd_stream_header");
  }

  const magic = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, true);
  if (magic !== AEROGPU_CMD_STREAM_MAGIC) {
    throw new Error(`Bad command stream magic: 0x${magic.toString(16)}`);
  }

  const abiVersion = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, true);
  parseAndValidateAbiVersionU32(abiVersion);

  const sizeBytes = view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_CMD_STREAM_HEADER_SIZE) {
    throw new Error(`cmd_stream.size_bytes too small: ${sizeBytes}`);
  }

  return {
    abiVersion,
    sizeBytes,
    flags: view.getUint32(byteOffset + AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, true),
  };
}

export const AEROGPU_CMD_HDR_SIZE = 8;
export const AEROGPU_CMD_HDR_OFF_OPCODE = 0;
export const AEROGPU_CMD_HDR_OFF_SIZE_BYTES = 4;

export const AerogpuCmdOpcode = {
  Nop: 0,
  DebugMarker: 1,

  CreateBuffer: 0x100,
  CreateTexture2d: 0x101,
  DestroyResource: 0x102,
  ResourceDirtyRange: 0x103,
  UploadResource: 0x104,
  CopyBuffer: 0x105,
  CopyTexture2d: 0x106,

  CreateShaderDxbc: 0x200,
  DestroyShader: 0x201,
  BindShaders: 0x202,
  SetShaderConstantsF: 0x203,
  CreateInputLayout: 0x204,
  DestroyInputLayout: 0x205,
  SetInputLayout: 0x206,

  SetBlendState: 0x300,
  SetDepthStencilState: 0x301,
  SetRasterizerState: 0x302,

  SetRenderTargets: 0x400,
  SetViewport: 0x401,
  SetScissor: 0x402,

  SetVertexBuffers: 0x500,
  SetIndexBuffer: 0x501,
  SetPrimitiveTopology: 0x502,
  SetTexture: 0x510,
  SetSamplerState: 0x511,
  SetRenderState: 0x512,

  Clear: 0x600,
  Draw: 0x601,
  DrawIndexed: 0x602,

  Present: 0x700,
  PresentEx: 0x701,

  ExportSharedSurface: 0x710,
  ImportSharedSurface: 0x711,

  Flush: 0x720,
} as const;

export type AerogpuCmdOpcode = (typeof AerogpuCmdOpcode)[keyof typeof AerogpuCmdOpcode];
export const AerogpuShaderStage = {
  Vertex: 0,
  Pixel: 1,
  Compute: 2,
} as const;

export type AerogpuShaderStage = (typeof AerogpuShaderStage)[keyof typeof AerogpuShaderStage];

export const AerogpuIndexFormat = {
  Uint16: 0,
  Uint32: 1,
} as const;

export type AerogpuIndexFormat = (typeof AerogpuIndexFormat)[keyof typeof AerogpuIndexFormat];
export const AerogpuPrimitiveTopology = {
  PointList: 1,
  LineList: 2,
  LineStrip: 3,
  TriangleList: 4,
  TriangleStrip: 5,
  TriangleFan: 6,
} as const;

export type AerogpuPrimitiveTopology =
  (typeof AerogpuPrimitiveTopology)[keyof typeof AerogpuPrimitiveTopology];

export const AEROGPU_RESOURCE_USAGE_NONE = 0;
export const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER = 1 << 0;
export const AEROGPU_RESOURCE_USAGE_INDEX_BUFFER = 1 << 1;
export const AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER = 1 << 2;
export const AEROGPU_RESOURCE_USAGE_TEXTURE = 1 << 3;
export const AEROGPU_RESOURCE_USAGE_RENDER_TARGET = 1 << 4;
export const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL = 1 << 5;
export const AEROGPU_RESOURCE_USAGE_SCANOUT = 1 << 6;

export const AEROGPU_COPY_FLAG_NONE = 0;
export const AEROGPU_COPY_FLAG_WRITEBACK_DST = 1 << 0;

export const AEROGPU_MAX_RENDER_TARGETS = 8;

export const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC = 0x59414c49; // "ILAY" LE
export const AEROGPU_INPUT_LAYOUT_BLOB_VERSION = 1;

export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE = 16;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_MAGIC = 0;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_VERSION = 4;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_ELEMENT_COUNT = 8;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_OFF_RESERVED0 = 12;

export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE = 28;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_NAME_HASH = 0;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_SEMANTIC_INDEX = 4;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_DXGI_FORMAT = 8;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT = 12;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_ALIGNED_BYTE_OFFSET = 16;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INPUT_SLOT_CLASS = 20;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_OFF_INSTANCE_DATA_STEP_RATE = 24;

export const AEROGPU_CLEAR_COLOR = 1 << 0;
export const AEROGPU_CLEAR_DEPTH = 1 << 1;
export const AEROGPU_CLEAR_STENCIL = 1 << 2;

export const AEROGPU_PRESENT_FLAG_NONE = 0;
export const AEROGPU_PRESENT_FLAG_VSYNC = 1 << 0;

// Selected packet sizes (in bytes) from the C header for layout conformance tests.
export const AEROGPU_CMD_CREATE_BUFFER_SIZE = 40;
export const AEROGPU_CMD_CREATE_TEXTURE2D_SIZE = 56;
export const AEROGPU_CMD_DESTROY_RESOURCE_SIZE = 16;
export const AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE = 32;
export const AEROGPU_CMD_UPLOAD_RESOURCE_SIZE = 32;
export const AEROGPU_CMD_COPY_BUFFER_SIZE = 48;
export const AEROGPU_CMD_COPY_TEXTURE2D_SIZE = 64;
export const AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE = 24;
export const AEROGPU_CMD_DESTROY_SHADER_SIZE = 16;
export const AEROGPU_CMD_BIND_SHADERS_SIZE = 24;
export const AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE = 24;
export const AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE = 20;
export const AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE = 16;
export const AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE = 16;
export const AEROGPU_CMD_SET_BLEND_STATE_SIZE = 28;
export const AEROGPU_CMD_SET_DEPTH_STENCIL_STATE_SIZE = 28;
export const AEROGPU_CMD_SET_RASTERIZER_STATE_SIZE = 32;
export const AEROGPU_CMD_SET_RENDER_TARGETS_SIZE = 48;
export const AEROGPU_CMD_SET_VIEWPORT_SIZE = 32;
export const AEROGPU_CMD_SET_SCISSOR_SIZE = 24;
export const AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE = 16;
export const AEROGPU_CMD_SET_INDEX_BUFFER_SIZE = 24;
export const AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE = 16;
export const AEROGPU_CMD_SET_TEXTURE_SIZE = 24;
export const AEROGPU_CMD_SET_SAMPLER_STATE_SIZE = 24;
export const AEROGPU_CMD_SET_RENDER_STATE_SIZE = 16;
export const AEROGPU_CMD_CLEAR_SIZE = 36;
export const AEROGPU_CMD_DRAW_SIZE = 24;
export const AEROGPU_CMD_DRAW_INDEXED_SIZE = 28;
export const AEROGPU_CMD_PRESENT_SIZE = 16;
export const AEROGPU_CMD_PRESENT_EX_SIZE = 24;
export const AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE = 24;
export const AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE = 24;
export const AEROGPU_CMD_FLUSH_SIZE = 16;

export interface AerogpuCmdHdr {
  opcode: number;
  sizeBytes: number;
}

export function decodeCmdHdr(view: DataView, byteOffset = 0): AerogpuCmdHdr {
  if (view.byteLength < byteOffset + AEROGPU_CMD_HDR_SIZE) {
    throw new Error("Buffer too small for aerogpu_cmd_hdr");
  }

  const opcode = view.getUint32(byteOffset + AEROGPU_CMD_HDR_OFF_OPCODE, true);
  const sizeBytes = view.getUint32(byteOffset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, true);
  if (sizeBytes < AEROGPU_CMD_HDR_SIZE) {
    throw new Error(`cmd_hdr.size_bytes too small: ${sizeBytes}`);
  }
  if (sizeBytes % 4 !== 0) {
    throw new Error(`cmd_hdr.size_bytes not 4-byte aligned: ${sizeBytes}`);
  }

  return { opcode, sizeBytes };
}

export interface AerogpuVertexBufferBinding {
  buffer: AerogpuHandle;
  strideBytes: number;
  offsetBytes: number;
}

function alignUp(v: number, a: number): number {
  return (v + (a - 1)) & ~(a - 1);
}

/**
 * Safe command stream builder for `aerogpu_cmd.h`.
 *
 * Primarily intended for tests/fixtures and host-side tooling.
 */
export class AerogpuCmdWriter {
  private buf: ArrayBuffer = new ArrayBuffer(0);
  private view: DataView = new DataView(this.buf);
  private len = 0;

  constructor() {
    this.reset();
  }

  reset(): void {
    this.buf = new ArrayBuffer(AEROGPU_CMD_STREAM_HEADER_SIZE);
    this.view = new DataView(this.buf);
    this.len = AEROGPU_CMD_STREAM_HEADER_SIZE;

    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_MAGIC, AEROGPU_CMD_STREAM_MAGIC, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_ABI_VERSION, AEROGPU_ABI_VERSION_U32, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, this.len, true);
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_FLAGS, 0, true);
  }

  finish(): Uint8Array {
    this.view.setUint32(AEROGPU_CMD_STREAM_HEADER_OFF_SIZE_BYTES, this.len, true);
    return new Uint8Array(this.buf, 0, this.len).slice();
  }

  private ensureCapacity(capacity: number): void {
    if (this.buf.byteLength >= capacity) return;
    let newCap = this.buf.byteLength;
    while (newCap < capacity) newCap = Math.max(64, newCap * 2);
    const next = new ArrayBuffer(newCap);
    new Uint8Array(next).set(new Uint8Array(this.buf, 0, this.len));
    this.buf = next;
    this.view = new DataView(this.buf);
  }

  private appendRaw(opcode: AerogpuCmdOpcode, cmdSize: number): number {
    const alignedSize = alignUp(cmdSize, 4);
    const offset = this.len;
    this.ensureCapacity(offset + alignedSize);
    new Uint8Array(this.buf, offset, alignedSize).fill(0);
    this.view.setUint32(offset + AEROGPU_CMD_HDR_OFF_OPCODE, opcode, true);
    this.view.setUint32(offset + AEROGPU_CMD_HDR_OFF_SIZE_BYTES, alignedSize, true);
    this.len += alignedSize;
    return offset;
  }

  createBuffer(
    bufferHandle: AerogpuHandle,
    usageFlags: number,
    sizeBytes: bigint,
    backingAllocId: number,
    backingOffsetBytes: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CreateBuffer, AEROGPU_CMD_CREATE_BUFFER_SIZE);
    this.view.setUint32(base + 8, bufferHandle, true);
    this.view.setUint32(base + 12, usageFlags, true);
    this.view.setBigUint64(base + 16, sizeBytes, true);
    this.view.setUint32(base + 24, backingAllocId, true);
    this.view.setUint32(base + 28, backingOffsetBytes, true);
  }

  createTexture2d(
    textureHandle: AerogpuHandle,
    usageFlags: number,
    format: number,
    width: number,
    height: number,
    mipLevels: number,
    arrayLayers: number,
    rowPitchBytes: number,
    backingAllocId: number,
    backingOffsetBytes: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.CreateTexture2d, AEROGPU_CMD_CREATE_TEXTURE2D_SIZE);
    this.view.setUint32(base + 8, textureHandle, true);
    this.view.setUint32(base + 12, usageFlags, true);
    this.view.setUint32(base + 16, format, true);
    this.view.setUint32(base + 20, width, true);
    this.view.setUint32(base + 24, height, true);
    this.view.setUint32(base + 28, mipLevels, true);
    this.view.setUint32(base + 32, arrayLayers, true);
    this.view.setUint32(base + 36, rowPitchBytes, true);
    this.view.setUint32(base + 40, backingAllocId, true);
    this.view.setUint32(base + 44, backingOffsetBytes, true);
  }

  destroyResource(resourceHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyResource, AEROGPU_CMD_DESTROY_RESOURCE_SIZE);
    this.view.setUint32(base + 8, resourceHandle, true);
  }

  resourceDirtyRange(resourceHandle: AerogpuHandle, offsetBytes: bigint, sizeBytes: bigint): void {
    const base = this.appendRaw(AerogpuCmdOpcode.ResourceDirtyRange, AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE);
    this.view.setUint32(base + 8, resourceHandle, true);
    this.view.setBigUint64(base + 16, offsetBytes, true);
    this.view.setBigUint64(base + 24, sizeBytes, true);
  }

  uploadResource(resourceHandle: AerogpuHandle, offsetBytes: bigint, data: Uint8Array): void {
    const unpadded = AEROGPU_CMD_UPLOAD_RESOURCE_SIZE + data.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.UploadResource, unpadded);
    this.view.setUint32(base + 8, resourceHandle, true);
    this.view.setBigUint64(base + 16, offsetBytes, true);
    this.view.setBigUint64(base + 24, BigInt(data.byteLength), true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_UPLOAD_RESOURCE_SIZE, data.byteLength).set(data);
  }

  createShaderDxbc(shaderHandle: AerogpuHandle, stage: AerogpuShaderStage, dxbcBytes: Uint8Array): void {
    const unpadded = AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE + dxbcBytes.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.CreateShaderDxbc, unpadded);
    this.view.setUint32(base + 8, shaderHandle, true);
    this.view.setUint32(base + 12, stage, true);
    this.view.setUint32(base + 16, dxbcBytes.byteLength, true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE, dxbcBytes.byteLength).set(dxbcBytes);
  }

  destroyShader(shaderHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyShader, AEROGPU_CMD_DESTROY_SHADER_SIZE);
    this.view.setUint32(base + 8, shaderHandle, true);
  }

  bindShaders(vs: AerogpuHandle, ps: AerogpuHandle, cs: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.BindShaders, AEROGPU_CMD_BIND_SHADERS_SIZE);
    this.view.setUint32(base + 8, vs, true);
    this.view.setUint32(base + 12, ps, true);
    this.view.setUint32(base + 16, cs, true);
  }

  createInputLayout(inputLayoutHandle: AerogpuHandle, blob: Uint8Array): void {
    const unpadded = AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE + blob.byteLength;
    const base = this.appendRaw(AerogpuCmdOpcode.CreateInputLayout, unpadded);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
    this.view.setUint32(base + 12, blob.byteLength, true);
    new Uint8Array(this.buf, base + AEROGPU_CMD_CREATE_INPUT_LAYOUT_SIZE, blob.byteLength).set(blob);
  }

  destroyInputLayout(inputLayoutHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DestroyInputLayout, AEROGPU_CMD_DESTROY_INPUT_LAYOUT_SIZE);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
  }

  setInputLayout(inputLayoutHandle: AerogpuHandle): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetInputLayout, AEROGPU_CMD_SET_INPUT_LAYOUT_SIZE);
    this.view.setUint32(base + 8, inputLayoutHandle, true);
  }

  setRenderTargets(colors: readonly AerogpuHandle[], depthStencil: AerogpuHandle): void {
    if (colors.length > AEROGPU_MAX_RENDER_TARGETS) {
      throw new Error(`too many render targets: ${colors.length}`);
    }
    const base = this.appendRaw(AerogpuCmdOpcode.SetRenderTargets, AEROGPU_CMD_SET_RENDER_TARGETS_SIZE);
    this.view.setUint32(base + 8, colors.length, true);
    this.view.setUint32(base + 12, depthStencil, true);
    for (let i = 0; i < colors.length; i++) {
      this.view.setUint32(base + 16 + i * 4, colors[i], true);
    }
  }

  setViewport(x: number, y: number, width: number, height: number, minDepth: number, maxDepth: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetViewport, AEROGPU_CMD_SET_VIEWPORT_SIZE);
    this.view.setFloat32(base + 8, x, true);
    this.view.setFloat32(base + 12, y, true);
    this.view.setFloat32(base + 16, width, true);
    this.view.setFloat32(base + 20, height, true);
    this.view.setFloat32(base + 24, minDepth, true);
    this.view.setFloat32(base + 28, maxDepth, true);
  }

  setScissor(x: number, y: number, width: number, height: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetScissor, AEROGPU_CMD_SET_SCISSOR_SIZE);
    this.view.setInt32(base + 8, x, true);
    this.view.setInt32(base + 12, y, true);
    this.view.setInt32(base + 16, width, true);
    this.view.setInt32(base + 20, height, true);
  }

  setVertexBuffers(startSlot: number, bindings: readonly AerogpuVertexBufferBinding[]): void {
    const unpadded = AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE + bindings.length * 16;
    const base = this.appendRaw(AerogpuCmdOpcode.SetVertexBuffers, unpadded);
    this.view.setUint32(base + 8, startSlot, true);
    this.view.setUint32(base + 12, bindings.length, true);
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i];
      const off = base + AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE + i * 16;
      this.view.setUint32(off + 0, b.buffer, true);
      this.view.setUint32(off + 4, b.strideBytes, true);
      this.view.setUint32(off + 8, b.offsetBytes, true);
    }
  }

  setIndexBuffer(buffer: AerogpuHandle, format: AerogpuIndexFormat, offsetBytes: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetIndexBuffer, AEROGPU_CMD_SET_INDEX_BUFFER_SIZE);
    this.view.setUint32(base + 8, buffer, true);
    this.view.setUint32(base + 12, format, true);
    this.view.setUint32(base + 16, offsetBytes, true);
  }

  setPrimitiveTopology(topology: AerogpuPrimitiveTopology): void {
    const base = this.appendRaw(AerogpuCmdOpcode.SetPrimitiveTopology, AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE);
    this.view.setUint32(base + 8, topology, true);
  }

  clear(flags: number, colorRgba: [number, number, number, number], depth: number, stencil: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Clear, AEROGPU_CMD_CLEAR_SIZE);
    this.view.setUint32(base + 8, flags, true);
    for (let i = 0; i < 4; i++) {
      this.view.setFloat32(base + 12 + i * 4, colorRgba[i], true);
    }
    this.view.setFloat32(base + 28, depth, true);
    this.view.setUint32(base + 32, stencil, true);
  }

  draw(vertexCount: number, instanceCount: number, firstVertex: number, firstInstance: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Draw, AEROGPU_CMD_DRAW_SIZE);
    this.view.setUint32(base + 8, vertexCount, true);
    this.view.setUint32(base + 12, instanceCount, true);
    this.view.setUint32(base + 16, firstVertex, true);
    this.view.setUint32(base + 20, firstInstance, true);
  }

  drawIndexed(
    indexCount: number,
    instanceCount: number,
    firstIndex: number,
    baseVertex: number,
    firstInstance: number,
  ): void {
    const base = this.appendRaw(AerogpuCmdOpcode.DrawIndexed, AEROGPU_CMD_DRAW_INDEXED_SIZE);
    this.view.setUint32(base + 8, indexCount, true);
    this.view.setUint32(base + 12, instanceCount, true);
    this.view.setUint32(base + 16, firstIndex, true);
    this.view.setInt32(base + 20, baseVertex, true);
    this.view.setUint32(base + 24, firstInstance, true);
  }

  present(scanoutId: number, flags: number): void {
    const base = this.appendRaw(AerogpuCmdOpcode.Present, AEROGPU_CMD_PRESENT_SIZE);
    this.view.setUint32(base + 8, scanoutId, true);
    this.view.setUint32(base + 12, flags, true);
  }

  flush(): void {
    this.appendRaw(AerogpuCmdOpcode.Flush, AEROGPU_CMD_FLUSH_SIZE);
  }
}
