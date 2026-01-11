// AeroGPU D3D9Ex user-mode display driver (UMD) - public entrypoints.
//
// This is a clean-room implementation intended for Windows 7 SP1 (WDDM 1.1).
//
// The canonical in-tree build is performed via MSBuild/WDK10 (see `drivers\aerogpu\aerogpu.sln`).
// When `AEROGPU_D3D9_USE_WDK_DDI` is defined, this header pulls in the official
// D3D9 UMD DDI headers (`d3d9umddi.h`, `d3dumddi.h`) from a Win7-capable WDK and
// the rest of the code should use those types directly.
//
// For repository/portable builds (no WDK headers available), we provide a tiny
// subset of the Win7 D3D9UMDDI ABI. It is intentionally incomplete; it exists so
// the codebase can be built and iterated on without requiring the WDK.
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
typedef int32_t LONG;
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
  #ifndef SUCCEEDED
    #define SUCCEEDED(hr) (((HRESULT)(hr)) >= 0)
  #endif
  #ifndef FAILED
    #define FAILED(hr) (((HRESULT)(hr)) < 0)
  #endif
  #ifndef AEROGPU_LUID_DEFINED
    #define AEROGPU_LUID_DEFINED
typedef struct _LUID {
  DWORD LowPart;
  LONG HighPart;
} LUID;
  #endif
#endif

// Common D3D9 HRESULTs used by D3D9Ex GetData/CreateQuery paths.
#ifndef D3DERR_NOTAVAILABLE
  #define D3DERR_NOTAVAILABLE ((HRESULT)0x8876086AL)
#endif
#ifndef D3DERR_WASSTILLDRAWING
  #define D3DERR_WASSTILLDRAWING ((HRESULT)0x8876021CL)
#endif

// HRESULT helpers (normally provided by Windows headers).
#ifndef SUCCEEDED
  #define SUCCEEDED(hr) (((HRESULT)(hr)) >= 0)
#endif
#ifndef FAILED
  #define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

#if defined(_WIN32)
  #define AEROGPU_D3D9_CALL __stdcall
  #define AEROGPU_D3D9_EXPORT extern "C" __declspec(dllexport)
#else
  #define AEROGPU_D3D9_CALL
  #define AEROGPU_D3D9_EXPORT extern "C"
#endif

// -----------------------------------------------------------------------------
// D3D9UMDDI ABI surface
// -----------------------------------------------------------------------------
// In WDK mode (`AEROGPU_D3D9_USE_WDK_DDI`), pull in the official headers.
//
// In portable mode, define a minimal subset of the Win7 ABI so the UMD sources
// can be compiled without the WDK.

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  #include <d3dumddi.h>
  #include <d3d9umddi.h>
#endif

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI))

// ---- Minimal WDDM handle shims ------------------------------------------------
// D3D9UMDDI handle types are opaque driver-private pointers. The WDK represents
// them as tiny wrapper structs with a single `pDrvPrivate` field; we mirror that
// layout so call sites can be written once and compiled both with and without
// the WDK headers.

typedef struct _D3D9DDI_HADAPTER {
  void* pDrvPrivate;
} D3D9DDI_HADAPTER;

typedef struct _D3D9DDI_HDEVICE {
  void* pDrvPrivate;
} D3D9DDI_HDEVICE;

typedef struct _D3D9DDI_HRESOURCE {
  void* pDrvPrivate;
} D3D9DDI_HRESOURCE;

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

// ---- Adapter open ABI ---------------------------------------------------------
typedef struct _D3D9DDIARG_OPENADAPTER {
  UINT Interface;
  UINT Version;
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3D9DDI_HADAPTER hAdapter; // out
} D3D9DDIARG_OPENADAPTER;

typedef struct _D3D9DDIARG_OPENADAPTER2 {
  UINT Interface;
  UINT Version;
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3D9DDI_HADAPTER hAdapter; // out
} D3D9DDIARG_OPENADAPTER2;

typedef struct _D3D9DDIARG_OPENADAPTERFROMHDC {
  UINT Interface;
  UINT Version;
  HDC hDc;
  LUID AdapterLuid; // out (best effort)
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3D9DDI_HADAPTER hAdapter; // out
} D3D9DDIARG_OPENADAPTERFROMHDC;

typedef struct _D3D9DDIARG_OPENADAPTERFROMLUID {
  UINT Interface;
  UINT Version;
  LUID AdapterLuid; // in
  D3DDDI_ADAPTERCALLBACKS* pAdapterCallbacks;
  D3DDDI_ADAPTERCALLBACKS2* pAdapterCallbacks2;
  D3D9DDI_HADAPTER hAdapter; // out
} D3D9DDIARG_OPENADAPTERFROMLUID;

// ---- Adapter vtable ABI (minimal) --------------------------------------------
typedef struct _D3D9DDIARG_GETCAPS {
  UINT Type;
  void* pData;
  UINT DataSize;
} D3D9DDIARG_GETCAPS;

typedef struct _D3D9DDIARG_CREATEDEVICE {
  D3D9DDI_HADAPTER hAdapter;
  D3D9DDI_HDEVICE hDevice; // out
  UINT Flags;
} D3D9DDIARG_CREATEDEVICE;

typedef struct _D3D9DDIARG_QUERYADAPTERINFO {
  UINT Type;
  void* pData;
  UINT DataSize;
} D3D9DDIARG_QUERYADAPTERINFO;

// Note: For portable builds, we alias the D3D9DDI device vtable name to the
// AeroGPU-private subset so we can keep call sites uniform.
typedef struct AEROGPU_D3D9DDI_DEVICEFUNCS D3D9DDI_DEVICEFUNCS;

typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CLOSEADAPTER)(D3D9DDI_HADAPTER hAdapter);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_GETCAPS)(D3D9DDI_HADAPTER hAdapter, const D3D9DDIARG_GETCAPS* pGetCaps);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_CREATEDEVICE)(
    D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    D3D9DDI_DEVICEFUNCS* pDeviceFuncs);
typedef HRESULT(AEROGPU_D3D9_CALL* PFND3D9DDI_QUERYADAPTERINFO)(
    D3D9DDI_HADAPTER hAdapter,
    const D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo);

typedef struct _D3D9DDI_ADAPTERFUNCS {
  PFND3D9DDI_CLOSEADAPTER pfnCloseAdapter;
  PFND3D9DDI_GETCAPS pfnGetCaps;
  PFND3D9DDI_CREATEDEVICE pfnCreateDevice;
  PFND3D9DDI_QUERYADAPTERINFO pfnQueryAdapterInfo;
} D3D9DDI_ADAPTERFUNCS;

#endif // !(_WIN32 && AEROGPU_D3D9_USE_WDK_DDI)

// -----------------------------------------------------------------------------
// AeroGPU private D3D9 DDI surface (translation layer)
// -----------------------------------------------------------------------------
// These are internal, portable-only definitions used by the command-stream
// translation layer. They intentionally do not match the full WDK ABI.

typedef D3D9DDI_HADAPTER AEROGPU_D3D9DDI_HADAPTER;
typedef D3D9DDI_HDEVICE AEROGPU_D3D9DDI_HDEVICE;
typedef D3D9DDI_HRESOURCE AEROGPU_D3D9DDI_HRESOURCE;
typedef D3D9DDI_HSWAPCHAIN AEROGPU_D3D9DDI_HSWAPCHAIN;
typedef D3D9DDI_HSHADER AEROGPU_D3D9DDI_HSHADER;
typedef D3D9DDI_HVERTEXDECL AEROGPU_D3D9DDI_HVERTEXDECL;
typedef D3D9DDI_HQUERY AEROGPU_D3D9DDI_HQUERY;

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

typedef D3D9DDIARG_OPENADAPTER AEROGPU_D3D9DDIARG_OPENADAPTER;
typedef D3D9DDIARG_OPENADAPTER2 AEROGPU_D3D9DDIARG_OPENADAPTER2;
typedef D3D9DDIARG_OPENADAPTERFROMHDC AEROGPU_D3D9DDIARG_OPENADAPTERFROMHDC;
typedef D3D9DDIARG_OPENADAPTERFROMLUID AEROGPU_D3D9DDIARG_OPENADAPTERFROMLUID;

// GetCaps / QueryAdapterInfo are adapter-level queries.
// These argument layouts are intended to match the Windows 7-era WDK D3D9 UMD DDI.
// The runtime selects the payload format based on `type`.
typedef struct AEROGPU_D3D9DDIARG_GETCAPS {
  uint32_t type; // D3DDDICAPS_TYPE / D3D9DDICAPS_TYPE (WDK)
  void* pData;
  uint32_t data_size;
} AEROGPU_D3D9DDIARG_GETCAPS;

typedef struct AEROGPU_D3D9DDIARG_QUERYADAPTERINFO {
  uint32_t type; // D3DDDIQUERYADAPTERINFO_* / D3D9QUERYADAPTERINFO_* (WDK)
  void* pPrivateDriverData;
  uint32_t private_driver_data_size;
} AEROGPU_D3D9DDIARG_QUERYADAPTERINFO;

typedef struct AEROGPU_D3D9DDIARG_CREATEDEVICE {
  AEROGPU_D3D9DDI_HADAPTER hAdapter;
  AEROGPU_D3D9DDI_HDEVICE hDevice; // out: driver-owned handle
  uint32_t flags;
  // WDDM builds provide a D3DDDI_DEVICECALLBACKS pointer here. In compat builds
  // this remains NULL and the driver uses an in-process submission stub.
  const void* pCallbacks;
} AEROGPU_D3D9DDIARG_CREATEDEVICE;

typedef struct AEROGPU_D3D9DDI_PRESENT_PARAMETERS {
  uint32_t backbuffer_width;
  uint32_t backbuffer_height;
  uint32_t backbuffer_format;
  uint32_t backbuffer_count;
  uint32_t swap_effect;
  uint32_t flags;
  HWND hDeviceWindow;
  BOOL windowed;
  uint32_t presentation_interval;
} AEROGPU_D3D9DDI_PRESENT_PARAMETERS;

typedef struct AEROGPU_D3D9DDIARG_CREATESWAPCHAIN {
  AEROGPU_D3D9DDI_PRESENT_PARAMETERS present_params;
  AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain; // out
  AEROGPU_D3D9DDI_HRESOURCE hBackBuffer; // out (primary backbuffer)
} AEROGPU_D3D9DDIARG_CREATESWAPCHAIN;

typedef struct AEROGPU_D3D9DDIARG_RESET {
  AEROGPU_D3D9DDI_PRESENT_PARAMETERS present_params;
} AEROGPU_D3D9DDIARG_RESET;

typedef struct AEROGPU_D3D9DDIARG_CREATERESOURCE {
  uint32_t type;     // driver-defined
  uint32_t format;   // driver-defined
  uint32_t width;
  uint32_t height;
  uint32_t depth;
  uint32_t mip_levels;
  uint32_t usage;    // driver-defined (e.g. render target, dynamic)
  // D3DPOOL numeric value (D3DPOOL_DEFAULT/MANAGED/SYSTEMMEM/SCRATCH).
  //
  // The Win7 D3D9 runtime uses this to request system-memory surfaces for
  // GetRenderTargetData readback (via CreateOffscreenPlainSurface with
  // D3DPOOL_SYSTEMMEM).
  uint32_t pool;
  uint32_t size;     // for buffers (bytes)
  AEROGPU_D3D9DDI_HRESOURCE hResource; // out

  // Optional shared handle pointer.
  //
  // D3D9Ex semantics (mirrors CreateTexture/CreateRenderTarget, etc):
  // - pSharedHandle == NULL: not a shared resource
  // - pSharedHandle != NULL and *pSharedHandle == NULL: create a new shared resource
  // - pSharedHandle != NULL and *pSharedHandle != NULL: open an existing shared resource
  HANDLE* pSharedHandle;

  // Optional per-allocation private driver data blob (`aerogpu_wddm_alloc_priv`).
  //
  // In real WDDM builds the D3D runtime provides this as a per-allocation buffer
  // whose contents are persisted by dxgkrnl for shared allocations.
  //
  // AeroGPU treats this as **UMD → KMD input**: the UMD writes
  // `aerogpu_wddm_alloc_priv` (alloc_id/share_token/size) at creation time.
  // dxgkrnl preserves the bytes for shared allocations and replays them when the
  // resource is opened in another process, allowing both processes to observe
  // identical IDs.
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
} AEROGPU_D3D9DDIARG_CREATERESOURCE;

typedef struct AEROGPU_D3D9DDIARG_GETRENDERTARGETDATA {
  AEROGPU_D3D9DDI_HRESOURCE hSrcResource;
  AEROGPU_D3D9DDI_HRESOURCE hDstResource;
} AEROGPU_D3D9DDIARG_GETRENDERTARGETDATA;

typedef struct AEROGPU_D3D9DDIARG_COPYRECTS {
  AEROGPU_D3D9DDI_HRESOURCE hSrcResource;
  AEROGPU_D3D9DDI_HRESOURCE hDstResource;
  // Optional rect list. If NULL/0, treat as full-surface copy.
  const RECT* pSrcRects;
  uint32_t rect_count;
} AEROGPU_D3D9DDIARG_COPYRECTS;

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
  AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain;
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

typedef D3D9DDI_ADAPTERFUNCS AEROGPU_D3D9DDI_ADAPTERFUNCS;
// -----------------------------------------------------------------------------
// Compositor-critical 2D operations (StretchRect/Blt, ColorFill, Update*)
// -----------------------------------------------------------------------------

// Minimal D3D9 StretchRect/Blt argument subset.
typedef struct AEROGPU_D3D9DDIARG_BLT {
  AEROGPU_D3D9DDI_HRESOURCE hSrc;
  AEROGPU_D3D9DDI_HRESOURCE hDst;
  const RECT* pSrcRect; // NULL == full resource
  const RECT* pDstRect; // NULL == full resource
  uint32_t filter;      // D3DTEXTUREFILTERTYPE numeric (POINT/LINEAR supported)
  uint32_t flags;       // reserved (0)
} AEROGPU_D3D9DDIARG_BLT;

typedef struct AEROGPU_D3D9DDIARG_COLORFILL {
  AEROGPU_D3D9DDI_HRESOURCE hDst;
  const RECT* pRect;     // NULL == full resource
  uint32_t color_argb;   // D3DCOLOR (0xAARRGGBB)
  uint32_t flags;        // reserved (0)
} AEROGPU_D3D9DDIARG_COLORFILL;

typedef struct AEROGPU_D3D9DDIARG_UPDATESURFACE {
  AEROGPU_D3D9DDI_HRESOURCE hSrc;
  const RECT* pSrcRect; // NULL == full source
  AEROGPU_D3D9DDI_HRESOURCE hDst;
  const RECT* pDstRect; // NULL == full destination
  uint32_t flags;       // reserved (0)
} AEROGPU_D3D9DDIARG_UPDATESURFACE;

typedef struct AEROGPU_D3D9DDIARG_UPDATETEXTURE {
  AEROGPU_D3D9DDI_HRESOURCE hSrc;
  AEROGPU_D3D9DDI_HRESOURCE hDst;
  uint32_t flags; // reserved (0)
} AEROGPU_D3D9DDIARG_UPDATETEXTURE;
typedef struct AEROGPU_D3D9DDI_DEVICEFUNCS AEROGPU_D3D9DDI_DEVICEFUNCS;

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
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CREATESWAPCHAIN)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDIARG_CREATESWAPCHAIN* pCreateSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_DESTROYSWAPCHAIN)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETSWAPCHAIN)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t index, AEROGPU_D3D9DDI_HSWAPCHAIN* phSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_SETSWAPCHAIN)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_RESET)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_RESET* pReset);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_RESETEX)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_RESET* pReset);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_CHECKDEVICESTATE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, HWND hWnd);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_ROTATERESOURCEIDENTITIES)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, AEROGPU_D3D9DDI_HRESOURCE* pResources, uint32_t resource_count);
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
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_GETRENDERTARGETDATA)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_GETRENDERTARGETDATA* pGetRenderTargetData);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_COPYRECTS)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_COPYRECTS* pCopyRects);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_WAITFORIDLE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice);

typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_BLT)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_BLT* pBlt);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_COLORFILL)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_COLORFILL* pColorFill);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_UPDATESURFACE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_UPDATESURFACE* pUpdateSurface);
typedef HRESULT(AEROGPU_D3D9_CALL* PFN_AEROGPU_D3D9DDI_UPDATETEXTURE)(
    AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_UPDATETEXTURE* pUpdateTexture);

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
  PFN_AEROGPU_D3D9DDI_CREATESWAPCHAIN pfnCreateSwapChain;
  PFN_AEROGPU_D3D9DDI_DESTROYSWAPCHAIN pfnDestroySwapChain;
  PFN_AEROGPU_D3D9DDI_GETSWAPCHAIN pfnGetSwapChain;
  PFN_AEROGPU_D3D9DDI_SETSWAPCHAIN pfnSetSwapChain;
  PFN_AEROGPU_D3D9DDI_RESET pfnReset;
  PFN_AEROGPU_D3D9DDI_RESETEX pfnResetEx;
  PFN_AEROGPU_D3D9DDI_CHECKDEVICESTATE pfnCheckDeviceState;
  PFN_AEROGPU_D3D9DDI_ROTATERESOURCEIDENTITIES pfnRotateResourceIdentities;
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

  // Readback / copy helpers used by GetRenderTargetData and related operations.
  PFN_AEROGPU_D3D9DDI_GETRENDERTARGETDATA pfnGetRenderTargetData;
  PFN_AEROGPU_D3D9DDI_COPYRECTS pfnCopyRects;
  PFN_AEROGPU_D3D9DDI_WAITFORIDLE pfnWaitForIdle;

  // 2D compositor helpers.
  PFN_AEROGPU_D3D9DDI_BLT pfnBlt;
  PFN_AEROGPU_D3D9DDI_COLORFILL pfnColorFill;
  PFN_AEROGPU_D3D9DDI_UPDATESURFACE pfnUpdateSurface;
  PFN_AEROGPU_D3D9DDI_UPDATETEXTURE pfnUpdateTexture;
};

// -----------------------------------------------------------------------------
// UMD entrypoints
// -----------------------------------------------------------------------------

// Win7 D3D9 runtime entrypoints: open an adapter and return the adapter vtable.
//
// These signatures match the Win7 D3D9UMDDI prototypes. In portable mode they
// compile against the minimal ABI shims above.
AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter(
    D3D9DDIARG_OPENADAPTER* pOpenAdapter,
    D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);

AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapter2(
    D3D9DDIARG_OPENADAPTER2* pOpenAdapter,
    D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);

AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromHdc(
    D3D9DDIARG_OPENADAPTERFROMHDC* pOpenAdapter,
    D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);

AEROGPU_D3D9_EXPORT HRESULT AEROGPU_D3D9_CALL OpenAdapterFromLuid(
    D3D9DDIARG_OPENADAPTERFROMLUID* pOpenAdapter,
    D3D9DDI_ADAPTERFUNCS* pAdapterFuncs);
