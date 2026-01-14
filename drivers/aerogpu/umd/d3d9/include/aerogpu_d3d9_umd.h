// AeroGPU D3D9Ex user-mode display driver (UMD) - public entrypoints / ABI surface.
//
// Goal: build as a Windows 7 SP1 (WDDM 1.1) D3D9 user-mode display driver.
//
// This header supports two modes:
// - WDK mode (`AEROGPU_UMD_USE_WDK_HEADERS=1`): include the official Win7 D3D9 UMD DDI headers
//   (`d3dumddi.h`, `d3d9umddi.h`, ...).
// - Portable mode (default): define a *minimal* subset of the Win7 D3D9 UMD DDI ABI using the
//   *canonical WDK names* (D3DDDI_*, D3D9DDI_*). This keeps the repo self-contained and lets
//   host-side tests compile without the Windows SDK/WDK.
//
// NOTE: The portable subset is intentionally incomplete; it only contains the pieces exercised
// by the current translation layer.

#pragma once

#include <stddef.h>
#include <stdint.h>

// -------------------------------------------------------------------------------------------------
// Platform / calling convention
// -------------------------------------------------------------------------------------------------

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN 1
  #endif
  #ifndef NOMINMAX
    #define NOMINMAX 1
  #endif
  #include <windows.h>
#else
typedef void* HANDLE;
typedef void* HWND;
typedef void* HDC;
typedef uint32_t DWORD;
typedef int32_t LONG;
typedef uint32_t UINT;
typedef int32_t HRESULT;
typedef uint8_t BYTE;
typedef uint16_t WORD;
typedef int32_t BOOL;
typedef struct _LARGE_INTEGER {
  int64_t QuadPart;
} LARGE_INTEGER;

typedef struct _GUID {
  uint32_t Data1;
  uint16_t Data2;
  uint16_t Data3;
  uint8_t Data4[8];
} GUID;

typedef struct _RECT {
  long left;
  long top;
  long right;
  long bottom;
} RECT;

typedef struct _POINT {
  long x;
  long y;
} POINT;

  #ifndef TRUE
    #define TRUE 1
  #endif
  #ifndef FALSE
    #define FALSE 0
  #endif

  #ifndef APIENTRY
    #define APIENTRY
  #endif

  // The Win7 WDK headers use `__stdcall` for many D3D9 UMD entrypoints on x86.
  // Portable (non-Windows) builds don't need stdcall calling conventions, but
  // the translation layer uses template specializations on `__stdcall` function
  // pointer types to remain compatible with different header vintages.
  //
  // On x86-64, GCC ignores the `stdcall` attribute (there is only one ABI),
  // which would make the `Ret(__stdcall*)(...)` and `Ret(*)(...)` template
  // specializations collide. Use `ms_abi` to keep the types distinct in
  // portable builds.
  #ifndef __stdcall
    #if defined(__x86_64__) || defined(_M_X64)
      #define __stdcall __attribute__((ms_abi))
    #elif defined(__i386__) || defined(_M_IX86)
      #define __stdcall __attribute__((stdcall))
    #else
      #define __stdcall
    #endif
  #endif

  #ifndef S_OK
    #define S_OK ((HRESULT)0)
  #endif
  #ifndef S_FALSE
    #define S_FALSE ((HRESULT)1)
  #endif
  #ifndef E_FAIL
    #define E_FAIL ((HRESULT)0x80004005L)
  #endif
  #ifndef E_INVALIDARG
    #define E_INVALIDARG ((HRESULT)0x80070057L)
  #endif
  #ifndef E_OUTOFMEMORY
    #define E_OUTOFMEMORY ((HRESULT)0x8007000EL)
  #endif
  #ifndef E_NOTIMPL
    #define E_NOTIMPL ((HRESULT)0x80004001L)
  #endif
  #ifndef AEROGPU_LUID_DEFINED
    #define AEROGPU_LUID_DEFINED
typedef struct _LUID {
  DWORD LowPart;
  LONG HighPart;
} LUID;
  #endif
#endif

// Windows-style HRESULT helpers (portable builds).
//
// When building on Windows, <windows.h> provides these macros. For portable host
// tests we define them here so shared code can use SUCCEEDED/FAILED without
// pulling in any platform headers.
#ifndef SUCCEEDED
  #define SUCCEEDED(hr) (((HRESULT)(hr)) >= 0)
#endif
#ifndef FAILED
  #define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

// Common D3D9 HRESULTs used by D3D9Ex GetData/CreateQuery paths.
#ifndef D3DERR_NOTAVAILABLE
  #define D3DERR_NOTAVAILABLE ((HRESULT)0x8876086AL)
#endif
#ifndef D3DERR_DEVICELOST
  // D3DERR_DEVICELOST (0x88760868). Returned to signal a device-lost/hung state
  // (e.g. WDDM submission failures). Keep a local definition so portable builds
  // don't require d3d9.h.
  #define D3DERR_DEVICELOST ((HRESULT)0x88760868L)
#endif
#ifndef D3DERR_INVALIDCALL
  #define D3DERR_INVALIDCALL ((HRESULT)0x8876086CUL)
#endif
#ifndef D3DERR_WASSTILLDRAWING
  #define D3DERR_WASSTILLDRAWING ((HRESULT)0x8876021CL)
#endif

// Export / calling convention helpers.
#ifndef APIENTRY
  #define APIENTRY
#endif

#define AEROGPU_D3D9_CALL APIENTRY

#if defined(_WIN32)
  #define AEROGPU_D3D9_EXPORT extern "C" __declspec(dllexport)
#else
  #define AEROGPU_D3D9_EXPORT extern "C"
#endif

// Build switch compatibility:
// - D3D10/11 UMD uses `AEROGPU_UMD_USE_WDK_HEADERS`.
// - The D3D9 UMD historically used `AEROGPU_D3D9_USE_WDK_DDI`.
//
// Treat them as synonyms so build systems can use either.
#if !defined(AEROGPU_UMD_USE_WDK_HEADERS)
  #if defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    #define AEROGPU_UMD_USE_WDK_HEADERS 1
  #else
    #define AEROGPU_UMD_USE_WDK_HEADERS 0
  #endif
#endif

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #ifndef AEROGPU_D3D9_USE_WDK_DDI
    #define AEROGPU_D3D9_USE_WDK_DDI 1
  #endif
#endif

// -------------------------------------------------------------------------------------------------
// D3D9 UMD DDI ABI surface
// -------------------------------------------------------------------------------------------------

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // WDK mode: compile against the real Win7 D3D9 UMD DDI headers.
  #include <d3dkmthk.h>
  #include <d3d9caps.h>
  #include <d3d9types.h>
  #include <d3dumddi.h>
  #include <d3d9umddi.h>
#else

#if defined(_WIN32)
  // Portable mode on Windows: rely on the Windows SDK for the classic D3D9 type
  // definitions (e.g. D3DMATRIX/D3DTRANSFORMSTATETYPE) so host-side tests can
  // compile without the WDK.
  #include <d3d9types.h>
#endif

#if !defined(_WIN32)
// ---- D3D9 public types/constants (subset) ------------------------------------
// Repository builds do not include the Windows SDK/WDK, but the UMD still needs
// ABI-compatible public structs (D3DCAPS9, D3DADAPTER_IDENTIFIER9) to satisfy
// Win7 D3D9Ex runtime behavior.

// Shader version encoding (mirrors d3d9caps.h).
#ifndef D3DVS_VERSION
  #define D3DVS_VERSION(major, minor) (0xFFFE0000u | ((uint32_t)(major) << 8) | (uint32_t)(minor))
#endif
#ifndef D3DPS_VERSION
  #define D3DPS_VERSION(major, minor) (0xFFFF0000u | ((uint32_t)(major) << 8) | (uint32_t)(minor))
#endif

// D3DPRESENT_INTERVAL_* bitmask values (from d3d9types.h).
#ifndef D3DPRESENT_INTERVAL_ONE
  #define D3DPRESENT_INTERVAL_ONE 0x00000001u
#endif
#ifndef D3DPRESENT_INTERVAL_IMMEDIATE
  #define D3DPRESENT_INTERVAL_IMMEDIATE 0x80000000u
#endif

// D3DDEVTYPE_* (from d3d9types.h). Only the values needed by D3DCAPS9 are
// mirrored here so host-side tests can run without the Windows SDK.
#ifndef D3DDEVTYPE_HAL
  #define D3DDEVTYPE_HAL 1u
#endif

// ---- Fixed-function lighting public types (subset) ----------------------------
// Some fixed-function state (material/lights) is cached in the UMD even in
// portable builds, so we mirror the canonical d3d9types.h structs here.
typedef struct _D3DVECTOR {
  float x;
  float y;
  float z;
} D3DVECTOR;

typedef struct _D3DCOLORVALUE {
  float r;
  float g;
  float b;
  float a;
} D3DCOLORVALUE;

typedef struct _D3DMATERIAL9 {
  D3DCOLORVALUE Diffuse;
  D3DCOLORVALUE Ambient;
  D3DCOLORVALUE Specular;
  D3DCOLORVALUE Emissive;
  float Power;
} D3DMATERIAL9;

typedef enum _D3DLIGHTTYPE {
  D3DLIGHT_POINT = 1,
  D3DLIGHT_SPOT = 2,
  D3DLIGHT_DIRECTIONAL = 3,
} D3DLIGHTTYPE;

typedef struct _D3DLIGHT9 {
  D3DLIGHTTYPE Type;
  D3DCOLORVALUE Diffuse;
  D3DCOLORVALUE Specular;
  D3DCOLORVALUE Ambient;
  D3DVECTOR Position;
  D3DVECTOR Direction;
  float Range;
  float Falloff;
  float Attenuation0;
  float Attenuation1;
  float Attenuation2;
  float Theta;
  float Phi;
} D3DLIGHT9;

// D3DCAPS2_* (from d3d9caps.h).
#ifndef D3DCAPS2_CANRENDERWINDOWED
  #define D3DCAPS2_CANRENDERWINDOWED 0x00080000u
#endif
#ifndef D3DCAPS2_CANSHARERESOURCE
  #define D3DCAPS2_CANSHARERESOURCE 0x00100000u
#endif

// D3DCAPS_* (from d3d9caps.h).
#ifndef D3DCAPS_READ_SCANLINE
  #define D3DCAPS_READ_SCANLINE 0x00020000u
#endif

// D3DDEVCAPS_* (from d3d9caps.h). Keep this conservative; only define bits we
// need to reason about caps/feature invariants in portable builds.
#ifndef D3DDEVCAPS_HWTRANSFORMANDLIGHT
  #define D3DDEVCAPS_HWTRANSFORMANDLIGHT 0x00010000u
#endif
#ifndef D3DDEVCAPS_DRAWPRIMITIVES2
  #define D3DDEVCAPS_DRAWPRIMITIVES2 0x00002000u
#endif
#ifndef D3DDEVCAPS_DRAWPRIMITIVES2EX
  #define D3DDEVCAPS_DRAWPRIMITIVES2EX 0x00008000u
#endif
#ifndef D3DDEVCAPS_QUINTICRTPATCHES
  #define D3DDEVCAPS_QUINTICRTPATCHES 0x00200000u
#endif
#ifndef D3DDEVCAPS_RTPATCHES
  #define D3DDEVCAPS_RTPATCHES 0x00400000u
#endif
#ifndef D3DDEVCAPS_NPATCHES
  #define D3DDEVCAPS_NPATCHES 0x01000000u
#endif

// D3DPMISCCAPS_* (from d3d9caps.h).
#ifndef D3DPMISCCAPS_CLIPTLVERTS
  #define D3DPMISCCAPS_CLIPTLVERTS 0x00000200u
#endif
#ifndef D3DPMISCCAPS_SEPARATEALPHABLEND
  #define D3DPMISCCAPS_SEPARATEALPHABLEND 0x00004000u
#endif
#ifndef D3DPMISCCAPS_BLENDOP
  #define D3DPMISCCAPS_BLENDOP 0x00008000u
#endif

// D3DPRASTERCAPS_* (from d3d9caps.h).
#ifndef D3DPRASTERCAPS_SCISSORTEST
  #define D3DPRASTERCAPS_SCISSORTEST 0x00001000u
#endif
#ifndef D3DPRASTERCAPS_ZTEST
  #define D3DPRASTERCAPS_ZTEST 0x00000010u
#endif
#ifndef D3DPRASTERCAPS_CULLCCW
  #define D3DPRASTERCAPS_CULLCCW 0x00000020u
#endif
#ifndef D3DPRASTERCAPS_CULLCW
  #define D3DPRASTERCAPS_CULLCW 0x00000040u
#endif

// D3DPTFILTERCAPS_* (from d3d9caps.h).
#ifndef D3DPTFILTERCAPS_MINFPOINT
  #define D3DPTFILTERCAPS_MINFPOINT 0x00000100u
#endif
#ifndef D3DPTFILTERCAPS_MINFLINEAR
  #define D3DPTFILTERCAPS_MINFLINEAR 0x00000200u
#endif
#ifndef D3DPTFILTERCAPS_MIPFPOINT
  #define D3DPTFILTERCAPS_MIPFPOINT 0x00010000u
#endif
#ifndef D3DPTFILTERCAPS_MIPFLINEAR
  #define D3DPTFILTERCAPS_MIPFLINEAR 0x00020000u
#endif
#ifndef D3DPTFILTERCAPS_MAGFPOINT
  #define D3DPTFILTERCAPS_MAGFPOINT 0x01000000u
#endif
#ifndef D3DPTFILTERCAPS_MAGFLINEAR
  #define D3DPTFILTERCAPS_MAGFLINEAR 0x02000000u
#endif

// D3DPBLENDCAPS_* (from d3d9caps.h).
#ifndef D3DPBLENDCAPS_ZERO
  #define D3DPBLENDCAPS_ZERO 0x00000001u
#endif
#ifndef D3DPBLENDCAPS_ONE
  #define D3DPBLENDCAPS_ONE 0x00000002u
#endif
#ifndef D3DPBLENDCAPS_SRCALPHA
  #define D3DPBLENDCAPS_SRCALPHA 0x00000010u
#endif
#ifndef D3DPBLENDCAPS_INVSRCALPHA
  #define D3DPBLENDCAPS_INVSRCALPHA 0x00000020u
#endif
#ifndef D3DPBLENDCAPS_DESTALPHA
  #define D3DPBLENDCAPS_DESTALPHA 0x00000040u
#endif
#ifndef D3DPBLENDCAPS_INVDESTALPHA
  #define D3DPBLENDCAPS_INVDESTALPHA 0x00000080u
#endif
#ifndef D3DPBLENDCAPS_BLENDFACTOR
  #define D3DPBLENDCAPS_BLENDFACTOR 0x00002000u
#endif
#ifndef D3DPBLENDCAPS_INVBLENDFACTOR
  #define D3DPBLENDCAPS_INVBLENDFACTOR 0x00004000u
#endif

// D3DBLENDOPCAPS_* (from d3d9caps.h).
#ifndef D3DBLENDOPCAPS_ADD
  #define D3DBLENDOPCAPS_ADD 0x00000001u
#endif
#ifndef D3DBLENDOPCAPS_SUBTRACT
  #define D3DBLENDOPCAPS_SUBTRACT 0x00000002u
#endif
#ifndef D3DBLENDOPCAPS_REVSUBTRACT
  #define D3DBLENDOPCAPS_REVSUBTRACT 0x00000004u
#endif
#ifndef D3DBLENDOPCAPS_MIN
  #define D3DBLENDOPCAPS_MIN 0x00000008u
#endif
#ifndef D3DBLENDOPCAPS_MAX
  #define D3DBLENDOPCAPS_MAX 0x00000010u
#endif

// D3DPCMPCAPS_* (from d3d9caps.h).
#ifndef D3DPCMPCAPS_NEVER
  #define D3DPCMPCAPS_NEVER 0x00000001u
#endif
#ifndef D3DPCMPCAPS_LESS
  #define D3DPCMPCAPS_LESS 0x00000002u
#endif
#ifndef D3DPCMPCAPS_EQUAL
  #define D3DPCMPCAPS_EQUAL 0x00000004u
#endif
#ifndef D3DPCMPCAPS_LESSEQUAL
  #define D3DPCMPCAPS_LESSEQUAL 0x00000008u
#endif
#ifndef D3DPCMPCAPS_GREATER
  #define D3DPCMPCAPS_GREATER 0x00000010u
#endif
#ifndef D3DPCMPCAPS_NOTEQUAL
  #define D3DPCMPCAPS_NOTEQUAL 0x00000020u
#endif
#ifndef D3DPCMPCAPS_GREATEREQUAL
  #define D3DPCMPCAPS_GREATEREQUAL 0x00000040u
#endif
#ifndef D3DPCMPCAPS_ALWAYS
  #define D3DPCMPCAPS_ALWAYS 0x00000080u
#endif

// D3DSTENCILCAPS_* (from d3d9caps.h).
#ifndef D3DSTENCILCAPS_KEEP
  #define D3DSTENCILCAPS_KEEP 0x00000001u
#endif
#ifndef D3DSTENCILCAPS_ZERO
  #define D3DSTENCILCAPS_ZERO 0x00000002u
#endif
#ifndef D3DSTENCILCAPS_REPLACE
  #define D3DSTENCILCAPS_REPLACE 0x00000004u
#endif
#ifndef D3DSTENCILCAPS_INCRSAT
  #define D3DSTENCILCAPS_INCRSAT 0x00000008u
#endif
#ifndef D3DSTENCILCAPS_DECRSAT
  #define D3DSTENCILCAPS_DECRSAT 0x00000010u
#endif
#ifndef D3DSTENCILCAPS_INVERT
  #define D3DSTENCILCAPS_INVERT 0x00000020u
#endif
#ifndef D3DSTENCILCAPS_INCR
  #define D3DSTENCILCAPS_INCR 0x00000040u
#endif
#ifndef D3DSTENCILCAPS_DECR
  #define D3DSTENCILCAPS_DECR 0x00000080u
#endif
#ifndef D3DSTENCILCAPS_TWOSIDED
  #define D3DSTENCILCAPS_TWOSIDED 0x00000100u
#endif

// D3DFVFCAPS_* (from d3d9caps.h).
//
// Note: `D3DCAPS9::FVFCaps` encodes the *maximum* number of texture coordinate sets
// supported by the fixed-function pipeline in the low bits (mask below). The
// remaining bits are feature flags (e.g. point-size).
#ifndef D3DFVFCAPS_TEXCOORDCOUNTMASK
  #define D3DFVFCAPS_TEXCOORDCOUNTMASK 0x0000FFFFu
#endif
#ifndef D3DFVFCAPS_DONOTSTRIPELEMENTS
  #define D3DFVFCAPS_DONOTSTRIPELEMENTS 0x00080000u
#endif
#ifndef D3DFVFCAPS_PSIZE
  #define D3DFVFCAPS_PSIZE 0x00100000u
#endif

// D3DPSHADECAPS_* (from d3d9caps.h).
#ifndef D3DPSHADECAPS_COLORGOURAUDRGB
  #define D3DPSHADECAPS_COLORGOURAUDRGB 0x00000008u
#endif

// D3DPTADDRESSCAPS_* (from d3d9caps.h).
#ifndef D3DPTADDRESSCAPS_WRAP
  #define D3DPTADDRESSCAPS_WRAP 0x00000001u
#endif
#ifndef D3DPTADDRESSCAPS_MIRROR
  #define D3DPTADDRESSCAPS_MIRROR 0x00000002u
#endif
#ifndef D3DPTADDRESSCAPS_CLAMP
  #define D3DPTADDRESSCAPS_CLAMP 0x00000004u
#endif

// D3DTEXOPCAPS_* (texture stage operations; subset from d3d9caps.h).
#ifndef D3DTEXOPCAPS_DISABLE
  #define D3DTEXOPCAPS_DISABLE 0x00000001u
#endif
#ifndef D3DTEXOPCAPS_SELECTARG1
  #define D3DTEXOPCAPS_SELECTARG1 0x00000002u
#endif
#ifndef D3DTEXOPCAPS_SELECTARG2
  #define D3DTEXOPCAPS_SELECTARG2 0x00000004u
#endif
#ifndef D3DTEXOPCAPS_MODULATE
  #define D3DTEXOPCAPS_MODULATE 0x00000008u
#endif
#ifndef D3DTEXOPCAPS_MODULATE2X
  #define D3DTEXOPCAPS_MODULATE2X 0x00000010u
#endif
#ifndef D3DTEXOPCAPS_MODULATE4X
  #define D3DTEXOPCAPS_MODULATE4X 0x00000020u
#endif
#ifndef D3DTEXOPCAPS_ADD
  #define D3DTEXOPCAPS_ADD 0x00000040u
#endif
#ifndef D3DTEXOPCAPS_ADDSIGNED
  #define D3DTEXOPCAPS_ADDSIGNED 0x00000080u
#endif
#ifndef D3DTEXOPCAPS_SUBTRACT
  #define D3DTEXOPCAPS_SUBTRACT 0x00000200u
#endif
#ifndef D3DTEXOPCAPS_BLENDDIFFUSEALPHA
  #define D3DTEXOPCAPS_BLENDDIFFUSEALPHA 0x00000800u
#endif
#ifndef D3DTEXOPCAPS_BLENDTEXTUREALPHA
  #define D3DTEXOPCAPS_BLENDTEXTUREALPHA 0x00001000u
#endif

// D3DPTEXTURECAPS_* (subset).
#ifndef D3DPTEXTURECAPS_POW2
  #define D3DPTEXTURECAPS_POW2 0x00000002u
#endif
#ifndef D3DPTEXTURECAPS_ALPHA
  #define D3DPTEXTURECAPS_ALPHA 0x00000004u
#endif
#ifndef D3DPTEXTURECAPS_MIPMAP
  #define D3DPTEXTURECAPS_MIPMAP 0x00000008u
#endif
#ifndef D3DPTEXTURECAPS_CUBEMAP
  #define D3DPTEXTURECAPS_CUBEMAP 0x00000200u
#endif

// D3DDTCAPS_* (vertex declaration types; subset from d3d9caps.h).
#ifndef D3DDTCAPS_FLOAT1
  #define D3DDTCAPS_FLOAT1 0x00000001u
#endif
#ifndef D3DDTCAPS_FLOAT2
  #define D3DDTCAPS_FLOAT2 0x00000002u
#endif
#ifndef D3DDTCAPS_FLOAT3
  #define D3DDTCAPS_FLOAT3 0x00000004u
#endif
#ifndef D3DDTCAPS_FLOAT4
  #define D3DDTCAPS_FLOAT4 0x00000008u
#endif
#ifndef D3DDTCAPS_D3DCOLOR
  #define D3DDTCAPS_D3DCOLOR 0x00000010u
#endif
#ifndef D3DDTCAPS_UBYTE4
  #define D3DDTCAPS_UBYTE4 0x00000020u
#endif
#ifndef D3DDTCAPS_UBYTE4N
  #define D3DDTCAPS_UBYTE4N 0x00000100u
#endif
#ifndef D3DDTCAPS_SHORT2
  #define D3DDTCAPS_SHORT2 0x00000040u
#endif
#ifndef D3DDTCAPS_SHORT4
  #define D3DDTCAPS_SHORT4 0x00000080u
#endif
#ifndef D3DDTCAPS_SHORT2N
  #define D3DDTCAPS_SHORT2N 0x00000200u
#endif
#ifndef D3DDTCAPS_SHORT4N
  #define D3DDTCAPS_SHORT4N 0x00000400u
#endif
#ifndef D3DDTCAPS_USHORT2N
  #define D3DDTCAPS_USHORT2N 0x00000800u
#endif
#ifndef D3DDTCAPS_USHORT4N
  #define D3DDTCAPS_USHORT4N 0x00001000u
#endif

typedef struct _D3DVSHADERCAPS2_0 {
  DWORD Caps;
  int32_t DynamicFlowControlDepth;
  int32_t NumTemps;
  int32_t StaticFlowControlDepth;
  int32_t NumInstructionSlots;
} D3DVSHADERCAPS2_0;

typedef struct _D3DPSHADERCAPS2_0 {
  DWORD Caps;
  int32_t DynamicFlowControlDepth;
  int32_t NumTemps;
  int32_t StaticFlowControlDepth;
  int32_t NumInstructionSlots;
} D3DPSHADERCAPS2_0;

// Full D3DCAPS9 layout (Win7-era; from d3d9caps.h).
 typedef struct _D3DCAPS9 {
   DWORD DeviceType;
   UINT AdapterOrdinal;
   DWORD Caps;
   DWORD Caps2;
   DWORD Caps3;
   DWORD PresentationIntervals;
   DWORD CursorCaps;
   DWORD DevCaps;
   DWORD PrimitiveMiscCaps;
   DWORD RasterCaps;
   DWORD ZCmpCaps;
   DWORD SrcBlendCaps;
   DWORD DestBlendCaps;
   // Supported blend operations (D3DBLENDOPCAPS_*). Present in the Win7-era
   // D3DCAPS9 layout and required to correctly advertise D3DRS_BLENDOP support.
   DWORD BlendOpCaps;
   DWORD AlphaCmpCaps;
   DWORD ShadeCaps;
   DWORD TextureCaps;
   DWORD TextureFilterCaps;
   DWORD CubeTextureFilterCaps;
  DWORD VolumeTextureFilterCaps;
  DWORD TextureAddressCaps;
  DWORD VolumeTextureAddressCaps;
  DWORD LineCaps;
  DWORD MaxTextureWidth;
  DWORD MaxTextureHeight;
  DWORD MaxVolumeExtent;
  DWORD MaxTextureRepeat;
  DWORD MaxTextureAspectRatio;
  DWORD MaxAnisotropy;
  float MaxVertexW;
  float GuardBandLeft;
  float GuardBandTop;
  float GuardBandRight;
  float GuardBandBottom;
  float ExtentsAdjust;
  DWORD StencilCaps;
  DWORD FVFCaps;
  DWORD TextureOpCaps;
  DWORD MaxTextureBlendStages;
  DWORD MaxSimultaneousTextures;
  DWORD VertexProcessingCaps;
  DWORD MaxActiveLights;
  DWORD MaxUserClipPlanes;
  DWORD MaxVertexBlendMatrices;
  DWORD MaxVertexBlendMatrixIndex;
  float MaxPointSize;
  DWORD MaxPrimitiveCount;
  DWORD MaxVertexIndex;
  DWORD MaxStreams;
  DWORD MaxStreamStride;
  DWORD VertexShaderVersion;
  DWORD MaxVertexShaderConst;
  DWORD PixelShaderVersion;
  float PixelShader1xMaxValue;
  DWORD DevCaps2;
  float MaxNpatchTessellationLevel;
  DWORD Reserved5;
  UINT MasterAdapterOrdinal;
  UINT AdapterOrdinalInGroup;
  UINT NumberOfAdaptersInGroup;
  DWORD DeclTypes;
  DWORD NumSimultaneousRTs;
  DWORD StretchRectFilterCaps;
  D3DVSHADERCAPS2_0 VS20Caps;
  D3DPSHADERCAPS2_0 PS20Caps;
  DWORD VertexTextureFilterCaps;
  DWORD MaxVShaderInstructionsExecuted;
  DWORD MaxPShaderInstructionsExecuted;
  DWORD MaxVertexShader30InstructionSlots;
  DWORD MaxPixelShader30InstructionSlots;
} D3DCAPS9;

typedef struct _D3DADAPTER_IDENTIFIER9 {
  char Driver[512];
  char Description[512];
  char DeviceName[32];
  LARGE_INTEGER DriverVersion;
  DWORD VendorId;
  DWORD DeviceId;
  DWORD SubSysId;
  DWORD Revision;
  GUID DeviceIdentifier;
  DWORD WHQLLevel;
} D3DADAPTER_IDENTIFIER9;

// ---- Fixed-function transforms (subset) ---------------------------------------
// The Win7 D3D9 runtime frequently uses the SetTransform DDIs even when no user
// shaders are bound (fixed-function vertex processing). Provide the minimal
// public ABI needed by the UMD's state cache and host-side tests.
typedef uint32_t D3DTRANSFORMSTATETYPE;

// D3DMATRIX (from d3d9types.h). The real SDK exposes a union with _11/_12/etc
// fields; for the UMD we only require an ABI-compatible 16-float layout.
typedef struct _D3DMATRIX {
  float m[4][4];
} D3DMATRIX;

// Common D3DTRANSFORMSTATETYPE numeric values (from d3d9types.h).
// Keep these optional: code can still use raw numeric values if needed.
#ifndef D3DTS_VIEW
  #define D3DTS_VIEW 2u
#endif
#ifndef D3DTS_PROJECTION
  #define D3DTS_PROJECTION 3u
#endif
#ifndef D3DTS_WORLD
  #define D3DTS_WORLD 256u
#endif

typedef enum _D3DDDICAPS_TYPE {
  D3DDDICAPS_GETD3D9CAPS = 1,
  D3DDDICAPS_GETFORMATCOUNT = 2,
  D3DDDICAPS_GETFORMAT = 3,
  D3DDDICAPS_GETMULTISAMPLEQUALITYLEVELS = 4,
} D3DDDICAPS_TYPE;

typedef enum _D3DDDI_QUERYADAPTERINFO_TYPE {
  D3DDDIQUERYADAPTERINFO_GETADAPTERIDENTIFIER = 1,
  D3DDDIQUERYADAPTERINFO_GETADAPTERLUID = 2,
} D3DDDI_QUERYADAPTERINFO_TYPE;

#endif // !defined(_WIN32)

// ---- Minimal handle shims -----------------------------------------------------
// D3D9 UMD DDI handle types are opaque driver-private pointers. The WDK models
// them as tiny wrapper structs with a single `pDrvPrivate` field; mirror that
// layout so code can be compiled both with and without the WDK headers.

typedef struct _D3DDDI_HADAPTER {
  void* pDrvPrivate;
} D3DDDI_HADAPTER;

typedef struct _D3DDDI_HDEVICE {
  void* pDrvPrivate;
} D3DDDI_HDEVICE;

typedef struct _D3DDDI_HRESOURCE {
  void* pDrvPrivate;
} D3DDDI_HRESOURCE;

typedef struct _D3D9DDI_HSWAPCHAIN {
  void* pDrvPrivate;
} D3D9DDI_HSWAPCHAIN;

typedef struct _D3D9DDI_HSHADER {
  void* pDrvPrivate;
} D3D9DDI_HSHADER;

typedef struct _D3D9DDI_HVERTEXDECL {
  void* pDrvPrivate;
} D3D9DDI_HVERTEXDECL;

typedef struct _D3D9DDI_HQUERY {
  void* pDrvPrivate;
} D3D9DDI_HQUERY;

typedef struct _D3D9DDI_HSTATEBLOCK {
  void* pDrvPrivate;
} D3D9DDI_HSTATEBLOCK;

// Handle for D3D9 patch rendering APIs (DrawRectPatch/DrawTriPatch/DeletePatch).
typedef struct _D3D9DDI_HPATCH {
  void* pDrvPrivate;
} D3D9DDI_HPATCH;

// ---- Callback-table shims -----------------------------------------------------
// The real callback tables are large and defined in `d3dumddi.h`. For portable
// builds we only need opaque placeholders (we store the pointers).

typedef struct _D3DDDI_ADAPTERCALLBACKS {
  void* pfnDummy;
} D3DDDI_ADAPTERCALLBACKS;

typedef struct _D3DDDI_ADAPTERCALLBACKS2 {
  void* pfnDummy;
} D3DDDI_ADAPTERCALLBACKS2;

// Forward declarations for function tables referenced by the OpenAdapter arg
// structs.
typedef struct _D3D9DDI_ADAPTERFUNCS D3D9DDI_ADAPTERFUNCS;
typedef struct _D3D9DDI_DEVICEFUNCS D3D9DDI_DEVICEFUNCS;

// ---- Common DDI enums/types (subset) -----------------------------------------
typedef uint32_t D3DDDIFORMAT;

typedef enum _D3DDDIPRIMITIVETYPE {
  D3DDDIPT_POINTLIST = 1,
  D3DDDIPT_LINELIST = 2,
  D3DDDIPT_LINESTRIP = 3,
  D3DDDIPT_TRIANGLELIST = 4,
  D3DDDIPT_TRIANGLESTRIP = 5,
  D3DDDIPT_TRIANGLEFAN = 6,
} D3DDDIPRIMITIVETYPE;

// ---- Patch rendering (DrawRectPatch/DrawTriPatch) -----------------------------
// Minimal public D3D9 patch types used by D3D9 patch DDIs.
//
// These mirror the public D3D9 API structs from d3d9types.h so host-side tests
// can compile without the Windows SDK/WDK.
typedef enum _D3DBASISTYPE {
  D3DBASIS_BEZIER = 0,
  D3DBASIS_BSPLINE = 1,
  D3DBASIS_CATMULL_ROM = 2,
} D3DBASISTYPE;

typedef enum _D3DDEGREETYPE {
  D3DDEGREE_LINEAR = 1,
  D3DDEGREE_QUADRATIC = 2,
  D3DDEGREE_CUBIC = 3,
  D3DDEGREE_QUINTIC = 5,
} D3DDEGREETYPE;

typedef struct _D3DRECTPATCH_INFO {
  UINT StartVertexOffset;
  UINT NumVertices;
  D3DBASISTYPE Basis;
  D3DDEGREETYPE Degree;
} D3DRECTPATCH_INFO;

typedef struct _D3DTRIPATCH_INFO {
  UINT StartVertexOffset;
  UINT NumVertices;
  D3DBASISTYPE Basis;
  D3DDEGREETYPE Degree;
} D3DTRIPATCH_INFO;

typedef struct _D3DDDIVIEWPORTINFO {
  float X;
  float Y;
  float Width;
  float Height;
  float MinZ;
  float MaxZ;
} D3DDDIVIEWPORTINFO;

typedef struct _D3DDDI_LOCKEDBOX {
  void* pData;
  uint32_t RowPitch;
  uint32_t SlicePitch;
} D3DDDI_LOCKEDBOX;

// ---- Minimal Win7/WDDM 1.1 device callbacks ----------------------------------
//
// For WDDM submissions the D3D9 runtime passes a `D3DDDI_DEVICECALLBACKS` table
// during CreateDevice. The UMD must call into this table to create a kernel-mode
// device/context and to submit DMA buffers (Render/Present).
//
// We intentionally define a small ABI slice here so the UMD can be built without
// WDK headers. The layouts are validated via:
//   drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/
//
// Notes:
// - Win7 kernel handles (`D3DKMT_HANDLE`) are always 32-bit.
// - AeroGPU uses a "no patch list" strategy and submits with NumPatchLocations=0.
// - The runtime may rotate the DMA buffer / allocation list pointers over time;
//   render/present callbacks can return updated pointers for the next submission.

typedef uint32_t D3DKMT_HANDLE;

typedef struct _D3DDDI_ALLOCATIONLIST {
  D3DKMT_HANDLE hAllocation;
  union {
    struct {
      UINT WriteOperation : 1;
      UINT DoNotRetireInstance : 1;
      UINT Offer : 1;
      UINT Reserved : 29;
    };
    UINT Value;
  };
  UINT AllocationListSlotId;
} D3DDDI_ALLOCATIONLIST;

// Patch list is unused by AeroGPU ("no patch list" strategy). Keep a placeholder
// type so we can hold pointers/sizes provided by the runtime.
typedef struct _D3DDDI_PATCHLOCATIONLIST {
  UINT dummy;
} D3DDDI_PATCHLOCATIONLIST;

typedef struct _D3DDDIARG_CREATEDEVICE {
  void* hAdapter;
  D3DKMT_HANDLE hDevice; // out
} D3DDDIARG_CREATEDEVICE;

typedef struct _D3DDDIARG_DESTROYDEVICE {
  D3DKMT_HANDLE hDevice;
} D3DDDIARG_DESTROYDEVICE;

typedef struct _D3DDDIARG_CREATECONTEXTFLAGS {
  union {
    struct {
      UINT NullRendering : 1;
      UINT Reserved : 31;
    };
    UINT Value;
  };
} D3DDDIARG_CREATECONTEXTFLAGS;

typedef struct _D3DDDIARG_CREATECONTEXT {
  D3DKMT_HANDLE hDevice;
  UINT NodeOrdinal;
  UINT EngineAffinity;
  D3DDDIARG_CREATECONTEXTFLAGS Flags;
  void* pPrivateDriverData;      // in
  UINT PrivateDriverDataSize;    // in
  D3DKMT_HANDLE hContext;    // out
  D3DKMT_HANDLE hSyncObject; // out
  void* pCommandBuffer;      // out
  UINT CommandBufferSize;    // out (bytes)
  D3DDDI_ALLOCATIONLIST* pAllocationList; // out
  UINT AllocationListSize;               // out (entries)
  D3DDDI_PATCHLOCATIONLIST* pPatchLocationList; // out
  UINT PatchLocationListSize;                   // out (entries)
  void* pDmaBufferPrivateData;   // out (optional; sized by KMD caps)
  UINT DmaBufferPrivateDataSize; // out (bytes)
} D3DDDIARG_CREATECONTEXT;

typedef struct _D3DDDIARG_DESTROYCONTEXT {
  D3DKMT_HANDLE hContext;
} D3DDDIARG_DESTROYCONTEXT;

typedef struct _D3DDDIARG_DESTROYSYNCHRONIZATIONOBJECT {
  D3DKMT_HANDLE hSyncObject;
} D3DDDIARG_DESTROYSYNCHRONIZATIONOBJECT;

// SubmitCommand callback args (Win7 D3D9 runtimes commonly route submissions
// through this entrypoint instead of Render/Present).
typedef struct _D3DDDIARG_SUBMITCOMMAND {
  D3DKMT_HANDLE hContext;
  void* pCommandBuffer;
  UINT CommandLength;     // bytes used
  UINT CommandBufferSize; // bytes capacity
  D3DDDI_ALLOCATIONLIST* pAllocationList;
  UINT AllocationListSize; // entries used (legacy: no NumAllocations field)
  D3DDDI_PATCHLOCATIONLIST* pPatchLocationList;
  UINT PatchLocationListSize; // entries used
  void* pDmaBufferPrivateData;
  UINT DmaBufferPrivateDataSize; // bytes
  // Fence outputs (WDK header-dependent).
  //
  // Win7-era headers commonly expose a 32-bit SubmissionFenceId. Newer header
  // vintages can also include 64-bit fence value fields.
  UINT SubmissionFenceId; // out (legacy 32-bit fence value)
  uint64_t NewFenceValue; // out (preferred 64-bit fence value when present)
  uint64_t FenceValue;    // out (alternate 64-bit fence value)
  uint64_t* pFenceValue;  // out (alternate pointer form)
} D3DDDIARG_SUBMITCOMMAND;

typedef struct _D3DDDICB_RENDER {
  D3DKMT_HANDLE hContext;
  void* pCommandBuffer;
  UINT CommandLength;     // bytes used
  UINT CommandBufferSize; // bytes capacity
  D3DDDI_ALLOCATIONLIST* pAllocationList;
  UINT AllocationListSize; // entries capacity
  UINT NumAllocations;     // entries used
  D3DDDI_PATCHLOCATIONLIST* pPatchLocationList;
  UINT PatchLocationListSize; // entries capacity
  UINT NumPatchLocations;     // entries used
  void* pDmaBufferPrivateData;
  UINT DmaBufferPrivateDataSize; // bytes
  // Win7/WDDM 1.1 submission fences are 32-bit (ULONG).
  UINT SubmissionFenceId; // out
  void* pNewCommandBuffer; // out
  UINT NewCommandBufferSize;
  D3DDDI_ALLOCATIONLIST* pNewAllocationList; // out
  UINT NewAllocationListSize;
  D3DDDI_PATCHLOCATIONLIST* pNewPatchLocationList; // out
  UINT NewPatchLocationListSize;
} D3DDDICB_RENDER;

typedef struct _D3DDDICB_PRESENT {
  D3DKMT_HANDLE hContext;
  void* pCommandBuffer;
  UINT CommandLength;     // bytes used
  UINT CommandBufferSize; // bytes capacity
  D3DDDI_ALLOCATIONLIST* pAllocationList;
  UINT AllocationListSize; // entries capacity
  UINT NumAllocations;     // entries used
  D3DDDI_PATCHLOCATIONLIST* pPatchLocationList;
  UINT PatchLocationListSize; // entries capacity
  UINT NumPatchLocations;     // entries used
  void* pDmaBufferPrivateData;
  UINT DmaBufferPrivateDataSize; // bytes
  UINT SubmissionFenceId; // out
  void* pNewCommandBuffer; // out
  UINT NewCommandBufferSize;
  D3DDDI_ALLOCATIONLIST* pNewAllocationList; // out
  UINT NewAllocationListSize;
  D3DDDI_PATCHLOCATIONLIST* pNewPatchLocationList; // out
  UINT NewPatchLocationListSize;
} D3DDDICB_PRESENT;

typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_CREATEDEVICE)(D3DDDIARG_CREATEDEVICE* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_DESTROYDEVICE)(D3DDDIARG_DESTROYDEVICE* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_CREATECONTEXT)(D3DDDIARG_CREATECONTEXT* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_CREATECONTEXT2)(D3DDDIARG_CREATECONTEXT* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_DESTROYCONTEXT)(D3DDDIARG_DESTROYCONTEXT* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_DESTROYSYNCOBJECT)(D3DDDIARG_DESTROYSYNCHRONIZATIONOBJECT* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_SUBMITCOMMAND)(D3DDDIARG_SUBMITCOMMAND* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_RENDER)(D3DDDICB_RENDER* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_PRESENT)(D3DDDICB_PRESENT* pData);

typedef struct _D3DDDI_DEVICECALLBACKS {
  // DMA buffer/resource allocation management.
  // NOTE: In the Win7 WDK ABI, `pfnAllocateCb` is the first member (offset 0).
  void* pfnAllocateCb;
  void* pfnDeallocateCb;
  PFND3DDDICB_SUBMITCOMMAND pfnSubmitCommandCb;
  PFND3DDDICB_RENDER pfnRenderCb;
  PFND3DDDICB_PRESENT pfnPresentCb;
  void* pfnWaitForSynchronizationObjectCb;
  void* pfnLockCb;
  void* pfnUnlockCb;
  void* pfnSetErrorCb;

  // Device/context lifecycle.
  PFND3DDDICB_CREATEDEVICE pfnCreateDeviceCb;
  PFND3DDDICB_DESTROYDEVICE pfnDestroyDeviceCb;
  PFND3DDDICB_CREATECONTEXT2 pfnCreateContextCb2;
  PFND3DDDICB_CREATECONTEXT pfnCreateContextCb;
  PFND3DDDICB_DESTROYCONTEXT pfnDestroyContextCb;
  PFND3DDDICB_DESTROYSYNCOBJECT pfnDestroySynchronizationObjectCb;

  // DMA buffer acquisition helper (optional).
  void* pfnGetCommandBufferCb;
} D3DDDI_DEVICECALLBACKS;

// -----------------------------------------------------------------------------
// Portable ABI sanity checks (anchors)
// -----------------------------------------------------------------------------
// These offsets are validated against Win7-era WDK headers via the probe in:
//   drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/
// Keep a few compile-time anchors here so portable builds do not silently drift.
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnAllocateCb) == 0,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnAllocateCb must be at offset 0");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnDeallocateCb) == sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)),
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnDeallocateCb offset drift");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnSubmitCommandCb) ==
                  sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)) * 2,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnSubmitCommandCb offset drift");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnRenderCb) ==
                  sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)) * 3,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnRenderCb offset drift");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnPresentCb) ==
                  sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)) * 4,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnPresentCb offset drift");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb2) ==
                  sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)) * 11,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnCreateContextCb2 offset drift");
static_assert(offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb) ==
                  sizeof(decltype(D3DDDI_DEVICECALLBACKS{}.pfnAllocateCb)) * 12,
              "D3DDDI_DEVICECALLBACKS ABI mismatch: pfnCreateContextCb offset drift");

static_assert(offsetof(D3DDDIARG_CREATECONTEXT, pPrivateDriverData) == 16,
              "D3DDDIARG_CREATECONTEXT ABI mismatch: pPrivateDriverData offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, hDevice) == 0,
              "D3DDDIARG_CREATECONTEXT ABI mismatch: hDevice offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, NodeOrdinal) == 4,
              "D3DDDIARG_CREATECONTEXT ABI mismatch: NodeOrdinal offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, EngineAffinity) == 8,
              "D3DDDIARG_CREATECONTEXT ABI mismatch: EngineAffinity offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, Flags) == 12,
              "D3DDDIARG_CREATECONTEXT ABI mismatch: Flags offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, PrivateDriverDataSize) ==
                  offsetof(D3DDDIARG_CREATECONTEXT, pPrivateDriverData) + sizeof(void*),
              "D3DDDIARG_CREATECONTEXT ABI mismatch: PrivateDriverDataSize offset drift");
static_assert(offsetof(D3DDDIARG_CREATECONTEXT, hContext) ==
                  offsetof(D3DDDIARG_CREATECONTEXT, PrivateDriverDataSize) + sizeof(UINT),
              "D3DDDIARG_CREATECONTEXT ABI mismatch: hContext offset drift");

static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, hContext) == 0,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: hContext offset drift");
#if UINTPTR_MAX == 0xFFFFFFFFu
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) == 4,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pCommandBuffer offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) == 8,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: CommandLength offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) == 12,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: CommandBufferSize offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) == 16,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pAllocationList offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) == 20,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: AllocationListSize offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) == 24,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pPatchLocationList offset drift (x86)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) == 28,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: PatchLocationListSize offset drift (x86)");
#else
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) == 8,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pCommandBuffer offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) == 16,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: CommandLength offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) == 20,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: CommandBufferSize offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) == 24,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pAllocationList offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) == 32,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: AllocationListSize offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) == 40,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: pPatchLocationList offset drift (x64)");
static_assert(offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) == 48,
              "D3DDDIARG_SUBMITCOMMAND ABI mismatch: PatchLocationListSize offset drift (x64)");
#endif

// ---- Adapter open ABI ---------------------------------------------------------
typedef struct _D3DDDIARG_OPENADAPTER {
  UINT Interface;
  UINT Version;
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3DDDI_HADAPTER hAdapter; // out
  D3D9DDI_ADAPTERFUNCS* pAdapterFuncs; // out
} D3DDDIARG_OPENADAPTER;

typedef struct _D3DDDIARG_OPENADAPTER2 {
  UINT Interface;
  UINT Version;
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3DDDI_HADAPTER hAdapter; // out
  D3D9DDI_ADAPTERFUNCS* pAdapterFuncs; // out
} D3DDDIARG_OPENADAPTER2;

// -----------------------------------------------------------------------------
// Portable ABI sanity checks (anchors)
// -----------------------------------------------------------------------------
static_assert(offsetof(D3DDDIARG_OPENADAPTER, pAdapterCallbacks) == 8,
              "D3DDDIARG_OPENADAPTER ABI mismatch: pAdapterCallbacks offset drift");
static_assert(offsetof(D3DDDIARG_OPENADAPTER2, pAdapterCallbacks) == 8,
              "D3DDDIARG_OPENADAPTER2 ABI mismatch: pAdapterCallbacks offset drift");
#if UINTPTR_MAX == 0xFFFFFFFFu
static_assert(sizeof(D3DDDIARG_OPENADAPTER) == 24, "D3DDDIARG_OPENADAPTER ABI mismatch: sizeof drift (x86)");
static_assert(sizeof(D3DDDIARG_OPENADAPTER2) == 24, "D3DDDIARG_OPENADAPTER2 ABI mismatch: sizeof drift (x86)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER, hAdapter) == 16,
              "D3DDDIARG_OPENADAPTER ABI mismatch: hAdapter offset drift (x86)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER, pAdapterFuncs) == 20,
              "D3DDDIARG_OPENADAPTER ABI mismatch: pAdapterFuncs offset drift (x86)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER2, hAdapter) == 16,
              "D3DDDIARG_OPENADAPTER2 ABI mismatch: hAdapter offset drift (x86)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER2, pAdapterFuncs) == 20,
              "D3DDDIARG_OPENADAPTER2 ABI mismatch: pAdapterFuncs offset drift (x86)");
#else
static_assert(sizeof(D3DDDIARG_OPENADAPTER) == 40, "D3DDDIARG_OPENADAPTER ABI mismatch: sizeof drift (x64)");
static_assert(sizeof(D3DDDIARG_OPENADAPTER2) == 40, "D3DDDIARG_OPENADAPTER2 ABI mismatch: sizeof drift (x64)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER, hAdapter) == 24,
              "D3DDDIARG_OPENADAPTER ABI mismatch: hAdapter offset drift (x64)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER, pAdapterFuncs) == 32,
              "D3DDDIARG_OPENADAPTER ABI mismatch: pAdapterFuncs offset drift (x64)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER2, hAdapter) == 24,
              "D3DDDIARG_OPENADAPTER2 ABI mismatch: hAdapter offset drift (x64)");
static_assert(offsetof(D3DDDIARG_OPENADAPTER2, pAdapterFuncs) == 32,
              "D3DDDIARG_OPENADAPTER2 ABI mismatch: pAdapterFuncs offset drift (x64)");
#endif

typedef struct _D3DDDIARG_OPENADAPTERFROMHDC {
  UINT Interface;
  UINT Version;
  HDC hDc;
  LUID AdapterLuid; // out (best effort)
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3DDDI_HADAPTER hAdapter; // out
  D3D9DDI_ADAPTERFUNCS* pAdapterFuncs; // out
} D3DDDIARG_OPENADAPTERFROMHDC;

typedef struct _D3DDDIARG_OPENADAPTERFROMLUID {
  UINT Interface;
  UINT Version;
  LUID AdapterLuid; // in
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3DDDI_HADAPTER hAdapter; // out
  D3D9DDI_ADAPTERFUNCS* pAdapterFuncs; // out
} D3DDDIARG_OPENADAPTERFROMLUID;

// ---- Adapter-level argument/dispatch structs ---------------------------------
typedef struct _D3D9DDIARG_GETCAPS {
  uint32_t Type;
  void* pData;
  uint32_t DataSize;
} D3D9DDIARG_GETCAPS;

typedef struct _D3D9DDIARG_QUERYADAPTERINFO {
  uint32_t Type;
  void* pPrivateDriverData;
  uint32_t PrivateDriverDataSize;
} D3D9DDIARG_QUERYADAPTERINFO;

typedef struct _D3D9DDIARG_CREATEDEVICE {
  D3DDDI_HADAPTER hAdapter;
  D3DDDI_HDEVICE hDevice; // out
  uint32_t Flags;
  const D3DDDI_DEVICECALLBACKS* pCallbacks; // runtime callbacks (WDDM submission)
} D3D9DDIARG_CREATEDEVICE;

typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CLOSEADAPTER)(D3DDDI_HADAPTER hAdapter);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETCAPS)(D3DDDI_HADAPTER hAdapter, const D3D9DDIARG_GETCAPS* pGetCaps);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATEDEVICE)(D3D9DDIARG_CREATEDEVICE* pCreateDevice, D3D9DDI_DEVICEFUNCS* pDeviceFuncs);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_QUERYADAPTERINFO)(D3DDDI_HADAPTER hAdapter, const D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo);

typedef struct _D3D9DDI_ADAPTERFUNCS {
  PFND3D9DDI_CLOSEADAPTER pfnCloseAdapter;
  PFND3D9DDI_GETCAPS pfnGetCaps;
  PFND3D9DDI_CREATEDEVICE pfnCreateDevice;
  PFND3D9DDI_QUERYADAPTERINFO pfnQueryAdapterInfo;
} D3D9DDI_ADAPTERFUNCS;

static_assert(offsetof(D3D9DDI_ADAPTERFUNCS, pfnCloseAdapter) == 0,
              "D3D9DDI_ADAPTERFUNCS ABI mismatch: pfnCloseAdapter offset drift");
static_assert(offsetof(D3D9DDI_ADAPTERFUNCS, pfnGetCaps) == sizeof(void*),
              "D3D9DDI_ADAPTERFUNCS ABI mismatch: pfnGetCaps offset drift");
static_assert(offsetof(D3D9DDI_ADAPTERFUNCS, pfnCreateDevice) == sizeof(void*) * 2,
              "D3D9DDI_ADAPTERFUNCS ABI mismatch: pfnCreateDevice offset drift");
static_assert(offsetof(D3D9DDI_ADAPTERFUNCS, pfnQueryAdapterInfo) == sizeof(void*) * 3,
              "D3D9DDI_ADAPTERFUNCS ABI mismatch: pfnQueryAdapterInfo offset drift");
#if UINTPTR_MAX == 0xFFFFFFFFu
static_assert(sizeof(D3D9DDI_ADAPTERFUNCS) == 16, "D3D9DDI_ADAPTERFUNCS ABI mismatch: sizeof drift (x86)");
#else
static_assert(sizeof(D3D9DDI_ADAPTERFUNCS) == 32, "D3D9DDI_ADAPTERFUNCS ABI mismatch: sizeof drift (x64)");
#endif

// -----------------------------------------------------------------------------
// Device-level argument structs (subset)
// -----------------------------------------------------------------------------

typedef struct _D3DDDI_SCANLINEORDERING {
  uint32_t Value;
} D3DDDI_SCANLINEORDERING;

typedef struct _D3DDDI_DISPLAYMODEEX {
  uint32_t Size;
  uint32_t Width;
  uint32_t Height;
  uint32_t RefreshRate;
  uint32_t Format; // D3DFORMAT numeric value
  uint32_t ScanLineOrdering;
} D3DDDI_DISPLAYMODEEX;

typedef enum _D3DDDI_ROTATION {
  D3DDDI_ROTATION_IDENTITY = 1,
  D3DDDI_ROTATION_90 = 2,
  D3DDDI_ROTATION_180 = 3,
  D3DDDI_ROTATION_270 = 4,
} D3DDDI_ROTATION;

typedef struct _D3D9DDI_PRESENT_PARAMETERS {
  uint32_t backbuffer_width;
  uint32_t backbuffer_height;
  uint32_t backbuffer_format;
  uint32_t backbuffer_count;
  uint32_t swap_effect;
  uint32_t flags;
  HWND hDeviceWindow;
  BOOL windowed;
  uint32_t presentation_interval;
} D3D9DDI_PRESENT_PARAMETERS;

typedef struct _D3D9DDIARG_CREATESWAPCHAIN {
  D3D9DDI_PRESENT_PARAMETERS present_params;
  D3D9DDI_HSWAPCHAIN hSwapChain; // out
  D3DDDI_HRESOURCE hBackBuffer;  // out (primary backbuffer)
} D3D9DDIARG_CREATESWAPCHAIN;

typedef struct _D3D9DDIARG_RESET {
  D3D9DDI_PRESENT_PARAMETERS present_params;
} D3D9DDIARG_RESET;

typedef struct _D3D9DDIARG_CREATERESOURCE {
  uint32_t type;   // driver-defined
  uint32_t format; // driver-defined (D3DFORMAT numeric)
  uint32_t width;
  uint32_t height;
  uint32_t depth;
  uint32_t mip_levels;
  uint32_t usage; // driver-defined (e.g. render target, dynamic)
  uint32_t pool;  // D3DPOOL numeric value
  uint32_t size;  // for buffers (bytes)
  D3DDDI_HRESOURCE hResource; // out

  HANDLE* pSharedHandle; // optional

  // Optional per-allocation private driver data blob (`aerogpu_wddm_alloc_priv` /
  // `aerogpu_wddm_alloc_priv_v2`).
  //
  // In real WDDM builds the D3D runtime provides this as a per-allocation buffer
  // passed through dxgkrnl to the KMD. AeroGPU uses it to carry stable IDs
  // across the UMD↔KMD boundary and (for shared resources) across processes:
  //
  // - The UMD supplies `alloc_id` (u32) and `flags` (including whether the
  //   allocation is shared).
  // - The KMD writes back `size_bytes` and, for shared allocations, a stable
  //   64-bit `share_token` in `aerogpu_wddm_alloc_priv.share_token` (see
  //   `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
  // - For shared allocations, dxgkrnl preserves the blob and returns the exact
  //   same bytes on cross-process opens, so both processes observe identical IDs.
  //
  // Do NOT derive `share_token` from the numeric value of the user-mode shared `HANDLE`:
  // for real NT handles it is process-local (commonly different after
  // `DuplicateHandle`), and some D3D9Ex stacks use token-style shared handles that
  // still must not be treated as a stable protocol key (and should not be passed
  // to `CloseHandle`).
  //
  // See also: drivers/aerogpu/protocol/aerogpu_wddm_alloc.h
  //
  // The "PrivateDriverData" naming matches WDK conventions; keep the legacy
  // "KmdAllocPrivateData" alias so repo-only callers can be explicit.
  union {
    void* pKmdAllocPrivateData;
    void* pPrivateDriverData;
  };
  union {
    uint32_t KmdAllocPrivateDataSize;
    uint32_t PrivateDriverDataSize;
  };

  uint32_t wddm_hAllocation; // optional
} D3D9DDIARG_CREATERESOURCE;

typedef struct _D3D9DDIARG_GETRENDERTARGETDATA {
  D3DDDI_HRESOURCE hSrcResource;
  D3DDDI_HRESOURCE hDstResource;
} D3D9DDIARG_GETRENDERTARGETDATA;

typedef struct _D3D9DDIARG_COPYRECTS {
  D3DDDI_HRESOURCE hSrcResource;
  D3DDDI_HRESOURCE hDstResource;
  const RECT* pSrcRects;
  uint32_t rect_count;
} D3D9DDIARG_COPYRECTS;

typedef struct _D3D9DDIARG_OPENRESOURCE {
  const void* pPrivateDriverData;
  uint32_t private_driver_data_size;
  uint32_t type;
  uint32_t format;
  uint32_t width;
  uint32_t height;
  uint32_t depth;
  uint32_t mip_levels;
  uint32_t usage;
  uint32_t size;
  D3DDDI_HRESOURCE hResource; // out

  // Optional WDDM allocation handle for this resource's backing store
  // (per-process). This is required to build the WDDM allocation list for
  // submissions when using `backing_alloc_id` references (no patch list).
  // 0 if not provided / not applicable in portable builds.
  uint32_t wddm_hAllocation;
} D3D9DDIARG_OPENRESOURCE;

typedef struct _D3D9DDIARG_LOCK {
  D3DDDI_HRESOURCE hResource;
  uint32_t offset_bytes;
  uint32_t size_bytes;
  uint32_t flags;
} D3D9DDIARG_LOCK;

typedef struct _D3D9DDIARG_UNLOCK {
  D3DDDI_HRESOURCE hResource;
  uint32_t offset_bytes;
  uint32_t size_bytes;
} D3D9DDIARG_UNLOCK;

typedef struct _D3D9DDIARG_PRESENT {
  D3DDDI_HRESOURCE hSrc;
  D3D9DDI_HSWAPCHAIN hSwapChain;
  HWND hWnd;
  uint32_t sync_interval;
  uint32_t flags;
} D3D9DDIARG_PRESENT;

typedef struct _D3D9DDIARG_PRESENTEX {
  D3DDDI_HRESOURCE hSrc;
  HWND hWnd;
  uint32_t sync_interval;
  uint32_t d3d9_present_flags;
} D3D9DDIARG_PRESENTEX;

typedef struct _D3D9DDI_PRESENTSTATS {
  uint32_t PresentCount;
  uint32_t PresentRefreshCount;
  uint32_t SyncRefreshCount;
  int64_t SyncQPCTime;
  int64_t SyncGPUTime;
} D3D9DDI_PRESENTSTATS;

typedef struct _D3D9DDIARG_CREATEQUERY {
  uint32_t type;
  D3D9DDI_HQUERY hQuery; // out
} D3D9DDIARG_CREATEQUERY;

typedef struct _D3D9DDIARG_ISSUEQUERY {
  D3D9DDI_HQUERY hQuery;
  uint32_t flags;
} D3D9DDIARG_ISSUEQUERY;

typedef struct _D3D9DDIARG_GETQUERYDATA {
  D3D9DDI_HQUERY hQuery;
  void* pData;
  uint32_t data_size;
  uint32_t flags;
} D3D9DDIARG_GETQUERYDATA;

// Draw*2 DDIs (DrawPrimitive2 / DrawIndexedPrimitive2), used by some runtimes
// for "UP" style draw paths.
typedef struct _D3DDDIARG_DRAWPRIMITIVE2 {
  D3DDDIPRIMITIVETYPE PrimitiveType;
  uint32_t PrimitiveCount;
  const void* pVertexStreamZeroData;
  uint32_t VertexStreamZeroStride;
} D3DDDIARG_DRAWPRIMITIVE2;

typedef struct _D3DDDIARG_DRAWINDEXEDPRIMITIVE2 {
  D3DDDIPRIMITIVETYPE PrimitiveType;
  uint32_t PrimitiveCount;
  uint32_t MinIndex;
  uint32_t NumVertices;
  const void* pIndexData;
  D3DDDIFORMAT IndexDataFormat;
  const void* pVertexStreamZeroData;
  uint32_t VertexStreamZeroStride;
} D3DDDIARG_DRAWINDEXEDPRIMITIVE2;

// Device::ProcessVertices emulation.
//
// The D3D9 runtime consumes the currently-bound stream sources as the vertex
// input and writes into `hDestBuffer`.
//
// Flags note:
// - `Flags` is passed through from `IDirect3DDevice9::ProcessVertices` (D3DPV_*
//   bits). AeroGPU currently observes `D3DPV_DONOTCOPYDATA` (`0x1`), meaning “do
//   not write non-position output elements”; the UMD preserves the destination
//   bytes for any non-position fields.
//
// Portable ABI note:
// - The Win7 WDK defines this struct in `d3dumddi.h`.
// - Some header vintages may not include `DestStride`. When `DestStride` is
//   absent (or is present but set to 0), the AeroGPU UMD attempts to infer the
//   effective destination stride from **stream 0** of `hVertexDecl` when
//   possible.
//   - The fixed-function CPU transform subset requires that this inference
//     succeeds (the driver must know where to write `POSITIONT`).
//   - The memcpy fallback path may fall back to the currently-bound stream 0
//     stride when the destination declaration is unavailable or does not allow
//     stride inference.
typedef struct _D3DDDIARG_PROCESSVERTICES {
  uint32_t SrcStartIndex;
  uint32_t DestIndex;
  uint32_t VertexCount;
  D3DDDI_HRESOURCE hDestBuffer;
  D3D9DDI_HVERTEXDECL hVertexDecl;
  uint32_t Flags;
  // Optional; some header vintages omit this field. When present, 0 means “infer
  // destination stride” (prefer stream 0 of the destination vertex decl).
  uint32_t DestStride;
} D3DDDIARG_PROCESSVERTICES;

typedef struct _D3D9DDIARG_GETDISPLAYMODEEX {
  uint32_t swapchain;
  D3DDDI_DISPLAYMODEEX* pMode; // optional
  D3DDDI_ROTATION* pRotation;  // optional
} D3D9DDIARG_GETDISPLAYMODEEX;

typedef struct _D3D9DDIARG_QUERYRESOURCERESIDENCY {
  const D3DDDI_HRESOURCE* pResources;
  uint32_t resource_count;
  uint32_t* pResidencyStatus;
} D3D9DDIARG_QUERYRESOURCERESIDENCY;

typedef struct _D3D9DDIARG_COMPOSERECTS {
  uint32_t reserved0;
  uint32_t reserved1;
} D3D9DDIARG_COMPOSERECTS;

typedef struct _D3D9DDIARG_BLT {
  D3DDDI_HRESOURCE hSrc;
  D3DDDI_HRESOURCE hDst;
  const RECT* pSrcRect;
  const RECT* pDstRect;
  uint32_t filter;
  uint32_t flags;
} D3D9DDIARG_BLT;

typedef struct _D3D9DDIARG_COLORFILL {
  D3DDDI_HRESOURCE hDst;
  const RECT* pRect;
  uint32_t color_argb;
  uint32_t flags;
} D3D9DDIARG_COLORFILL;

typedef struct _D3D9DDIARG_UPDATESURFACE {
  D3DDDI_HRESOURCE hSrc;
  const RECT* pSrcRect;
  D3DDDI_HRESOURCE hDst;
  union {
    const POINT* pDstPoint;
    const RECT* pDstRect;
  };
  uint32_t flags;
} D3D9DDIARG_UPDATESURFACE;

typedef struct _D3D9DDIARG_UPDATETEXTURE {
  D3DDDI_HRESOURCE hSrc;
  D3DDDI_HRESOURCE hDst;
  uint32_t flags;
} D3D9DDIARG_UPDATETEXTURE;

// -----------------------------------------------------------------------------
// Device function table (subset)
// -----------------------------------------------------------------------------

typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYDEVICE)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATERESOURCE)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_CREATERESOURCE* pCreateResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_OPENRESOURCE)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_OPENRESOURCE* pOpenResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_OPENRESOURCE2)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_OPENRESOURCE* pOpenResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYRESOURCE)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE hResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_LOCK)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_LOCK* pLock, D3DDDI_LOCKEDBOX* pLockedBox);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_UNLOCK)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_UNLOCK* pUnlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETRENDERTARGET)(D3DDDI_HDEVICE hDevice, uint32_t slot, D3DDDI_HRESOURCE hSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETDEPTHSTENCIL)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE hSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETVIEWPORT)(D3DDDI_HDEVICE hDevice, const D3DDDIVIEWPORTINFO* pViewport);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSCISSORRECT)(D3DDDI_HDEVICE hDevice, const RECT* pRect, BOOL enabled);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETTEXTURE)(D3DDDI_HDEVICE hDevice, uint32_t stage, D3DDDI_HRESOURCE hTexture);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETTEXTURESTAGESTATE)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETTEXTURESTAGESTATE)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t state, uint32_t* pValue);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSAMPLERSTATE)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETRENDERSTATE)(D3DDDI_HDEVICE hDevice, uint32_t state, uint32_t value);
// Fixed-function transform state (WORLD/VIEW/PROJECTION).
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETTRANSFORM)(D3DDDI_HDEVICE hDevice, D3DTRANSFORMSTATETYPE state, const D3DMATRIX* pMatrix);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_MULTIPLYTRANSFORM)(D3DDDI_HDEVICE hDevice, D3DTRANSFORMSTATETYPE state, const D3DMATRIX* pMatrix);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETTRANSFORM)(D3DDDI_HDEVICE hDevice, D3DTRANSFORMSTATETYPE state, D3DMATRIX* pMatrix);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATEVERTEXDECL)(D3DDDI_HDEVICE hDevice, const void* pDecl, uint32_t decl_size, D3D9DDI_HVERTEXDECL* phDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETVERTEXDECL)(D3DDDI_HDEVICE hDevice, D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYVERTEXDECL)(D3DDDI_HDEVICE hDevice, D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETFVF)(D3DDDI_HDEVICE hDevice, uint32_t fvf);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATESHADER)(D3DDDI_HDEVICE hDevice, uint32_t stage, const void* pBytecode, uint32_t bytecode_size, D3D9DDI_HSHADER* phShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADER)(D3DDDI_HDEVICE hDevice, uint32_t stage, D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYSHADER)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADERCONSTF)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t start_reg, const float* pData, uint32_t vec4_count);
// Optional shader integer/bool constant DDIs. Some WDK vintages expose these in the device function
// table; in portable mode we include them so host-side tests can exercise the paths.
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADERCONSTI)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t start_reg, const int32_t* pData, uint32_t vec4_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADERCONSTB)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t start_reg, const BOOL* pData, uint32_t bool_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSTREAMSOURCE)(D3DDDI_HDEVICE hDevice, uint32_t stream, D3DDDI_HRESOURCE hVb, uint32_t offset_bytes, uint32_t stride_bytes);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETINDICES)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE hIb, D3DDDIFORMAT fmt, uint32_t offset_bytes);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_BEGINSCENE)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_ENDSCENE)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CLEAR)(D3DDDI_HDEVICE hDevice, uint32_t flags, uint32_t color_rgba8, float depth, uint32_t stencil);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWPRIMITIVE)(D3DDDI_HDEVICE hDevice, D3DDDIPRIMITIVETYPE type, uint32_t start_vertex, uint32_t primitive_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWPRIMITIVEUP)(D3DDDI_HDEVICE hDevice, D3DDDIPRIMITIVETYPE type, uint32_t primitive_count, const void* pVertexData, uint32_t stride_bytes);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWINDEXEDPRIMITIVE)(D3DDDI_HDEVICE hDevice, D3DDDIPRIMITIVETYPE type, int32_t base_vertex, uint32_t min_index, uint32_t num_vertices, uint32_t start_index, uint32_t primitive_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWPRIMITIVE2)(D3DDDI_HDEVICE hDevice, const D3DDDIARG_DRAWPRIMITIVE2* pDraw);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWINDEXEDPRIMITIVE2)(D3DDDI_HDEVICE hDevice, const D3DDDIARG_DRAWINDEXEDPRIMITIVE2* pDraw);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_PROCESSVERTICES)(D3DDDI_HDEVICE hDevice, const D3DDDIARG_PROCESSVERTICES* pProcessVertices);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATESWAPCHAIN)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_CREATESWAPCHAIN* pCreateSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYSWAPCHAIN)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSWAPCHAIN hSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETSWAPCHAIN)(D3DDDI_HDEVICE hDevice, uint32_t index, D3D9DDI_HSWAPCHAIN* phSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSWAPCHAIN)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSWAPCHAIN hSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_RESET)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_RESET* pReset);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_RESETEX)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_RESET* pReset);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CHECKDEVICESTATE)(D3DDDI_HDEVICE hDevice, HWND hWnd);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_WAITFORVBLANK)(D3DDDI_HDEVICE hDevice, uint32_t swap_chain_index);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETGPUTHREADPRIORITY)(D3DDDI_HDEVICE hDevice, int32_t priority);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETGPUTHREADPRIORITY)(D3DDDI_HDEVICE hDevice, int32_t* pPriority);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CHECKRESOURCERESIDENCY)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE* pResources, uint32_t count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_QUERYRESOURCERESIDENCY)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_QUERYRESOURCERESIDENCY* pArgs);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETDISPLAYMODEEX)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_GETDISPLAYMODEEX* pGetModeEx);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_COMPOSERECTS)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_COMPOSERECTS* pComposeRects);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GENERATEMIPSUBLEVELS)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE hTexture);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_ROTATERESOURCEIDENTITIES)(D3DDDI_HDEVICE hDevice, D3DDDI_HRESOURCE* pResources, uint32_t resource_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_PRESENT)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_PRESENT* pPresent);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_PRESENTEX)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_PRESENTEX* pPresentEx);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_FLUSH)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETMAXFRAMELATENCY)(D3DDDI_HDEVICE hDevice, uint32_t max_frame_latency);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETMAXFRAMELATENCY)(D3DDDI_HDEVICE hDevice, uint32_t* pMaxFrameLatency);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETPRESENTSTATS)(D3DDDI_HDEVICE hDevice, D3D9DDI_PRESENTSTATS* pStats);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETLASTPRESENTCOUNT)(D3DDDI_HDEVICE hDevice, uint32_t* pLastPresentCount);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATEQUERY)(D3DDDI_HDEVICE hDevice, D3D9DDIARG_CREATEQUERY* pCreateQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYQUERY)(D3DDDI_HDEVICE hDevice, D3D9DDI_HQUERY hQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_ISSUEQUERY)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_ISSUEQUERY* pIssueQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETQUERYDATA)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_GETQUERYDATA* pGetQueryData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETRENDERTARGETDATA)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_GETRENDERTARGETDATA* pGetRenderTargetData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_COPYRECTS)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_COPYRECTS* pCopyRects);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_WAITFORIDLE)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_BLT)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_BLT* pBlt);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_COLORFILL)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_COLORFILL* pColorFill);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_UPDATESURFACE)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_UPDATESURFACE* pUpdateSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_UPDATETEXTURE)(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_UPDATETEXTURE* pUpdateTexture);

// D3D9 device cursor DDIs (subset).
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETCURSORPROPERTIES)(D3DDDI_HDEVICE hDevice,
                                                                  uint32_t x_hotspot,
                                                                  uint32_t y_hotspot,
                                                                  D3DDDI_HRESOURCE hCursorBitmap);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETCURSORPOSITION)(D3DDDI_HDEVICE hDevice,
                                                                int32_t x,
                                                                int32_t y,
                                                                uint32_t flags);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SHOWCURSOR)(D3DDDI_HDEVICE hDevice, BOOL bShow);

typedef struct _D3DDDIARG_DRAWRECTPATCH {
  UINT Handle;
  const float* pNumSegs; // float[4]
  const D3DRECTPATCH_INFO* pRectPatchInfo;
} D3DDDIARG_DRAWRECTPATCH;

typedef struct _D3DDDIARG_DRAWTRIPATCH {
  UINT Handle;
  const float* pNumSegs; // float[3]
  const D3DTRIPATCH_INFO* pTriPatchInfo;
} D3DDDIARG_DRAWTRIPATCH;

typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWRECTPATCH)(D3DDDI_HDEVICE hDevice, const D3DDDIARG_DRAWRECTPATCH* pDrawRectPatch);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DRAWTRIPATCH)(D3DDDI_HDEVICE hDevice, const D3DDDIARG_DRAWTRIPATCH* pDrawTriPatch);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DELETEPATCH)(D3DDDI_HDEVICE hDevice, UINT Handle);

// State blocks (Create/Capture/Apply + Begin/End record).
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATESTATEBLOCK)(D3DDDI_HDEVICE hDevice, uint32_t type_u32, D3D9DDI_HSTATEBLOCK* phStateBlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DELETESTATEBLOCK)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CAPTURESTATEBLOCK)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_APPLYSTATEBLOCK)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_BEGINSTATEBLOCK)(D3DDDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_ENDSTATEBLOCK)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK* phStateBlock);

struct _D3D9DDI_DEVICEFUNCS {
  PFND3D9DDI_DESTROYDEVICE pfnDestroyDevice;
  PFND3D9DDI_CREATERESOURCE pfnCreateResource;
  PFND3D9DDI_OPENRESOURCE pfnOpenResource;
  PFND3D9DDI_OPENRESOURCE2 pfnOpenResource2;
  PFND3D9DDI_DESTROYRESOURCE pfnDestroyResource;
  PFND3D9DDI_LOCK pfnLock;
  PFND3D9DDI_UNLOCK pfnUnlock;
  PFND3D9DDI_SETRENDERTARGET pfnSetRenderTarget;
  PFND3D9DDI_SETDEPTHSTENCIL pfnSetDepthStencil;
  PFND3D9DDI_SETVIEWPORT pfnSetViewport;
  PFND3D9DDI_SETSCISSORRECT pfnSetScissorRect;
  PFND3D9DDI_SETTEXTURE pfnSetTexture;
  PFND3D9DDI_SETSAMPLERSTATE pfnSetSamplerState;
  PFND3D9DDI_SETRENDERSTATE pfnSetRenderState;
  PFND3D9DDI_CREATEVERTEXDECL pfnCreateVertexDecl;
  PFND3D9DDI_SETVERTEXDECL pfnSetVertexDecl;
  PFND3D9DDI_DESTROYVERTEXDECL pfnDestroyVertexDecl;
  PFND3D9DDI_SETFVF pfnSetFVF;
  PFND3D9DDI_CREATESHADER pfnCreateShader;
  PFND3D9DDI_SETSHADER pfnSetShader;
  PFND3D9DDI_DESTROYSHADER pfnDestroyShader;
  PFND3D9DDI_SETSHADERCONSTF pfnSetShaderConstF;
  PFND3D9DDI_SETSTREAMSOURCE pfnSetStreamSource;
  PFND3D9DDI_SETINDICES pfnSetIndices;
  PFND3D9DDI_BEGINSCENE pfnBeginScene;
  PFND3D9DDI_ENDSCENE pfnEndScene;
  PFND3D9DDI_CREATESWAPCHAIN pfnCreateSwapChain;
  PFND3D9DDI_DESTROYSWAPCHAIN pfnDestroySwapChain;
  PFND3D9DDI_GETSWAPCHAIN pfnGetSwapChain;
  PFND3D9DDI_SETSWAPCHAIN pfnSetSwapChain;
  PFND3D9DDI_RESET pfnReset;
  PFND3D9DDI_RESETEX pfnResetEx;
  PFND3D9DDI_CHECKDEVICESTATE pfnCheckDeviceState;
  PFND3D9DDI_WAITFORVBLANK pfnWaitForVBlank;
  PFND3D9DDI_SETGPUTHREADPRIORITY pfnSetGPUThreadPriority;
  PFND3D9DDI_GETGPUTHREADPRIORITY pfnGetGPUThreadPriority;
  PFND3D9DDI_CHECKRESOURCERESIDENCY pfnCheckResourceResidency;
  PFND3D9DDI_QUERYRESOURCERESIDENCY pfnQueryResourceResidency;
  PFND3D9DDI_GETDISPLAYMODEEX pfnGetDisplayModeEx;
  PFND3D9DDI_COMPOSERECTS pfnComposeRects;
  PFND3D9DDI_ROTATERESOURCEIDENTITIES pfnRotateResourceIdentities;
  PFND3D9DDI_PRESENT pfnPresent;
  PFND3D9DDI_PRESENTEX pfnPresentEx;
  PFND3D9DDI_FLUSH pfnFlush;
  PFND3D9DDI_SETMAXFRAMELATENCY pfnSetMaximumFrameLatency;
  PFND3D9DDI_GETMAXFRAMELATENCY pfnGetMaximumFrameLatency;
  PFND3D9DDI_GETPRESENTSTATS pfnGetPresentStats;
  PFND3D9DDI_GETLASTPRESENTCOUNT pfnGetLastPresentCount;
  PFND3D9DDI_CREATEQUERY pfnCreateQuery;
  PFND3D9DDI_DESTROYQUERY pfnDestroyQuery;
  PFND3D9DDI_ISSUEQUERY pfnIssueQuery;
  PFND3D9DDI_GETQUERYDATA pfnGetQueryData;
  PFND3D9DDI_GETRENDERTARGETDATA pfnGetRenderTargetData;
  PFND3D9DDI_COPYRECTS pfnCopyRects;
  PFND3D9DDI_WAITFORIDLE pfnWaitForIdle;
  PFND3D9DDI_BLT pfnBlt;
  PFND3D9DDI_COLORFILL pfnColorFill;
  PFND3D9DDI_UPDATESURFACE pfnUpdateSurface;
  PFND3D9DDI_UPDATETEXTURE pfnUpdateTexture;

  // NOTE: The Win7 WDK D3D9DDI_DEVICEFUNCS table places the legacy draw/clear
  // entrypoints after the swapchain/present/control blocks. Keep these members
  // at the tail so the offsets for CreateSwapChain/Present/Flush/etc match the
  // WDK ABI.
  PFND3D9DDI_CLEAR pfnClear;
  PFND3D9DDI_DRAWPRIMITIVE pfnDrawPrimitive;
  PFND3D9DDI_DRAWPRIMITIVEUP pfnDrawPrimitiveUP;
  PFND3D9DDI_DRAWINDEXEDPRIMITIVE pfnDrawIndexedPrimitive;
  PFND3D9DDI_DRAWPRIMITIVE2 pfnDrawPrimitive2;
  PFND3D9DDI_DRAWINDEXEDPRIMITIVE2 pfnDrawIndexedPrimitive2;

  // Patch rendering / ProcessVertices.
  // Placed at the tail so existing portable ABI anchor offsets remain stable.
  PFND3D9DDI_DRAWRECTPATCH pfnDrawRectPatch;
  PFND3D9DDI_DRAWTRIPATCH pfnDrawTriPatch;
  PFND3D9DDI_DELETEPATCH pfnDeletePatch;
  PFND3D9DDI_PROCESSVERTICES pfnProcessVertices;

  // Optional D3D9Ex/DDI helper entrypoints (present in some WDK vintages and
  // relied on by apps that use D3DUSAGE_AUTOGENMIPMAP).
  PFND3D9DDI_GENERATEMIPSUBLEVELS pfnGenerateMipSubLevels;
  // Optional fixed-function/DDI entrypoints (present in WDK builds). These are
  // used by the UMD to keep a cache of D3DTSS_* stage state, and stage0 is
  // consumed by the minimal fixed-function fallback path for shader selection.
  PFND3D9DDI_SETTEXTURESTAGESTATE pfnSetTextureStageState;
  PFND3D9DDI_GETTEXTURESTAGESTATE pfnGetTextureStageState;

  // Legacy fixed-function transform entrypoints. These are part of the Win7 D3D9
  // UMD DDI, but are only included in the portable ABI when needed by host-side
  // tests.
  PFND3D9DDI_SETTRANSFORM pfnSetTransform;
  PFND3D9DDI_MULTIPLYTRANSFORM pfnMultiplyTransform;
  PFND3D9DDI_GETTRANSFORM pfnGetTransform;

  // State blocks (Create/Capture/Apply + Begin/End record).
  PFND3D9DDI_CREATESTATEBLOCK pfnCreateStateBlock;
  PFND3D9DDI_DELETESTATEBLOCK pfnDeleteStateBlock;
  PFND3D9DDI_CAPTURESTATEBLOCK pfnCaptureStateBlock;
  PFND3D9DDI_APPLYSTATEBLOCK pfnApplyStateBlock;
  PFND3D9DDI_BEGINSTATEBLOCK pfnBeginStateBlock;
  PFND3D9DDI_ENDSTATEBLOCK pfnEndStateBlock;

  // Cursor DDIs are appended to the tail in the portable ABI subset so existing
  // anchor offsets remain stable.
  PFND3D9DDI_SETCURSORPROPERTIES pfnSetCursorProperties;
  PFND3D9DDI_SETCURSORPOSITION pfnSetCursorPosition;
  PFND3D9DDI_SHOWCURSOR pfnShowCursor;

  // Optional shader integer/bool constant DDIs. These are not part of the Win7 D3D9DDI_DEVICEFUNCS
  // layout we anchor to, so keep them at the tail in portable builds.
  PFND3D9DDI_SETSHADERCONSTI pfnSetShaderConstI;
  PFND3D9DDI_SETSHADERCONSTB pfnSetShaderConstB;
};

// -----------------------------------------------------------------------------
// Portable ABI sanity checks (anchors)
// -----------------------------------------------------------------------------
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyDevice) == 0,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyDevice offset drift");
#if UINTPTR_MAX == 0xFFFFFFFFu
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateResource) == 4,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateResource offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnOpenResource) == 8,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnOpenResource offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnOpenResource2) == 12,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnOpenResource2 offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyResource) == 16,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyResource offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnLock) == 20,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnLock offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUnlock) == 24,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUnlock offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetRenderTarget) == 28,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetRenderTarget offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetDepthStencil) == 32,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetDepthStencil offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetViewport) == 36,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetViewport offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetScissorRect) == 40,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetScissorRect offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetTexture) == 44,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetTexture offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetSamplerState) == 48,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetSamplerState offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetRenderState) == 52,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetRenderState offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateVertexDecl) == 56,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateVertexDecl offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetVertexDecl) == 60,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetVertexDecl offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyVertexDecl) == 64,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyVertexDecl offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetFVF) == 68,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetFVF offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateShader) == 72,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateShader offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetShader) == 76,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetShader offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyShader) == 80,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyShader offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetShaderConstF) == 84,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetShaderConstF offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetStreamSource) == 88,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetStreamSource offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetIndices) == 92,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetIndices offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnBeginScene) == 96,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnBeginScene offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnEndScene) == 100,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnEndScene offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateSwapChain) == 104,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateSwapChain offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroySwapChain) == 108,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroySwapChain offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetSwapChain) == 112,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetSwapChain offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetSwapChain) == 116,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetSwapChain offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnReset) == 120,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnReset offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnResetEx) == 124,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnResetEx offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckDeviceState) == 128,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCheckDeviceState offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForVBlank) == 132,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnWaitForVBlank offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetGPUThreadPriority) == 136,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetGPUThreadPriority offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetGPUThreadPriority) == 140,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetGPUThreadPriority offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckResourceResidency) == 144,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCheckResourceResidency offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnQueryResourceResidency) == 148,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnQueryResourceResidency offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetDisplayModeEx) == 152,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetDisplayModeEx offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnComposeRects) == 156,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnComposeRects offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnRotateResourceIdentities) == 160,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnRotateResourceIdentities offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnPresent) == 164,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnPresent offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnPresentEx) == 168,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnPresentEx offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnFlush) == 172,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnFlush offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetMaximumFrameLatency) == 176,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetMaximumFrameLatency offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetMaximumFrameLatency) == 180,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetMaximumFrameLatency offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetPresentStats) == 184,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetPresentStats offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetLastPresentCount) == 188,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetLastPresentCount offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateQuery) == 192,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateQuery offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyQuery) == 196,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyQuery offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnIssueQuery) == 200,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnIssueQuery offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetQueryData) == 204,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetQueryData offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetRenderTargetData) == 208,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetRenderTargetData offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCopyRects) == 212,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCopyRects offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForIdle) == 216,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnWaitForIdle offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnBlt) == 220,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnBlt offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnColorFill) == 224,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnColorFill offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateSurface) == 228,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUpdateSurface offset drift (x86)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateTexture) == 232,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUpdateTexture offset drift (x86)");
#else
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateResource) == 8,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateResource offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnOpenResource) == 16,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnOpenResource offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnOpenResource2) == 24,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnOpenResource2 offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyResource) == 32,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyResource offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnLock) == 40,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnLock offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUnlock) == 48,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUnlock offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetRenderTarget) == 56,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetRenderTarget offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetDepthStencil) == 64,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetDepthStencil offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetViewport) == 72,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetViewport offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetScissorRect) == 80,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetScissorRect offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetTexture) == 88,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetTexture offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetSamplerState) == 96,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetSamplerState offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetRenderState) == 104,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetRenderState offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateVertexDecl) == 112,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateVertexDecl offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetVertexDecl) == 120,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetVertexDecl offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyVertexDecl) == 128,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyVertexDecl offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetFVF) == 136,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetFVF offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateShader) == 144,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateShader offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetShader) == 152,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetShader offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyShader) == 160,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyShader offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetShaderConstF) == 168,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetShaderConstF offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetStreamSource) == 176,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetStreamSource offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetIndices) == 184,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetIndices offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnBeginScene) == 192,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnBeginScene offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnEndScene) == 200,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnEndScene offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateSwapChain) == 208,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateSwapChain offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroySwapChain) == 216,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroySwapChain offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetSwapChain) == 224,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetSwapChain offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetSwapChain) == 232,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetSwapChain offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnReset) == 240,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnReset offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnResetEx) == 248,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnResetEx offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckDeviceState) == 256,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCheckDeviceState offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForVBlank) == 264,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnWaitForVBlank offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetGPUThreadPriority) == 272,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetGPUThreadPriority offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetGPUThreadPriority) == 280,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetGPUThreadPriority offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckResourceResidency) == 288,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCheckResourceResidency offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnQueryResourceResidency) == 296,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnQueryResourceResidency offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetDisplayModeEx) == 304,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetDisplayModeEx offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnComposeRects) == 312,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnComposeRects offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnRotateResourceIdentities) == 320,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnRotateResourceIdentities offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnPresent) == 328,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnPresent offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnPresentEx) == 336,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnPresentEx offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnFlush) == 344,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnFlush offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetMaximumFrameLatency) == 352,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnSetMaximumFrameLatency offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetMaximumFrameLatency) == 360,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetMaximumFrameLatency offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetPresentStats) == 368,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetPresentStats offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetLastPresentCount) == 376,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetLastPresentCount offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateQuery) == 384,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCreateQuery offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyQuery) == 392,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnDestroyQuery offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnIssueQuery) == 400,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnIssueQuery offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetQueryData) == 408,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetQueryData offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetRenderTargetData) == 416,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnGetRenderTargetData offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnCopyRects) == 424,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnCopyRects offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForIdle) == 432,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnWaitForIdle offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnBlt) == 440,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnBlt offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnColorFill) == 448,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnColorFill offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateSurface) == 456,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUpdateSurface offset drift (x64)");
static_assert(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateTexture) == 464,
              "D3D9DDI_DEVICEFUNCS ABI mismatch: pfnUpdateTexture offset drift (x64)");
#endif

#endif // portable ABI subset

// -----------------------------------------------------------------------------
// UMD entrypoints
// -----------------------------------------------------------------------------

// Win7 D3D9 runtime entrypoints: open an adapter and return the adapter vtable.
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter(D3DDDIARG_OPENADAPTER* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter2(D3DDDIARG_OPENADAPTER2* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromHdc(D3DDDIARG_OPENADAPTERFROMHDC* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromLuid(D3DDDIARG_OPENADAPTERFROMLUID* pOpenAdapter);
