// AeroGPU D3D10/11 Windows 7 UMD - shared internal declarations.
//
// This driver is expected to be built as a DLL and loaded by the D3D10/D3D11
// runtime on Windows 7 SP1.
//
// The real implementation should be built with the Windows SDK/WDK which
// provides the official D3D10/11 DDI headers. For repository portability and to
// keep this directory self-contained, this header contains a minimal subset of
// the D3D10/11 DDI ABI required for the initial triangle milestone.
//
// When integrating with a real WDK build, define `AEROGPU_UMD_USE_WDK_HEADERS=1`
// (MSBuild: `/p:AeroGpuUseWdkHeaders=1`) to include the official headers instead
// of the local ABI subset.

#pragma once

#include <stdint.h>
#include <stddef.h>

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
  #define AEROGPU_APIENTRY __stdcall
  #define AEROGPU_UMD_EXPORT __declspec(dllexport)
#else
  typedef int32_t HRESULT;
  typedef uint32_t UINT;
  typedef size_t SIZE_T;
  #define AEROGPU_APIENTRY
  #define AEROGPU_UMD_EXPORT
  #ifndef S_OK
    #define S_OK ((HRESULT)0)
  #endif
  #ifndef E_FAIL
    #define E_FAIL ((HRESULT)0x80004005L)
  #endif
  #ifndef E_NOTIMPL
    #define E_NOTIMPL ((HRESULT)0x80004001L)
  #endif
  #ifndef E_NOINTERFACE
    #define E_NOINTERFACE ((HRESULT)0x80004002L)
  #endif
  #ifndef E_INVALIDARG
    #define E_INVALIDARG ((HRESULT)0x80070057L)
  #endif
  #ifndef E_OUTOFMEMORY
    #define E_OUTOFMEMORY ((HRESULT)0x8007000EL)
  #endif
  #ifndef SUCCEEDED
    #define SUCCEEDED(hr) (((HRESULT)(hr)) >= 0)
  #endif
  #ifndef FAILED
    #define FAILED(hr) (((HRESULT)(hr)) < 0)
  #endif
#endif

// DXGI_ERROR_WAS_STILL_DRAWING (from dxgi.h). This header is used in
// configurations that may not include dxgi.h directly.
#ifndef DXGI_ERROR_WAS_STILL_DRAWING
  #define DXGI_ERROR_WAS_STILL_DRAWING ((HRESULT)0x887A000AL)
#endif

// -------------------------------------------------------------------------------------------------
// Minimal D3D10/11 DDI ABI subset (Win7 milestone)
// -------------------------------------------------------------------------------------------------

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // The canonical build should use the official headers.
  //
  // These D3D*UMDDI headers ship with the Windows Driver Kit (WDK) / Windows Kits.
  // If you see this error, install the WDK and ensure the Windows Kits include
  // directories are on the compiler include path (Visual Studio/Build Tools
  // normally configures this automatically).
  #if defined(__has_include)
    #if !__has_include(<d3d10umddi.h>) || !__has_include(<d3d10_1umddi.h>) || !__has_include(<d3d11umddi.h>) || !__has_include(<d3dumddi.h>)
      #error "AEROGPU_UMD_USE_WDK_HEADERS=1 but required D3D DDI headers were not found. Install the Windows Driver Kit (WDK) (Windows Kits) so d3d10umddi.h/d3d11umddi.h are available."
    #endif
  #endif
  #include <d3d10umddi.h>
  #include <d3d10_1umddi.h>
  #include <d3d11umddi.h>
  #include <d3dumddi.h>
#else

// "Runtime" handle types (opaque to the driver).
typedef struct D3D10DDI_HRTADAPTER {
  void *pDrvPrivate;
} D3D10DDI_HRTADAPTER;

// "Driver" handle types (private pointer owned by the driver).
typedef struct D3D10DDI_HADAPTER {
  void *pDrvPrivate;
} D3D10DDI_HADAPTER;

typedef struct D3D10DDI_HDEVICE {
  void *pDrvPrivate;
} D3D10DDI_HDEVICE;

typedef struct D3D10DDI_HRESOURCE {
  void *pDrvPrivate;
} D3D10DDI_HRESOURCE;

typedef struct D3D10DDI_HSHADER {
  void *pDrvPrivate;
} D3D10DDI_HSHADER;

typedef struct D3D10DDI_HELEMENTLAYOUT {
  void *pDrvPrivate;
} D3D10DDI_HELEMENTLAYOUT;

typedef struct D3D10DDI_HRENDERTARGETVIEW {
  void *pDrvPrivate;
} D3D10DDI_HRENDERTARGETVIEW;

typedef struct D3D10DDI_HDEPTHSTENCILVIEW {
  void *pDrvPrivate;
} D3D10DDI_HDEPTHSTENCILVIEW;

typedef struct D3D10DDI_HSHADERRESOURCEVIEW {
  void *pDrvPrivate;
} D3D10DDI_HSHADERRESOURCEVIEW;

typedef struct D3D10DDI_HSAMPLER {
  void *pDrvPrivate;
} D3D10DDI_HSAMPLER;

typedef struct D3D10DDI_HBLENDSTATE {
  void *pDrvPrivate;
} D3D10DDI_HBLENDSTATE;

typedef struct D3D10DDI_HRASTERIZERSTATE {
  void *pDrvPrivate;
} D3D10DDI_HRASTERIZERSTATE;

typedef struct D3D10DDI_HDEPTHSTENCILSTATE {
  void *pDrvPrivate;
} D3D10DDI_HDEPTHSTENCILSTATE;

// Adapter open/create ABI.
typedef struct D3D10DDIARG_OPENADAPTER D3D10DDIARG_OPENADAPTER;
typedef struct D3D10DDIARG_CREATEDEVICE D3D10DDIARG_CREATEDEVICE;
typedef struct D3D10DDIARG_GETCAPS D3D10DDIARG_GETCAPS;
typedef struct D3D11DDIARG_GETCAPS D3D11DDIARG_GETCAPS;
typedef struct D3DDDI_DEVICECALLBACKS D3DDDI_DEVICECALLBACKS;

// Adapter caps querying (D3D10DDIARG_GETCAPS).
//
// The real WDK header uses D3D10DDI_GETCAPS_TYPE for Type; for the compat ABI we
// treat it as a UINT and rely on tracing to discover which values are requested.
struct D3D10DDIARG_GETCAPS {
  UINT Type;
  void* pData;
  UINT DataSize;
};

typedef HRESULT(AEROGPU_APIENTRY *PFND3D10DDI_GETCAPS)(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS*);
typedef SIZE_T(AEROGPU_APIENTRY *PFND3D10DDI_CALCPRIVATEDEVICESIZE)(D3D10DDI_HADAPTER,
                                                                     const D3D10DDIARG_CREATEDEVICE *);
typedef HRESULT(AEROGPU_APIENTRY *PFND3D10DDI_CREATEDEVICE)(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE *);
typedef void(AEROGPU_APIENTRY *PFND3D10DDI_CLOSEADAPTER)(D3D10DDI_HADAPTER);

typedef struct D3D10DDI_ADAPTERFUNCS {
  PFND3D10DDI_GETCAPS pfnGetCaps;
  PFND3D10DDI_CALCPRIVATEDEVICESIZE pfnCalcPrivateDeviceSize;
  PFND3D10DDI_CREATEDEVICE pfnCreateDevice;
  PFND3D10DDI_CLOSEADAPTER pfnCloseAdapter;
} D3D10DDI_ADAPTERFUNCS;

// D3D11 adapter caps entrypoint is required by the Win7 D3D11 runtime. The real
// WDK type uses `D3D11DDIARG_GETCAPS`; this local subset keeps the same layout
// but only models the fields required for bring-up.
typedef HRESULT(AEROGPU_APIENTRY *PFND3D11DDI_GETCAPS)(D3D10DDI_HADAPTER, const D3D11DDIARG_GETCAPS *);

struct D3D11DDIARG_GETCAPS {
  UINT Type;
  void* pData;
  UINT DataSize;
};

typedef struct D3D11DDI_ADAPTERFUNCS {
  PFND3D11DDI_GETCAPS pfnGetCaps;
  PFND3D10DDI_CALCPRIVATEDEVICESIZE pfnCalcPrivateDeviceSize;
  PFND3D10DDI_CREATEDEVICE pfnCreateDevice;
  PFND3D10DDI_CLOSEADAPTER pfnCloseAdapter;
} D3D11DDI_ADAPTERFUNCS;

struct D3D10DDIARG_OPENADAPTER {
  UINT Interface;
  UINT Version;
  D3D10DDI_HRTADAPTER hRTAdapter;
  D3D10DDI_HADAPTER hAdapter;
  D3D10DDI_ADAPTERFUNCS *pAdapterFuncs;
};

// Device ABI subset.
typedef struct AEROGPU_D3D10_11_DEVICEFUNCS AEROGPU_D3D10_11_DEVICEFUNCS;
typedef struct AEROGPU_D3D10_11_DEVICECALLBACKS AEROGPU_D3D10_11_DEVICECALLBACKS;

struct D3D10DDIARG_CREATEDEVICE {
  D3D10DDI_HDEVICE hDevice;
  const D3DDDI_DEVICECALLBACKS *pCallbacks;
  AEROGPU_D3D10_11_DEVICEFUNCS *pDeviceFuncs;
  // Optional callback table supplied by the harness/real runtime.
  //
  // In a real WDDM UMD this would be the D3D10/11 runtime callback table
  // (including submission + allocation management entrypoints). For repository
  // builds we keep it as a narrow AeroGPU-specific hook.
  const AEROGPU_D3D10_11_DEVICECALLBACKS *pDeviceCallbacks;
};
#endif // !_WIN32 || !AEROGPU_UMD_USE_WDK_HEADERS

// -------------------------------------------------------------------------------------------------
// Minimal D3D10/11 DDI ABI subset (Win7 milestone)
// -------------------------------------------------------------------------------------------------
//
// Even when building against the real WDK DDI headers, we keep these internal
// "AEROGPU_*" structures so that the translation layer can be compiled and
// unit-tested without needing to mirror the full WDK surface.

// Resource/shader descriptors (minimal).
typedef enum AEROGPU_DDI_RESOURCE_DIMENSION {
  AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER = 1,
  AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D = 3,
} AEROGPU_DDI_RESOURCE_DIMENSION;

typedef struct AEROGPU_DDI_SUBRESOURCE_DATA {
  const void *pSysMem;
  uint32_t SysMemPitch;
  uint32_t SysMemSlicePitch;
} AEROGPU_DDI_SUBRESOURCE_DATA;

typedef struct AEROGPU_DDIARG_CREATERESOURCE {
  AEROGPU_DDI_RESOURCE_DIMENSION Dimension;

  uint32_t BindFlags;
  uint32_t MiscFlags;

  // D3D10_USAGE / D3D11_USAGE numeric value.
  uint32_t Usage;

  // D3D10_CPU_ACCESS_FLAG / D3D11_CPU_ACCESS_FLAG numeric value.
  uint32_t CPUAccessFlags;

  // Buffer
  uint32_t ByteWidth;
  uint32_t StructureByteStride;

  // Texture2D
  uint32_t Width;
  uint32_t Height;
  uint32_t MipLevels;
  uint32_t ArraySize;
  uint32_t Format; // DXGI_FORMAT numeric value

  const AEROGPU_DDI_SUBRESOURCE_DATA *pInitialData;
  uint32_t InitialDataCount;

  // Additional fields present in the real D3D10/11 UMD DDIs that affect
  // allocation decisions. These are currently only used for tracing (the
  // repository build does not implement the full WDDM allocation contract).
  //
  // NOTE: These are intentionally appended to avoid changing offsets of the
  // fields already consumed by the bring-up implementation.
  uint32_t SampleDescCount;
  uint32_t SampleDescQuality;
  uint32_t ResourceFlags;
} AEROGPU_DDIARG_CREATERESOURCE;

// -------------------------------------------------------------------------------------------------
// D3D11 Map/Unmap ABI subset (portable build)
// -------------------------------------------------------------------------------------------------

// D3D11_USAGE values (numeric values from d3d11.h).
enum AEROGPU_D3D11_USAGE : uint32_t {
  AEROGPU_D3D11_USAGE_DEFAULT = 0,
  AEROGPU_D3D11_USAGE_IMMUTABLE = 1,
  AEROGPU_D3D11_USAGE_DYNAMIC = 2,
  AEROGPU_D3D11_USAGE_STAGING = 3,
};

// D3D11_CPU_ACCESS_* flags (numeric values from d3d11.h).
enum AEROGPU_D3D11_CPU_ACCESS_FLAG : uint32_t {
  AEROGPU_D3D11_CPU_ACCESS_WRITE = 0x10000,
  AEROGPU_D3D11_CPU_ACCESS_READ = 0x20000,
};

// D3D11_MAP values (numeric values from d3d11.h).
enum AEROGPU_D3D11_MAP : uint32_t {
  AEROGPU_D3D11_MAP_READ = 1,
  AEROGPU_D3D11_MAP_WRITE = 2,
  AEROGPU_D3D11_MAP_READ_WRITE = 3,
  AEROGPU_D3D11_MAP_WRITE_DISCARD = 4,
  AEROGPU_D3D11_MAP_WRITE_NO_OVERWRITE = 5,
};

// D3D11_MAP_FLAG_DO_NOT_WAIT (numeric value from d3d11.h).
constexpr uint32_t AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT = 0x100000;

typedef struct AEROGPU_D3D11_MAPPED_SUBRESOURCE {
  void *pData;
  uint32_t RowPitch;
  uint32_t DepthPitch;
} AEROGPU_D3D11_MAPPED_SUBRESOURCE;

typedef struct AEROGPU_D3D11DDIARG_MAP {
  D3D10DDI_HRESOURCE hResource;
  uint32_t Subresource;
  uint32_t MapType;   // AEROGPU_D3D11_MAP
  uint32_t MapFlags;  // AEROGPU_D3D11_MAP_FLAG_*
  AEROGPU_D3D11_MAPPED_SUBRESOURCE *pMappedSubresource;
} AEROGPU_D3D11DDIARG_MAP;

typedef struct AEROGPU_D3D11DDIARG_UNMAP {
  D3D10DDI_HRESOURCE hResource;
  uint32_t Subresource;
} AEROGPU_D3D11DDIARG_UNMAP;

typedef struct AEROGPU_DDIARG_CREATESHADER {
  const void *pCode;
  uint32_t CodeSize;
} AEROGPU_DDIARG_CREATESHADER;

typedef struct AEROGPU_DDI_INPUT_ELEMENT_DESC {
  const char *SemanticName;
  uint32_t SemanticIndex;
  uint32_t Format; // DXGI_FORMAT numeric value
  uint32_t InputSlot;
  uint32_t AlignedByteOffset;
  uint32_t InputSlotClass; // 0 per-vertex, 1 per-instance
  uint32_t InstanceDataStepRate;
} AEROGPU_DDI_INPUT_ELEMENT_DESC;

typedef struct AEROGPU_DDIARG_CREATEINPUTLAYOUT {
  const AEROGPU_DDI_INPUT_ELEMENT_DESC *pElements;
  uint32_t NumElements;
} AEROGPU_DDIARG_CREATEINPUTLAYOUT;

typedef struct AEROGPU_DDIARG_CREATERENDERTARGETVIEW {
  D3D10DDI_HRESOURCE hResource;
} AEROGPU_DDIARG_CREATERENDERTARGETVIEW;

typedef struct AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW {
  D3D10DDI_HRESOURCE hResource;
} AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW;

typedef enum AEROGPU_DDI_SRV_DIMENSION {
  AEROGPU_DDI_SRV_DIMENSION_UNKNOWN = 0,
  AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D = 3,
} AEROGPU_DDI_SRV_DIMENSION;

typedef struct AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW {
  D3D10DDI_HRESOURCE hResource;
  uint32_t Format; // DXGI_FORMAT numeric value (0 = use resource format)
  uint32_t ViewDimension; // AEROGPU_DDI_SRV_DIMENSION
  uint32_t MostDetailedMip;
  uint32_t MipLevels;
} AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW;

typedef struct AEROGPU_DDIARG_CREATESAMPLER {
  uint32_t Filter; // D3D11_FILTER numeric value (subset accepted)
  uint32_t AddressU; // D3D11_TEXTURE_ADDRESS_MODE numeric value
  uint32_t AddressV;
  uint32_t AddressW;
} AEROGPU_DDIARG_CREATESAMPLER;

typedef struct AEROGPU_DDIARG_CREATEBLENDSTATE {
  uint32_t dummy;
} AEROGPU_DDIARG_CREATEBLENDSTATE;

typedef struct AEROGPU_DDIARG_CREATERASTERIZERSTATE {
  uint32_t dummy;
} AEROGPU_DDIARG_CREATERASTERIZERSTATE;

typedef struct AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE {
  uint32_t dummy;
} AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE;

typedef struct AEROGPU_DDI_VIEWPORT {
  float TopLeftX;
  float TopLeftY;
  float Width;
  float Height;
  float MinDepth;
  float MaxDepth;
} AEROGPU_DDI_VIEWPORT;

typedef enum AEROGPU_DDI_CLEAR_FLAGS {
  AEROGPU_DDI_CLEAR_DEPTH = 0x1,
  AEROGPU_DDI_CLEAR_STENCIL = 0x2,
} AEROGPU_DDI_CLEAR_FLAGS;

typedef struct AEROGPU_DDIARG_PRESENT {
  D3D10DDI_HRESOURCE hBackBuffer;
  uint32_t SyncInterval;
} AEROGPU_DDIARG_PRESENT;

// -------------------------------------------------------------------------------------------------
// Optional device callback table (allocation-backed resources + submission plumbing)
// -------------------------------------------------------------------------------------------------

// Stable allocation ID used by the AeroGPU per-submit allocation table (`alloc_id`).
//
// This is the value that must be placed in `aerogpu_cmd_create_*::backing_alloc_id`
// (and therefore matches `aerogpu_alloc_entry.alloc_id`) when a resource is backed
// by guest memory.
//
// On real Win7/WDDM 1.1, this is a driver-defined `u32` persisted in WDDM
// allocation private driver data (`aerogpu_wddm_alloc_priv{,_v2}.alloc_id` in
// `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`). It is intentionally not the
// numeric value of the UMD-visible allocation handle (`D3DKMT_HANDLE`) and not
// the KMD-visible `DXGK_ALLOCATIONLIST::hAllocation` identity.
typedef uint32_t AEROGPU_WDDM_ALLOCATION_HANDLE;

// Allocate backing storage for a resource and return the stable allocation ID.
// For Texture2D allocations, the callback may also provide the linear row pitch.
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_ALLOCATE_BACKING)(
    void *pUserContext,
    const AEROGPU_DDIARG_CREATERESOURCE *pDesc,
    AEROGPU_WDDM_ALLOCATION_HANDLE *out_alloc_handle,
    uint64_t *out_alloc_size_bytes,
    uint32_t *out_row_pitch_bytes);

// Map/unmap a WDDM allocation for CPU access.
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_MAP_ALLOCATION)(void *pUserContext,
                                                                 AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle,
                                                                 void **out_cpu_ptr);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_UNMAP_ALLOCATION)(void *pUserContext,
                                                                AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle);

// Submit a command buffer and its referenced allocations.
// The real Win7/WDDM implementation will ensure the allocation handles are
// included in the runtime's allocation list so the KMD can build an
// `aerogpu_alloc_table` for the host.
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SUBMIT_CMD_STREAM)(
    void *pUserContext,
    const void *cmd_stream,
    uint32_t cmd_stream_size_bytes,
    const AEROGPU_WDDM_ALLOCATION_HANDLE *alloc_handles,
    uint32_t alloc_count,
    uint64_t *out_fence);

struct AEROGPU_D3D10_11_DEVICECALLBACKS {
  void *pUserContext;
  PFNAEROGPU_DDI_ALLOCATE_BACKING pfnAllocateBacking;
  PFNAEROGPU_DDI_MAP_ALLOCATION pfnMapAllocation;
  PFNAEROGPU_DDI_UNMAP_ALLOCATION pfnUnmapAllocation;
  PFNAEROGPU_DDI_SUBMIT_CMD_STREAM pfnSubmitCmdStream;
};

// Resource update/copy DDI structs (minimal).
typedef struct AEROGPU_DDI_MAPPED_SUBRESOURCE {
  void* pData;
  uint32_t RowPitch;
  uint32_t DepthPitch;
} AEROGPU_DDI_MAPPED_SUBRESOURCE;

typedef struct AEROGPU_DDI_BOX {
  uint32_t left;
  uint32_t top;
  uint32_t front;
  uint32_t right;
  uint32_t bottom;
  uint32_t back;
} AEROGPU_DDI_BOX;

// Runtime callback subset used by Map/Unmap.
//
// In real WDK builds this comes from `d3dumddi.h`. For the repository build we
// only need Lock/Unlock.
#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
typedef struct D3DDDICB_LOCK {
  uint32_t hAllocation;
  uint32_t Flags;
  uint32_t Subresource;
  uint32_t Offset;
  uint32_t Size;
  void* pData;
  uint32_t Pitch;
  uint32_t SlicePitch;
} D3DDDICB_LOCK;

typedef struct D3DDDICB_UNLOCK {
  uint32_t hAllocation;
  uint32_t Subresource;
} D3DDDICB_UNLOCK;

typedef HRESULT(AEROGPU_APIENTRY* PFND3DDDICB_LOCK)(D3D10DDI_HDEVICE, D3DDDICB_LOCK*);
typedef HRESULT(AEROGPU_APIENTRY* PFND3DDDICB_UNLOCK)(D3D10DDI_HDEVICE, const D3DDDICB_UNLOCK*);

struct D3DDDI_DEVICECALLBACKS {
  PFND3DDDICB_LOCK pfnLockCb;
  PFND3DDDICB_UNLOCK pfnUnlockCb;
};
#endif

typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYDEVICE)(D3D10DDI_HDEVICE);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATERESOURCESIZE)(D3D10DDI_HDEVICE,
                                                                         const AEROGPU_DDIARG_CREATERESOURCE *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATERESOURCE)(D3D10DDI_HDEVICE,
                                                                  const AEROGPU_DDIARG_CREATERESOURCE *,
                                                                  D3D10DDI_HRESOURCE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYRESOURCE)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATESHADERSIZE)(D3D10DDI_HDEVICE,
                                                                       const AEROGPU_DDIARG_CREATESHADER *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEVERTEXSHADER)(D3D10DDI_HDEVICE,
                                                                      const AEROGPU_DDIARG_CREATESHADER *,
                                                                      D3D10DDI_HSHADER);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEPIXELSHADER)(D3D10DDI_HDEVICE,
                                                                    const AEROGPU_DDIARG_CREATESHADER *,
                                                                    D3D10DDI_HSHADER);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYSHADER)(D3D10DDI_HDEVICE, D3D10DDI_HSHADER);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATEINPUTLAYOUTSIZE)(D3D10DDI_HDEVICE,
                                                                            const AEROGPU_DDIARG_CREATEINPUTLAYOUT *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEINPUTLAYOUT)(D3D10DDI_HDEVICE,
                                                                    const AEROGPU_DDIARG_CREATEINPUTLAYOUT *,
                                                                    D3D10DDI_HELEMENTLAYOUT);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYINPUTLAYOUT)(D3D10DDI_HDEVICE, D3D10DDI_HELEMENTLAYOUT);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATERTVSIZE)(D3D10DDI_HDEVICE,
                                                                    const AEROGPU_DDIARG_CREATERENDERTARGETVIEW *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATERTV)(D3D10DDI_HDEVICE,
                                                            const AEROGPU_DDIARG_CREATERENDERTARGETVIEW *,
                                                            D3D10DDI_HRENDERTARGETVIEW);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYRTV)(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATEDSVSIZE)(D3D10DDI_HDEVICE,
                                                                    const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEDSV)(D3D10DDI_HDEVICE,
                                                             const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW *,
                                                             D3D10DDI_HDEPTHSTENCILVIEW);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYDSV)(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATESHADERRESOURCEVIEWSIZE)(
    D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATESHADERRESOURCEVIEW)(D3D10DDI_HDEVICE,
                                                                          const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW *,
                                                                          D3D10DDI_HSHADERRESOURCEVIEW);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYSHADERRESOURCEVIEW)(D3D10DDI_HDEVICE, D3D10DDI_HSHADERRESOURCEVIEW);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATESAMPLERSIZE)(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESAMPLER *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATESAMPLER)(D3D10DDI_HDEVICE,
                                                                const AEROGPU_DDIARG_CREATESAMPLER *,
                                                                D3D10DDI_HSAMPLER);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYSAMPLER)(D3D10DDI_HDEVICE, D3D10DDI_HSAMPLER);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATEBLENDSTATESIZE)(D3D10DDI_HDEVICE,
                                                                           const AEROGPU_DDIARG_CREATEBLENDSTATE *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEBLENDSTATE)(D3D10DDI_HDEVICE,
                                                                   const AEROGPU_DDIARG_CREATEBLENDSTATE *,
                                                                   D3D10DDI_HBLENDSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYBLENDSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATERASTERIZERSTATESIZE)(
    D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERASTERIZERSTATE *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATERASTERIZERSTATE)(D3D10DDI_HDEVICE,
                                                                        const AEROGPU_DDIARG_CREATERASTERIZERSTATE *,
                                                                        D3D10DDI_HRASTERIZERSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYRASTERIZERSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE);

typedef SIZE_T(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CALCPRIVATEDEPTHSTENCILSTATESIZE)(
    D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CREATEDEPTHSTENCILSTATE)(D3D10DDI_HDEVICE,
                                                                          const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE *,
                                                                          D3D10DDI_HDEPTHSTENCILSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DESTROYDEPTHSTENCILSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE);

typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETRENDERTARGETS)(D3D10DDI_HDEVICE,
                                                                 D3D10DDI_HRENDERTARGETVIEW,
                                                                 D3D10DDI_HDEPTHSTENCILVIEW);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CLEARRTV)(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW, const float[4]);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_CLEARDSV)(D3D10DDI_HDEVICE,
                                                        D3D10DDI_HDEPTHSTENCILVIEW,
                                                        uint32_t clear_flags,
                                                        float depth,
                                                        uint8_t stencil);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETINPUTLAYOUT)(D3D10DDI_HDEVICE, D3D10DDI_HELEMENTLAYOUT);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETVERTEXBUFFER)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE, uint32_t stride,
                                                               uint32_t offset);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETINDEXBUFFER)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE, uint32_t format,
                                                              uint32_t offset);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETVIEWPORT)(D3D10DDI_HDEVICE, const AEROGPU_DDI_VIEWPORT *);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETDRAWSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HSHADER vs, D3D10DDI_HSHADER ps);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETBLENDSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETRASTERIZERSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETDEPTHSTENCILSTATE)(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETPRIMITIVETOPOLOGY)(D3D10DDI_HDEVICE, uint32_t topology);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETCONSTANTBUFFERS)(D3D10DDI_HDEVICE,
                                                                 uint32_t start_slot,
                                                                 uint32_t buffer_count,
                                                                 const D3D10DDI_HRESOURCE *pBuffers);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETSHADERRESOURCES)(D3D10DDI_HDEVICE,
                                                                 uint32_t start_slot,
                                                                 uint32_t view_count,
                                                                 const D3D10DDI_HSHADERRESOURCEVIEW *pViews);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_SETSAMPLERS)(D3D10DDI_HDEVICE,
                                                         uint32_t start_slot,
                                                         uint32_t sampler_count,
                                                         const D3D10DDI_HSAMPLER *pSamplers);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DRAW)(D3D10DDI_HDEVICE, uint32_t vertex_count, uint32_t start_vertex);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DRAWINDEXED)(D3D10DDI_HDEVICE, uint32_t index_count, uint32_t start_index,
                                                           int32_t base_vertex);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_PRESENT)(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_PRESENT *);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_FLUSH)(D3D10DDI_HDEVICE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_ROTATERESOURCEIDENTITIES)(D3D10DDI_HDEVICE,
                                                                        D3D10DDI_HRESOURCE *pResources,
                                                                        uint32_t numResources);

// Map/Unmap (D3D10/11-style resource updates).
//
// Win7 D3D11 runtimes may bypass the generic `pfnMap` and use specialized map
// entrypoints for staging resources and dynamic buffers. Keep this surface area
// available even in the "minimal ABI subset" build so the translation layer can
// be validated without WDK headers.
// NOTE: `AEROGPU_DDI_MAPPED_SUBRESOURCE` is declared above alongside other
// resource-copy/update helpers.
// D3D11_MAP numeric values from d3d11.h. D3D10 runtimes use a compatible subset.
typedef enum AEROGPU_DDI_MAP_TYPE {
  AEROGPU_DDI_MAP_READ = 1,
  AEROGPU_DDI_MAP_WRITE = 2,
  AEROGPU_DDI_MAP_READ_WRITE = 3,
  AEROGPU_DDI_MAP_WRITE_DISCARD = 4,
  AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE = 5,
} AEROGPU_DDI_MAP_TYPE;
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_MAP)(D3D10DDI_HDEVICE,
                                                      D3D10DDI_HRESOURCE,
                                                      uint32_t subresource,
                                                      uint32_t map_type,
                                                      uint32_t map_flags,
                                                      AEROGPU_DDI_MAPPED_SUBRESOURCE* pMapped);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_UNMAP)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE, uint32_t subresource);

typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_STAGINGRESOURCEMAP)(D3D10DDI_HDEVICE,
                                                                     D3D10DDI_HRESOURCE,
                                                                     uint32_t subresource,
                                                                     uint32_t map_type,
                                                                     uint32_t map_flags,
                                                                     AEROGPU_DDI_MAPPED_SUBRESOURCE* pMapped);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_STAGINGRESOURCEUNMAP)(D3D10DDI_HDEVICE,
                                                                    D3D10DDI_HRESOURCE,
                                                                    uint32_t subresource);

typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DYNAMICIABUFFERMAPDISCARD)(D3D10DDI_HDEVICE,
                                                                           D3D10DDI_HRESOURCE,
                                                                           void** ppData);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DYNAMICIABUFFERMAPNOOVERWRITE)(D3D10DDI_HDEVICE,
                                                                                D3D10DDI_HRESOURCE,
                                                                                void** ppData);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DYNAMICIABUFFERUNMAP)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE);

typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DYNAMICCONSTANTBUFFERMAPDISCARD)(D3D10DDI_HDEVICE,
                                                                                  D3D10DDI_HRESOURCE,
                                                                                  void** ppData);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_DYNAMICCONSTANTBUFFERUNMAP)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_UPDATESUBRESOURCEUP)(D3D10DDI_HDEVICE,
                                                                   D3D10DDI_HRESOURCE,
                                                                   uint32_t dst_subresource,
                                                                   const AEROGPU_DDI_BOX* pDstBox,
                                                                   const void* pSysMem,
                                                                   uint32_t SysMemPitch,
                                                                   uint32_t SysMemSlicePitch);
typedef void(AEROGPU_APIENTRY *PFNAEROGPU_DDI_COPYRESOURCE)(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE dst, D3D10DDI_HRESOURCE src);
typedef HRESULT(AEROGPU_APIENTRY *PFNAEROGPU_DDI_COPYSUBRESOURCEREGION)(D3D10DDI_HDEVICE,
                                                                        D3D10DDI_HRESOURCE dst,
                                                                        uint32_t dst_subresource,
                                                                        uint32_t dst_x,
                                                                        uint32_t dst_y,
                                                                        uint32_t dst_z,
                                                                        D3D10DDI_HRESOURCE src,
                                                                        uint32_t src_subresource,
                                                                        const AEROGPU_DDI_BOX* pSrcBox);

struct AEROGPU_D3D10_11_DEVICEFUNCS {
  PFNAEROGPU_DDI_DESTROYDEVICE pfnDestroyDevice;

  PFNAEROGPU_DDI_CALCPRIVATERESOURCESIZE pfnCalcPrivateResourceSize;
  PFNAEROGPU_DDI_CREATERESOURCE pfnCreateResource;
  PFNAEROGPU_DDI_DESTROYRESOURCE pfnDestroyResource;

  PFNAEROGPU_DDI_CALCPRIVATESHADERSIZE pfnCalcPrivateShaderSize;
  PFNAEROGPU_DDI_CREATEVERTEXSHADER pfnCreateVertexShader;
  PFNAEROGPU_DDI_CREATEPIXELSHADER pfnCreatePixelShader;
  PFNAEROGPU_DDI_DESTROYSHADER pfnDestroyShader;

  PFNAEROGPU_DDI_CALCPRIVATEINPUTLAYOUTSIZE pfnCalcPrivateInputLayoutSize;
  PFNAEROGPU_DDI_CREATEINPUTLAYOUT pfnCreateInputLayout;
  PFNAEROGPU_DDI_DESTROYINPUTLAYOUT pfnDestroyInputLayout;

  PFNAEROGPU_DDI_CALCPRIVATERTVSIZE pfnCalcPrivateRTVSize;
  PFNAEROGPU_DDI_CREATERTV pfnCreateRTV;
  PFNAEROGPU_DDI_DESTROYRTV pfnDestroyRTV;

  PFNAEROGPU_DDI_CALCPRIVATEDSVSIZE pfnCalcPrivateDSVSize;
  PFNAEROGPU_DDI_CREATEDSV pfnCreateDSV;
  PFNAEROGPU_DDI_DESTROYDSV pfnDestroyDSV;

  PFNAEROGPU_DDI_CALCPRIVATESHADERRESOURCEVIEWSIZE pfnCalcPrivateShaderResourceViewSize;
  PFNAEROGPU_DDI_CREATESHADERRESOURCEVIEW pfnCreateShaderResourceView;
  PFNAEROGPU_DDI_DESTROYSHADERRESOURCEVIEW pfnDestroyShaderResourceView;

  PFNAEROGPU_DDI_CALCPRIVATESAMPLERSIZE pfnCalcPrivateSamplerSize;
  PFNAEROGPU_DDI_CREATESAMPLER pfnCreateSampler;
  PFNAEROGPU_DDI_DESTROYSAMPLER pfnDestroySampler;

  PFNAEROGPU_DDI_CALCPRIVATEBLENDSTATESIZE pfnCalcPrivateBlendStateSize;
  PFNAEROGPU_DDI_CREATEBLENDSTATE pfnCreateBlendState;
  PFNAEROGPU_DDI_DESTROYBLENDSTATE pfnDestroyBlendState;

  PFNAEROGPU_DDI_CALCPRIVATERASTERIZERSTATESIZE pfnCalcPrivateRasterizerStateSize;
  PFNAEROGPU_DDI_CREATERASTERIZERSTATE pfnCreateRasterizerState;
  PFNAEROGPU_DDI_DESTROYRASTERIZERSTATE pfnDestroyRasterizerState;

  PFNAEROGPU_DDI_CALCPRIVATEDEPTHSTENCILSTATESIZE pfnCalcPrivateDepthStencilStateSize;
  PFNAEROGPU_DDI_CREATEDEPTHSTENCILSTATE pfnCreateDepthStencilState;
  PFNAEROGPU_DDI_DESTROYDEPTHSTENCILSTATE pfnDestroyDepthStencilState;

  PFNAEROGPU_DDI_SETRENDERTARGETS pfnSetRenderTargets;
  PFNAEROGPU_DDI_CLEARRTV pfnClearRTV;
  PFNAEROGPU_DDI_CLEARDSV pfnClearDSV;

  PFNAEROGPU_DDI_SETINPUTLAYOUT pfnSetInputLayout;
  PFNAEROGPU_DDI_SETVERTEXBUFFER pfnSetVertexBuffer;
  PFNAEROGPU_DDI_SETINDEXBUFFER pfnSetIndexBuffer;
  PFNAEROGPU_DDI_SETVIEWPORT pfnSetViewport;
  PFNAEROGPU_DDI_SETDRAWSTATE pfnSetDrawState;
  PFNAEROGPU_DDI_SETBLENDSTATE pfnSetBlendState;
  PFNAEROGPU_DDI_SETRASTERIZERSTATE pfnSetRasterizerState;
  PFNAEROGPU_DDI_SETDEPTHSTENCILSTATE pfnSetDepthStencilState;
  PFNAEROGPU_DDI_SETPRIMITIVETOPOLOGY pfnSetPrimitiveTopology;

  PFNAEROGPU_DDI_SETCONSTANTBUFFERS pfnVsSetConstantBuffers;
  PFNAEROGPU_DDI_SETCONSTANTBUFFERS pfnPsSetConstantBuffers;
  PFNAEROGPU_DDI_SETSHADERRESOURCES pfnVsSetShaderResources;
  PFNAEROGPU_DDI_SETSHADERRESOURCES pfnPsSetShaderResources;
  PFNAEROGPU_DDI_SETSAMPLERS pfnVsSetSamplers;
  PFNAEROGPU_DDI_SETSAMPLERS pfnPsSetSamplers;

  PFNAEROGPU_DDI_DRAW pfnDraw;
  PFNAEROGPU_DDI_DRAWINDEXED pfnDrawIndexed;
  PFNAEROGPU_DDI_MAP pfnMap;
  PFNAEROGPU_DDI_UNMAP pfnUnmap;
  PFNAEROGPU_DDI_PRESENT pfnPresent;
  PFNAEROGPU_DDI_FLUSH pfnFlush;
  PFNAEROGPU_DDI_ROTATERESOURCEIDENTITIES pfnRotateResourceIdentities;

  PFNAEROGPU_DDI_UPDATESUBRESOURCEUP pfnUpdateSubresourceUP;
  PFNAEROGPU_DDI_COPYRESOURCE pfnCopyResource;
  PFNAEROGPU_DDI_COPYSUBRESOURCEREGION pfnCopySubresourceRegion;

  // Map/Unmap-style entrypoints.
  //
  // Note: Win7 D3D11 runtimes are known to use specialized entrypoints instead
  // of calling the generic `pfnMap`/`pfnUnmap` directly.
  PFNAEROGPU_DDI_STAGINGRESOURCEMAP pfnStagingResourceMap;
  PFNAEROGPU_DDI_STAGINGRESOURCEUNMAP pfnStagingResourceUnmap;

  PFNAEROGPU_DDI_DYNAMICIABUFFERMAPDISCARD pfnDynamicIABufferMapDiscard;
  PFNAEROGPU_DDI_DYNAMICIABUFFERMAPNOOVERWRITE pfnDynamicIABufferMapNoOverwrite;
  PFNAEROGPU_DDI_DYNAMICIABUFFERUNMAP pfnDynamicIABufferUnmap;

  PFNAEROGPU_DDI_DYNAMICCONSTANTBUFFERMAPDISCARD pfnDynamicConstantBufferMapDiscard;
  PFNAEROGPU_DDI_DYNAMICCONSTANTBUFFERUNMAP pfnDynamicConstantBufferUnmap;
};

// D3D10 and D3D11 runtimes look for these entrypoints in the UMD DLL.
//
// Note: Export names are controlled via the module-definition (.def) files in
// this directory so Win32 builds export undecorated `OpenAdapter*` symbols
// (instead of stdcall-decorated `_OpenAdapter*@4`) as expected by Win7 runtimes.
extern "C" {
HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER *pOpenData);
HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER *pOpenData);
HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER *pOpenData);
}
