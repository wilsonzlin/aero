#pragma once

#include "../include/aerogpu_d3d9_umd.h"

#include <cstdint>

namespace aerogpu {

// D3D9 UMD DDI smoke-test call tracing.
//
// Design goals:
// - Logging/introspection only (no behavior changes).
// - Safe for dwm.exe: no allocations and no I/O on hot paths.
// - Low overhead: fixed-size buffer, atomic index, optional "seen" filtering.
//
// The trace is disabled by default and must be enabled via environment variables.
// See `docs/graphics/win7-d3d9-umd-tracing.md`.

constexpr uint64_t d3d9_trace_pack_u32_u32(uint32_t lo, uint32_t hi) {
  return static_cast<uint64_t>(lo) | (static_cast<uint64_t>(hi) << 32);
}

constexpr uint32_t d3d9_trace_unpack_lo_u32(uint64_t packed) {
  return static_cast<uint32_t>(packed & 0xFFFFFFFFull);
}

constexpr uint32_t d3d9_trace_unpack_hi_u32(uint64_t packed) {
  return static_cast<uint32_t>(packed >> 32);
}

inline uint64_t d3d9_trace_arg_ptr(const void* ptr) {
  return static_cast<uint64_t>(reinterpret_cast<uintptr_t>(ptr));
}

// Function identifiers for the D3D9UMDDI entrypoints implemented by this UMD.
enum class D3d9TraceFunc : uint16_t {
  OpenAdapter = 0,
  OpenAdapter2,
  OpenAdapterFromHdc,
  OpenAdapterFromLuid,

  AdapterClose,
  AdapterGetCaps,
  AdapterQueryAdapterInfo,
  AdapterCreateDevice,

  DeviceDestroy,
  DeviceCreateResource,
  DeviceOpenResource,
  DeviceOpenResource2,
  DeviceDestroyResource,
  DeviceCreateSwapChain,
  DeviceDestroySwapChain,
  DeviceGetSwapChain,
  DeviceSetSwapChain,
  DeviceReset,
  DeviceResetEx,
  DeviceCheckDeviceState,
  DeviceRotateResourceIdentities,
  DeviceLock,
  DeviceUnlock,
  DeviceGetRenderTargetData,
  DeviceCopyRects,
  DeviceSetRenderTarget,
  DeviceSetDepthStencil,
  DeviceSetViewport,
  DeviceSetScissorRect,
  DeviceSetTexture,
  DeviceSetSamplerState,
  DeviceSetRenderState,
  DeviceCreateVertexDecl,
  DeviceSetVertexDecl,
  DeviceDestroyVertexDecl,
  DeviceCreateShader,
  DeviceSetShader,
  DeviceDestroyShader,
  DeviceSetShaderConstF,
  DeviceBlt,
  DeviceColorFill,
  DeviceUpdateSurface,
  DeviceUpdateTexture,
  DeviceSetStreamSource,
  DeviceSetIndices,
  DeviceClear,
  DeviceDrawPrimitive,
  DeviceDrawIndexedPrimitive,
  DevicePresent,
  DevicePresentEx,
  DeviceSetMaximumFrameLatency,
  DeviceGetMaximumFrameLatency,
  DeviceGetPresentStats,
  DeviceGetLastPresentCount,
  DeviceFlush,
  DeviceWaitForVBlank,
  DeviceSetGPUThreadPriority,
  DeviceGetGPUThreadPriority,
  DeviceCheckResourceResidency,
  DeviceQueryResourceResidency,
  DeviceGetDisplayModeEx,
  DeviceComposeRects,
  DeviceCreateQuery,
  DeviceDestroyQuery,
  DeviceIssueQuery,
  DeviceGetQueryData,
  DeviceWaitForIdle,

  // New entrypoints should be appended to avoid renumbering existing trace IDs.
  DeviceSetFVF,
  DeviceDrawPrimitiveUP,

  // DDIs that were originally stubbed during bring-up. Some may become
  // implemented over time, but trace IDs are stable so entries are not reordered.
  DeviceSetTextureStageState,
  DeviceSetTransform,
  DeviceMultiplyTransform,
  DeviceSetClipPlane,
  DeviceSetShaderConstI,
  DeviceSetShaderConstB,
  DeviceSetMaterial,
  DeviceSetLight,
  DeviceLightEnable,
  DeviceSetNPatchMode,
  DeviceSetStreamSourceFreq,
  DeviceSetGammaRamp,
  DeviceCreateStateBlock,
  DeviceDeleteStateBlock,
  DeviceCaptureStateBlock,
  DeviceApplyStateBlock,
  DeviceValidateDevice,
  DeviceSetSoftwareVertexProcessing,
  DeviceSetCursorProperties,
  DeviceSetCursorPosition,
  DeviceShowCursor,
  DeviceSetPaletteEntries,
  DeviceSetCurrentTexturePalette,
  DeviceSetClipStatus,
  DeviceGetClipStatus,
  DeviceGetGammaRamp,
  DeviceDrawRectPatch,
  DeviceDrawTriPatch,
  DeviceDeletePatch,
  DeviceProcessVertices,
  DeviceGetRasterStatus,
  DeviceSetDialogBoxMode,
  DeviceDrawIndexedPrimitiveUP,
  DeviceGetSoftwareVertexProcessing,
  DeviceGetTransform,
  DeviceGetClipPlane,
  DeviceGetViewport,
  DeviceGetScissorRect,
  DeviceBeginStateBlock,
  DeviceEndStateBlock,
  DeviceGetMaterial,
  DeviceGetLight,
  DeviceGetLightEnable,
  DeviceGetRenderTarget,
  DeviceGetDepthStencil,
  DeviceGetTexture,
  DeviceGetTextureStageState,
  DeviceGetSamplerState,
  DeviceGetRenderState,
  DeviceGetPaletteEntries,
  DeviceGetCurrentTexturePalette,
  DeviceGetNPatchMode,
  DeviceGetFVF,
  DeviceGetVertexDecl,
  DeviceGetStreamSource,
  DeviceGetStreamSourceFreq,
  DeviceGetIndices,
  DeviceGetShader,
  DeviceGetShaderConstF,
  DeviceGetShaderConstI,
  DeviceGetShaderConstB,
  DeviceSetConvolutionMonoKernel,
  DeviceSetAutoGenFilterType,
  DeviceGetAutoGenFilterType,
  DeviceGenerateMipSubLevels,
  DeviceSetPriority,
  DeviceGetPriority,

  kCount,
};

// Trace record stored in the fixed-size trace buffer.
struct D3d9TraceRecord {
  uint64_t timestamp;
  uint32_t thread_id;
  uint32_t func_id;
  uint64_t arg0;
  uint64_t arg1;
  uint64_t arg2;
  uint64_t arg3;
  HRESULT hr;
};

void d3d9_trace_init_from_env();
void d3d9_trace_on_process_detach();
void d3d9_trace_maybe_dump_on_present(uint32_t present_count);
bool d3d9_trace_enabled();

// Helper for instrumenting entrypoints:
//   D3d9TraceCall trace(D3d9TraceFunc::DevicePresentEx, arg0, arg1, arg2, arg3);
//   ...
//   return trace.ret(S_OK);
//
// In non-tracing builds / when disabled, this compiles down to a couple of
// branches and no I/O.
class D3d9TraceCall {
public:
  D3d9TraceCall(D3d9TraceFunc func, uint64_t arg0, uint64_t arg1, uint64_t arg2, uint64_t arg3);
  ~D3d9TraceCall();

  D3d9TraceCall(const D3d9TraceCall&) = delete;
  D3d9TraceCall& operator=(const D3d9TraceCall&) = delete;
 
  HRESULT ret(HRESULT hr) {
    hr_ = hr;
    return hr;
  }

 private:
  D3d9TraceFunc func_ = D3d9TraceFunc::kCount;
  uint64_t arg0_ = 0;
  uint64_t arg1_ = 0;
  uint64_t arg2_ = 0;
  uint64_t arg3_ = 0;
  D3d9TraceRecord* record_ = nullptr;
  HRESULT hr_ = static_cast<HRESULT>(0x7FFFFFFF);
};

} // namespace aerogpu
