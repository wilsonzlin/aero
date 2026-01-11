// AeroGPU D3D9Ex user-mode display driver (UMD) - public entrypoints.
//
// This is a clean-room implementation intended for Windows 7 SP1 (WDDM 1.1).
// The real build uses WDK headers for the D3D9 DDI; for repository builds that
// don't have the WDK available, we provide a tiny "compat" surface with just
// enough types to keep the code self-contained.
//
// The goal of this header is not to perfectly mirror the WDK; it exists so the
// command-stream translation code is readable and testable in isolation.
// When integrating into an actual Win7 WDK build, define
//   AEROGPU_D3D9_USE_WDK_DDI
// and include the real WDK D3D9 DDI headers before this file.
//
#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
#else
typedef void* HANDLE;
typedef void* HWND;
typedef void* HDC;
typedef uint32_t DWORD;
typedef uint32_t UINT;
typedef int32_t HRESULT;
typedef uint8_t BYTE;
typedef int32_t BOOL;
typedef struct _RECT {
  long left;
  long top;
  long right;
  long bottom;
} RECT;

  #ifndef TRUE
    #define TRUE 1
  #endif
  #ifndef FALSE
    #define FALSE 0
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

  // Common D3D9 HRESULTs used by D3D9Ex GetData/CreateQuery paths.
  #ifndef D3DERR_NOTAVAILABLE
    #define D3DERR_NOTAVAILABLE ((HRESULT)0x8876086AL)
  #endif
  #ifndef D3DERR_WASSTILLDRAWING
    #define D3DERR_WASSTILLDRAWING ((HRESULT)0x8876021CL)
  #endif
#endif

#ifndef D3DERR_NOTAVAILABLE
  #define D3DERR_NOTAVAILABLE ((HRESULT)0x8876086AL)
#endif
#ifndef D3DERR_WASSTILLDRAWING
  #define D3DERR_WASSTILLDRAWING ((HRESULT)0x8876021CL)
#endif

#if defined(_WIN32)
  #define AEROGPU_D3D9_CALL __stdcall
  #define AEROGPU_D3D9_EXPORT extern "C" __declspec(dllexport)
#else
  #define AEROGPU_D3D9_CALL
  #define AEROGPU_D3D9_EXPORT extern "C"
#endif

// -----------------------------------------------------------------------------
// Minimal D3D9 DDI surface (compat only)
// -----------------------------------------------------------------------------
// Real driver builds should use WDK types (d3d9umddi.h et al). These are a
// minimal subset used by the translation layer.

typedef void* AEROGPU_D3D9DDI_HADAPTER;
typedef void* AEROGPU_D3D9DDI_HDEVICE;
typedef void* AEROGPU_D3D9DDI_HRESOURCE;
typedef void* AEROGPU_D3D9DDI_HSHADER;
typedef void* AEROGPU_D3D9DDI_HVERTEXDECL;
typedef void* AEROGPU_D3D9DDI_HQUERY;

typedef enum AEROGPU_D3D9DDI_SHADER_STAGE {
  AEROGPU_D3D9DDI_SHADER_STAGE_VS = 0,
  AEROGPU_D3D9DDI_SHADER_STAGE_PS = 1,
} AEROGPU_D3D9DDI_SHADER_STAGE;

typedef enum AEROGPU_D3D9DDI_PRIMITIVE_TYPE {
  AEROGPU_D3D9DDI_PRIM_POINTLIST = 1,
  AEROGPU_D3D9DDI_PRIM_LINELIST = 2,
  AEROGPU_D3D9DDI_PRIM_LINESTRIP = 3,
  AEROGPU_D3D9DDI_PRIM_TRIANGLELIST = 4,
  AEROGPU_D3D9DDI_PRIM_TRIANGLESTRIP = 5,
  AEROGPU_D3D9DDI_PRIM_TRIANGLEFAN = 6,
} AEROGPU_D3D9DDI_PRIMITIVE_TYPE;

typedef enum AEROGPU_D3D9DDI_INDEX_FORMAT {
  AEROGPU_D3D9DDI_INDEX_FORMAT_U16 = 0,
  AEROGPU_D3D9DDI_INDEX_FORMAT_U32 = 1,
} AEROGPU_D3D9DDI_INDEX_FORMAT;

typedef struct AEROGPU_D3D9DDI_VIEWPORT {
  float x;
  float y;
  float w;
  float h;
  float min_z;
  float max_z;
} AEROGPU_D3D9DDI_VIEWPORT;

typedef struct AEROGPU_D3D9DDI_LOCKED_BOX {
  void* pData;
  uint32_t rowPitch;
  uint32_t slicePitch;
} AEROGPU_D3D9DDI_LOCKED_BOX;

typedef struct AEROGPU_D3D9DDIARG_OPENADAPTER {
  uint32_t interface_version;
  AEROGPU_D3D9DDI_HADAPTER hAdapter; // out: driver-owned handle
  HDC hDc;                           // optional (may be NULL)
} AEROGPU_D3D9DDIARG_OPENADAPTER;

typedef struct AEROGPU_D3D9DDIARG_CREATEDEVICE {
  AEROGPU_D3D9DDI_HADAPTER hAdapter;
  AEROGPU_D3D9DDI_HDEVICE hDevice; // out: driver-owned handle
  uint32_t flags;
  // WDDM builds provide a D3DDDI_DEVICECALLBACKS pointer here. In compat builds
  // this remains NULL and the driver uses an in-process submission stub.
  const void* pCallbacks;
} AEROGPU_D3D9DDIARG_CREATEDEVICE;

typedef struct AEROGPU_D3D9DDIARG_CREATERESOURCE {
  uint32_t type;     // driver-defined
  uint32_t format;   // driver-defined
  uint32_t width;
  uint32_t height;
  uint32_t depth;
  uint32_t mip_levels;
  uint32_t usage;    // driver-defined (e.g. render target, dynamic)
  uint32_t size;     // for buffers (bytes)
  AEROGPU_D3D9DDI_HRESOURCE hResource; // out

  // Optional shared handle pointer.
  //
  // D3D9Ex semantics (mirrors CreateTexture/CreateRenderTarget, etc):
  // - pSharedHandle == NULL: not a shared resource
  // - pSharedHandle != NULL and *pSharedHandle == NULL: create a new shared resource
  // - pSharedHandle != NULL and *pSharedHandle != NULL: open an existing shared resource
  HANDLE* pSharedHandle;

  // Optional per-allocation private driver data blob (aerogpu_wddm_alloc_priv).
  //
  // In real WDDM builds this is the allocation private driver data that the UMD
  // attaches to allocations. For shared resources, dxgkrnl preserves it and
  // returns it to other processes on OpenResource/OpenAllocation.
  const void* pKmdAllocPrivateData;
  uint32_t KmdAllocPrivateDataSize;
} AEROGPU_D3D9DDIARG_CREATERESOURCE;

typedef struct AEROGPU_D3D9DDIARG_LOCK {
  AEROGPU_D3D9DDI_HRESOURCE hResource;
  uint32_t offset_bytes;
  uint32_t size_bytes;
  uint32_t flags;
} AEROGPU_D3D9DDIARG_LOCK;

typedef struct AEROGPU_D3D9DDIARG_UNLOCK {
  AEROGPU_D3D9DDI_HRESOURCE hResource;
  uint32_t offset_bytes;
  uint32_t size_bytes;
} AEROGPU_D3D9DDIARG_UNLOCK;

typedef struct AEROGPU_D3D9DDIARG_PRESENT {
  AEROGPU_D3D9DDI_HRESOURCE hSrc;
  HWND hWnd;
  uint32_t sync_interval; // 0 or 1
  uint32_t flags;
} AEROGPU_D3D9DDIARG_PRESENT;

// D3D9Ex-style present (mirrors IDirect3DDevice9Ex::PresentEx inputs).
typedef struct AEROGPU_D3D9DDIARG_PRESENTEX {
  AEROGPU_D3D9DDI_HRESOURCE hSrc;
  HWND hWnd;
  uint32_t sync_interval; // 0 or 1
  uint32_t d3d9_present_flags; // raw D3DPRESENT_* dwFlags
} AEROGPU_D3D9DDIARG_PRESENTEX;

// D3D9Ex present statistics (subset of D3DPRESENTSTATS).
typedef struct AEROGPU_D3D9DDI_PRESENTSTATS {
  uint32_t PresentCount;
  uint32_t PresentRefreshCount;
  uint32_t SyncRefreshCount;
  int64_t SyncQPCTime;
  int64_t SyncGPUTime;
} AEROGPU_D3D9DDI_PRESENTSTATS;

typedef struct AEROGPU_D3D9DDIARG_CREATEQUERY {
  uint32_t type; // driver-defined (event query used by D3D9 runtime)
  AEROGPU_D3D9DDI_HQUERY hQuery; // out
} AEROGPU_D3D9DDIARG_CREATEQUERY;

typedef struct AEROGPU_D3D9DDIARG_ISSUEQUERY {
  AEROGPU_D3D9DDI_HQUERY hQuery;
  uint32_t flags;
} AEROGPU_D3D9DDIARG_ISSUEQUERY;

typedef struct AEROGPU_D3D9DDIARG_GETQUERYDATA {
  AEROGPU_D3D9DDI_HQUERY hQuery;
  void* pData;
  uint32_t data_size;
  uint32_t flags;
} AEROGPU_D3D9DDIARG_GETQUERYDATA;

typedef struct AEROGPU_D3D9DDI_ADAPTERFUNCS AEROGPU_D3D9DDI_ADAPTERFUNCS;
typedef struct AEROGPU_D3D9DDI_DEVICEFUNCS AEROGPU_D3D9DDI_DEVICEFUNCS;

typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CLOSEADAPTER)(
    AEROGPU_D3D9DDI_HADAPTER hAdapter);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETCAPS)(
    AEROGPU_D3D9DDI_HADAPTER hAdapter, void* pCaps, uint32_t caps_size);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATEDEVICE)(
    AEROGPU_D3D9DDIARG_CREATEDEVICE* pCreateDevice, AEROGPU_D3D9DDI_DEVICEFUNCS* pDeviceFuncs);

struct AEROGPU_D3D9DDI_ADAPTERFUNCS {
  PFN_AEROGPU_D3D9DDI_CLOSEADAPTER pfnCloseAdapter;
  PFN_AEROGPU_D3D9DDI_GETCAPS pfnGetCaps;
  PFN_AEROGPU_D3D9DDI_CREATEDEVICE pfnCreateDevice;
};

typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYDEVICE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATERESOURCE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDIARG_CREATERESOURCE* pCreateResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYRESOURCE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HRESOURCE hResource);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_LOCK)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_LOCK* pLock, AEROGPU_D3D9DDI_LOCKED_BOX* pLockedBox);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_UNLOCK)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_UNLOCK* pUnlock);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETRENDERTARGET)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t slot, AEROGPU_D3D9DDI_HRESOURCE hSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETDEPTHSTENCIL)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HRESOURCE hSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETVIEWPORT)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDI_VIEWPORT* pViewport);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSCISSORRECT)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const RECT* pRect, BOOL enabled);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETTEXTURE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t stage, AEROGPU_D3D9DDI_HRESOURCE hTexture);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSAMPLERSTATE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t stage, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETRENDERSTATE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATEVERTEXDECL)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const void* pDecl, uint32_t decl_size, AEROGPU_D3D9DDI_HVERTEXDECL* phDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETVERTEXDECL)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYVERTEXDECL)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATESHADER)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_SHADER_STAGE stage, const void* pBytecode, uint32_t bytecode_size, AEROGPU_D3D9DDI_HSHADER* phShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSHADER)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_SHADER_STAGE stage, AEROGPU_D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYSHADER)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSHADERCONSTF)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_SHADER_STAGE stage, uint32_t start_reg, const float* pData, uint32_t vec4_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSTREAMSOURCE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t stream, AEROGPU_D3D9DDI_HRESOURCE hVb, uint32_t offset_bytes, uint32_t stride_bytes);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETINDICES)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HRESOURCE hIb, AEROGPU_D3D9DDI_INDEX_FORMAT fmt, uint32_t offset_bytes);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CLEAR)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t flags, uint32_t color_rgba8, float depth, uint32_t stencil);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DRAWPRIMITIVE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_PRIMITIVE_TYPE type, uint32_t start_vertex, uint32_t primitive_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DRAWINDEXEDPRIMITIVE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_PRIMITIVE_TYPE type, int32_t base_vertex, uint32_t min_index, uint32_t num_vertices, uint32_t start_index, uint32_t primitive_count);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_PRESENT)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_PRESENT* pPresent);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_PRESENTEX)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_PRESENTEX* pPresentEx);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_FLUSH)(
    AEROGPU_D3D9DDI_HDEVICE hDevice);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETMAXFRAMELATENCY)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t max_frame_latency);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETMAXFRAMELATENCY)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t* pMaxFrameLatency);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETPRESENTSTATS)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_PRESENTSTATS* pStats);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETLASTPRESENTCOUNT)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t* pLastPresentCount);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATEQUERY)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDIARG_CREATEQUERY* pCreateQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYQUERY)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HQUERY hQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_ISSUEQUERY)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_ISSUEQUERY* pIssueQuery);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETQUERYDATA)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_GETQUERYDATA* pGetQueryData);

struct AEROGPU_D3D9DDI_DEVICEFUNCS {
  PFN_AEROGPU_D3D9DDI_DESTROYDEVICE pfnDestroyDevice;
  PFN_AEROGPU_D3D9DDI_CREATERESOURCE pfnCreateResource;
  PFN_AEROGPU_D3D9DDI_DESTROYRESOURCE pfnDestroyResource;
  PFN_AEROGPU_D3D9DDI_LOCK pfnLock;
  PFN_AEROGPU_D3D9DDI_UNLOCK pfnUnlock;

  PFN_AEROGPU_D3D9DDI_SETRENDERTARGET pfnSetRenderTarget;
  PFN_AEROGPU_D3D9DDI_SETDEPTHSTENCIL pfnSetDepthStencil;
  PFN_AEROGPU_D3D9DDI_SETVIEWPORT pfnSetViewport;
  PFN_AEROGPU_D3D9DDI_SETSCISSORRECT pfnSetScissorRect;
  PFN_AEROGPU_D3D9DDI_SETTEXTURE pfnSetTexture;
  PFN_AEROGPU_D3D9DDI_SETSAMPLERSTATE pfnSetSamplerState;
  PFN_AEROGPU_D3D9DDI_SETRENDERSTATE pfnSetRenderState;

  PFN_AEROGPU_D3D9DDI_CREATEVERTEXDECL pfnCreateVertexDecl;
  PFN_AEROGPU_D3D9DDI_SETVERTEXDECL pfnSetVertexDecl;
  PFN_AEROGPU_D3D9DDI_DESTROYVERTEXDECL pfnDestroyVertexDecl;

  PFN_AEROGPU_D3D9DDI_CREATESHADER pfnCreateShader;
  PFN_AEROGPU_D3D9DDI_SETSHADER pfnSetShader;
  PFN_AEROGPU_D3D9DDI_DESTROYSHADER pfnDestroyShader;
  PFN_AEROGPU_D3D9DDI_SETSHADERCONSTF pfnSetShaderConstF;

  PFN_AEROGPU_D3D9DDI_SETSTREAMSOURCE pfnSetStreamSource;
  PFN_AEROGPU_D3D9DDI_SETINDICES pfnSetIndices;

  PFN_AEROGPU_D3D9DDI_CLEAR pfnClear;
  PFN_AEROGPU_D3D9DDI_DRAWPRIMITIVE pfnDrawPrimitive;
  PFN_AEROGPU_D3D9DDI_DRAWINDEXEDPRIMITIVE pfnDrawIndexedPrimitive;
  PFN_AEROGPU_D3D9DDI_PRESENT pfnPresent;
  PFN_AEROGPU_D3D9DDI_PRESENTEX pfnPresentEx;
  PFN_AEROGPU_D3D9DDI_FLUSH pfnFlush;
  PFN_AEROGPU_D3D9DDI_SETMAXFRAMELATENCY pfnSetMaximumFrameLatency;
  PFN_AEROGPU_D3D9DDI_GETMAXFRAMELATENCY pfnGetMaximumFrameLatency;
  PFN_AEROGPU_D3D9DDI_GETPRESENTSTATS pfnGetPresentStats;
  PFN_AEROGPU_D3D9DDI_GETLASTPRESENTCOUNT pfnGetLastPresentCount;

  PFN_AEROGPU_D3D9DDI_CREATEQUERY pfnCreateQuery;
  PFN_AEROGPU_D3D9DDI_DESTROYQUERY pfnDestroyQuery;
  PFN_AEROGPU_D3D9DDI_ISSUEQUERY pfnIssueQuery;
  PFN_AEROGPU_D3D9DDI_GETQUERYDATA pfnGetQueryData;
};

// -----------------------------------------------------------------------------
// UMD entrypoints
// -----------------------------------------------------------------------------

// Win7 D3D9 runtime entrypoint: open an adapter and return the adapter vtable.
// When built against the real WDK DDI, the signature and structures should be
// updated to match the WDK exactly. The exported name is the key contract.
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter(
    AEROGPU_D3D9DDIARG_OPENADAPTER* pOpenAdapter,
    AEROGPU_D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);

// Some runtimes call OpenAdapter2 on WDDM 1.1+ for version negotiation.
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter2(
    AEROGPU_D3D9DDIARG_OPENADAPTER* pOpenAdapter,
    AEROGPU_D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);
