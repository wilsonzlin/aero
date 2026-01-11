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
  #if defined(AEROGPU_D3D9_USE_WDK_DDI)
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

// D3DCAPS2_* (from d3d9caps.h).
#ifndef D3DCAPS2_CANRENDERWINDOWED
  #define D3DCAPS2_CANRENDERWINDOWED 0x00080000u
#endif
#ifndef D3DCAPS2_CANSHARERESOURCE
  #define D3DCAPS2_CANSHARERESOURCE 0x00100000u
#endif

// D3DPRASTERCAPS_* (from d3d9caps.h).
#ifndef D3DPRASTERCAPS_SCISSORTEST
  #define D3DPRASTERCAPS_SCISSORTEST 0x00001000u
#endif

// D3DPTFILTERCAPS_* (from d3d9caps.h).
#ifndef D3DPTFILTERCAPS_MINFPOINT
  #define D3DPTFILTERCAPS_MINFPOINT 0x00000100u
#endif
#ifndef D3DPTFILTERCAPS_MINFLINEAR
  #define D3DPTFILTERCAPS_MINFLINEAR 0x00000200u
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

// D3DPTEXTURECAPS_* (subset).
#ifndef D3DPTEXTURECAPS_POW2
  #define D3DPTEXTURECAPS_POW2 0x00000002u
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
  void* pPrivateDriverData;      // in
  UINT PrivateDriverDataSize;    // in
} D3DDDIARG_CREATECONTEXT;

typedef struct _D3DDDIARG_DESTROYCONTEXT {
  D3DKMT_HANDLE hContext;
} D3DDDIARG_DESTROYCONTEXT;

typedef struct _D3DDDIARG_DESTROYSYNCHRONIZATIONOBJECT {
  D3DKMT_HANDLE hSyncObject;
} D3DDDIARG_DESTROYSYNCHRONIZATIONOBJECT;

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
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_RENDER)(D3DDDICB_RENDER* pData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3DDDICB_PRESENT)(D3DDDICB_PRESENT* pData);

typedef struct _D3DDDI_DEVICECALLBACKS {
  // Device/context lifecycle.
  PFND3DDDICB_CREATEDEVICE pfnCreateDeviceCb;
  PFND3DDDICB_DESTROYDEVICE pfnDestroyDeviceCb;
  PFND3DDDICB_CREATECONTEXT2 pfnCreateContextCb2;
  PFND3DDDICB_CREATECONTEXT pfnCreateContextCb;
  PFND3DDDICB_DESTROYCONTEXT pfnDestroyContextCb;
  PFND3DDDICB_DESTROYSYNCOBJECT pfnDestroySynchronizationObjectCb;

  // DMA buffer management. (Not currently used by the D3D9 UMD; we submit using
  // the runtime-provided "current" DMA buffer pointers returned by CreateContext
  // and/or rotated through in/out submit structs.)
  void* pfnAllocateCb;
  void* pfnDeallocateCb;
  void* pfnGetCommandBufferCb;

  // Submission callbacks.
  PFND3DDDICB_RENDER pfnRenderCb;
  PFND3DDDICB_PRESENT pfnPresentCb;

  // Optional sync/lock/error helpers (not used by AeroGPU D3D9; reserved for ABI).
  void* pfnWaitForSynchronizationObjectCb;
  void* pfnLockCb;
  void* pfnUnlockCb;
  void* pfnSetErrorCb;
} D3DDDI_DEVICECALLBACKS;

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

  // Optional per-allocation private driver data blob (`aerogpu_wddm_alloc_priv`).
  //
  // In real WDDM builds the D3D runtime provides this as a per-allocation buffer
  // which is treated as INPUT (UMD -> dxgkrnl -> KMD). For shared allocations,
  // dxgkrnl preserves these bytes and returns them verbatim when another process
  // opens the resource (OpenResource/OpenAllocation).
  //
  // AeroGPU uses this buffer to persist a UMD-owned `aerogpu_wddm_alloc_priv`
  // (primarily `alloc_id` + size) for shared resources: the UMD writes the blob
  // during create, and dxgkrnl preserves/returns the bytes verbatim when another
  // process opens the resource.
  //
  // NOTE: The protocol `share_token` used by `EXPORT_SHARED_SURFACE` /
  // `IMPORT_SHARED_SURFACE` is persisted in `aerogpu_wddm_alloc_priv.share_token`
  // (see `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`). Do not derive it from
  // the numeric D3D shared `HANDLE` value (process-local).
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
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSAMPLERSTATE)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETRENDERSTATE)(D3DDDI_HDEVICE hDevice, uint32_t state, uint32_t value);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATEVERTEXDECL)(D3DDDI_HDEVICE hDevice, const void* pDecl, uint32_t decl_size, D3D9DDI_HVERTEXDECL* phDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETVERTEXDECL)(D3DDDI_HDEVICE hDevice, D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYVERTEXDECL)(D3DDDI_HDEVICE hDevice, D3D9DDI_HVERTEXDECL hDecl);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETFVF)(D3DDDI_HDEVICE hDevice, uint32_t fvf);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATESHADER)(D3DDDI_HDEVICE hDevice, uint32_t stage, const void* pBytecode, uint32_t bytecode_size, D3D9DDI_HSHADER* phShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADER)(D3DDDI_HDEVICE hDevice, uint32_t stage, D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_DESTROYSHADER)(D3DDDI_HDEVICE hDevice, D3D9DDI_HSHADER hShader);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_SETSHADERCONSTF)(D3DDDI_HDEVICE hDevice, uint32_t stage, uint32_t start_reg, const float* pData, uint32_t vec4_count);
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
  PFND3D9DDI_CLEAR pfnClear;
  PFND3D9DDI_DRAWPRIMITIVE pfnDrawPrimitive;
  PFND3D9DDI_DRAWPRIMITIVEUP pfnDrawPrimitiveUP;
  PFND3D9DDI_DRAWINDEXEDPRIMITIVE pfnDrawIndexedPrimitive;
  PFND3D9DDI_DRAWPRIMITIVE2 pfnDrawPrimitive2;
  PFND3D9DDI_DRAWINDEXEDPRIMITIVE2 pfnDrawIndexedPrimitive2;
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
};

#endif // portable ABI subset

// -----------------------------------------------------------------------------
// UMD entrypoints
// -----------------------------------------------------------------------------

// Win7 D3D9 runtime entrypoints: open an adapter and return the adapter vtable.
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter(D3DDDIARG_OPENADAPTER* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter2(D3DDDIARG_OPENADAPTER2* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromHdc(D3DDDIARG_OPENADAPTERFROMHDC* pOpenAdapter);
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromLuid(D3DDDIARG_OPENADAPTERFROMLUID* pOpenAdapter);
