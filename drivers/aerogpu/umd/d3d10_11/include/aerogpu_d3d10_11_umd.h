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
// When integrating with a real WDK build, define AEROGPU_UMD_USE_WDK_HEADERS=1
// to include the official headers instead of the local ABI subset.

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
#endif

// -------------------------------------------------------------------------------------------------
// Minimal D3D10/11 DDI ABI subset (Win7 milestone)
// -------------------------------------------------------------------------------------------------

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // The canonical build should use the official headers.
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

typedef SIZE_T(AEROGPU_APIENTRY *PFND3D10DDI_CALCPRIVATEDEVICESIZE)(D3D10DDI_HADAPTER,
                                                                    const D3D10DDIARG_CREATEDEVICE *);
typedef HRESULT(AEROGPU_APIENTRY *PFND3D10DDI_CREATEDEVICE)(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE *);
typedef void(AEROGPU_APIENTRY *PFND3D10DDI_CLOSEADAPTER)(D3D10DDI_HADAPTER);
typedef HRESULT(AEROGPU_APIENTRY *PFND3D10DDI_GETCAPS)(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS *);

typedef struct D3D10DDI_ADAPTERFUNCS {
  PFND3D10DDI_CALCPRIVATEDEVICESIZE pfnCalcPrivateDeviceSize;
  PFND3D10DDI_CREATEDEVICE pfnCreateDevice;
  PFND3D10DDI_CLOSEADAPTER pfnCloseAdapter;
  PFND3D10DDI_GETCAPS pfnGetCaps;
} D3D10DDI_ADAPTERFUNCS;

struct D3D10DDIARG_OPENADAPTER {
  UINT Interface;
  UINT Version;
  D3D10DDI_HRTADAPTER hRTAdapter;
  D3D10DDI_HADAPTER hAdapter;
  D3D10DDI_ADAPTERFUNCS *pAdapterFuncs;
};

// Capability query ABI (minimal subset).
struct D3D10DDIARG_GETCAPS {
  UINT Type;
  void *pData;
  UINT DataSize;
};

// Device ABI subset.
typedef struct AEROGPU_D3D10_11_DEVICEFUNCS AEROGPU_D3D10_11_DEVICEFUNCS;

struct D3D10DDIARG_CREATEDEVICE {
  D3D10DDI_HDEVICE hDevice;
  AEROGPU_D3D10_11_DEVICEFUNCS *pDeviceFuncs;
};
#endif // !_WIN32 || !AEROGPU_UMD_USE_WDK_HEADERS

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
  uint32_t Usage; // D3D10_USAGE / D3D11_USAGE numeric value
  uint32_t CPUAccessFlags; // D3D10_CPU_ACCESS_FLAG / D3D11_CPU_ACCESS_FLAG numeric value

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
} AEROGPU_DDIARG_CREATERESOURCE;

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

  PFNAEROGPU_DDI_DRAW pfnDraw;
  PFNAEROGPU_DDI_DRAWINDEXED pfnDrawIndexed;
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

  // Generic map/unmap wrappers.
  PFNAEROGPU_DDI_MAP pfnMap;
  PFNAEROGPU_DDI_UNMAP pfnUnmap;
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
