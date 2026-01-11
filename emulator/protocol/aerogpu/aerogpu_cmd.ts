// AeroGPU command stream layouts.
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.

import { parseAndValidateAbiVersionU32 } from "./aerogpu_pci.ts";

export type AerogpuHandle = number;

export const AEROGPU_CMD_STREAM_MAGIC = 0x444d4341; // "ACMD" LE

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

export const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC = 0x59414c49; // "ILAY" LE
export const AEROGPU_INPUT_LAYOUT_BLOB_VERSION = 1;

// Selected packet sizes (in bytes) from the C header for layout conformance tests.
export const AEROGPU_CMD_CREATE_BUFFER_SIZE = 40;
export const AEROGPU_CMD_CREATE_TEXTURE2D_SIZE = 56;
export const AEROGPU_CMD_DESTROY_RESOURCE_SIZE = 16;
export const AEROGPU_CMD_RESOURCE_DIRTY_RANGE_SIZE = 32;
export const AEROGPU_CMD_UPLOAD_RESOURCE_SIZE = 32;
export const AEROGPU_CMD_CREATE_SHADER_DXBC_SIZE = 24;
export const AEROGPU_CMD_DESTROY_SHADER_SIZE = 16;
export const AEROGPU_CMD_BIND_SHADERS_SIZE = 24;
export const AEROGPU_CMD_SET_SHADER_CONSTANTS_F_SIZE = 24;
export const AEROGPU_INPUT_LAYOUT_BLOB_HEADER_SIZE = 16;
export const AEROGPU_INPUT_LAYOUT_ELEMENT_DXGI_SIZE = 28;
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
