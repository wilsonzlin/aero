#include "aerogpu_d3d10_11_wddm_submit.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <algorithm>
#include <cassert>
#include <cstring>
#include <limits>
#include <mutex>
#include <type_traits>
#include <utility>

#include <windows.h>

#include "../../../protocol/aerogpu_cmd.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"
#include "../../../protocol/aerogpu_win7_abi.h"

#include "aerogpu_d3d10_11_log.h"

#ifndef FAILED
  #define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

namespace aerogpu::d3d10_11 {
namespace {

constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING

constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
}

#ifndef STATUS_TIMEOUT
  #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
#endif

#ifndef STATUS_INVALID_PARAMETER
  #define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif

template <typename Fn>
struct fn_first_param;

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(__stdcall*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename T, typename = void>
struct has_pfnCreateDeviceCb : std::false_type {};

template <typename T>
struct has_pfnCreateDeviceCb<T, std::void_t<decltype(std::declval<T>().pfnCreateDeviceCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnDestroyDeviceCb : std::false_type {};

template <typename T>
struct has_pfnDestroyDeviceCb<T, std::void_t<decltype(std::declval<T>().pfnDestroyDeviceCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnCreateContextCb2 : std::false_type {};

template <typename T>
struct has_pfnCreateContextCb2<T, std::void_t<decltype(std::declval<T>().pfnCreateContextCb2)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnCreateContextCb : std::false_type {};

template <typename T>
struct has_pfnCreateContextCb<T, std::void_t<decltype(std::declval<T>().pfnCreateContextCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnDestroySynchronizationObjectCb : std::false_type {};

template <typename T>
struct has_pfnDestroySynchronizationObjectCb<T, std::void_t<decltype(std::declval<T>().pfnDestroySynchronizationObjectCb)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_pfnDestroyContextCb : std::false_type {};

template <typename T>
struct has_pfnDestroyContextCb<T, std::void_t<decltype(std::declval<T>().pfnDestroyContextCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnAllocateCb : std::false_type {};

template <typename T>
struct has_pfnAllocateCb<T, std::void_t<decltype(std::declval<T>().pfnAllocateCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnDeallocateCb : std::false_type {};

template <typename T>
struct has_pfnDeallocateCb<T, std::void_t<decltype(std::declval<T>().pfnDeallocateCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnGetCommandBufferCb : std::false_type {};

template <typename T>
struct has_pfnGetCommandBufferCb<T, std::void_t<decltype(std::declval<T>().pfnGetCommandBufferCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnRenderCb : std::false_type {};

template <typename T>
struct has_pfnRenderCb<T, std::void_t<decltype(std::declval<T>().pfnRenderCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnPresentCb : std::false_type {};

template <typename T>
struct has_pfnPresentCb<T, std::void_t<decltype(std::declval<T>().pfnPresentCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnWaitForSynchronizationObjectCb : std::false_type {};

template <typename T>
struct has_pfnWaitForSynchronizationObjectCb<T, std::void_t<decltype(std::declval<T>().pfnWaitForSynchronizationObjectCb)>>
    : std::true_type {};

// Some d3dumddi callback signatures accept a runtime device handle (HRTDEVICE)
// as the first parameter. Different WDK vintages disagree on whether that handle
// is D3D10DDI_HRTDEVICE or D3D11DDI_HRTDEVICE, so try both.
template <typename Fn, typename HandleA, typename HandleB, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, HandleA handle_a, HandleB handle_b, Args&&... args) {
  if constexpr (std::is_invocable_v<Fn, HandleA, Args...>) {
    return fn(handle_a, std::forward<Args>(args)...);
  } else if constexpr (std::is_invocable_v<Fn, HandleB, Args...>) {
    return fn(handle_b, std::forward<Args>(args)...);
  } else {
    return fn(std::forward<Args>(args)...);
  }
}

D3D10DDI_HRTDEVICE MakeRtDevice10(void* p) {
  D3D10DDI_HRTDEVICE h{};
  h.pDrvPrivate = p;
  return h;
}

D3D11DDI_HRTDEVICE MakeRtDevice11(void* p) {
  D3D11DDI_HRTDEVICE h{};
  h.pDrvPrivate = p;
  return h;
}

struct AeroGpuD3dkmtProcs {
  decltype(&D3DKMTEscape) pfn_escape = nullptr;
  decltype(&D3DKMTWaitForSynchronizationObject) pfn_wait_for_syncobj = nullptr;
};

const AeroGpuD3dkmtProcs& GetAeroGpuD3dkmtProcs() {
  static AeroGpuD3dkmtProcs procs = [] {
    AeroGpuD3dkmtProcs p{};
    HMODULE gdi32 = GetModuleHandleW(L"gdi32.dll");
    if (!gdi32) {
      gdi32 = LoadLibraryW(L"gdi32.dll");
    }
    if (!gdi32) {
      return p;
    }
    p.pfn_escape = reinterpret_cast<decltype(&D3DKMTEscape)>(GetProcAddress(gdi32, "D3DKMTEscape"));
    p.pfn_wait_for_syncobj =
        reinterpret_cast<decltype(&D3DKMTWaitForSynchronizationObject)>(GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject"));
    return p;
  }();
  return procs;
}

template <typename CallbacksT>
HRESULT create_device_from_callbacks(const CallbacksT* callbacks,
                                     void* adapter_handle,
                                     void* runtime_device_private,
                                     D3DKMT_HANDLE* hDeviceOut) {
  if (!hDeviceOut) {
    return E_INVALIDARG;
  }
  *hDeviceOut = 0;
  if (!callbacks || !runtime_device_private) {
    return E_INVALIDARG;
  }

  if constexpr (!has_pfnCreateDeviceCb<CallbacksT>::value) {
    (void)adapter_handle;
    (void)runtime_device_private;
    return E_NOTIMPL;
  } else {
    if (!callbacks->pfnCreateDeviceCb) {
      return E_FAIL;
    }

    D3DDDICB_CREATEDEVICE data{};
    data.hAdapter = adapter_handle;

    const HRESULT hr = CallCbMaybeHandle(callbacks->pfnCreateDeviceCb,
                                         MakeRtDevice11(runtime_device_private),
                                         MakeRtDevice10(runtime_device_private),
                                         &data);
    if (FAILED(hr)) {
      return hr;
    }
    *hDeviceOut = data.hDevice;
    return (*hDeviceOut != 0) ? S_OK : E_FAIL;
  }
}

template <typename CallbacksT>
void destroy_device_if_present(const CallbacksT* callbacks, void* runtime_device_private, D3DKMT_HANDLE hDevice) {
  if (!callbacks || !runtime_device_private || !hDevice) {
    return;
  }
  if constexpr (!has_pfnDestroyDeviceCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroyDeviceCb) {
      return;
    }
    D3DDDICB_DESTROYDEVICE data{};
    __if_exists(D3DDDICB_DESTROYDEVICE::hDevice) {
      data.hDevice = hDevice;
    }
    (void)CallCbMaybeHandle(callbacks->pfnDestroyDeviceCb,
                            MakeRtDevice11(runtime_device_private),
                            MakeRtDevice10(runtime_device_private),
                            &data);
  }
}

template <typename CallbacksT>
void destroy_sync_object_if_present(const CallbacksT* callbacks, void* runtime_device_private, D3DKMT_HANDLE hSyncObject) {
  if (!callbacks || !runtime_device_private || !hSyncObject) {
    return;
  }
  if constexpr (!has_pfnDestroySynchronizationObjectCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroySynchronizationObjectCb) {
      return;
    }
    D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT data{};
    __if_exists(D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT::hSyncObject) {
      data.hSyncObject = hSyncObject;
    }
    (void)CallCbMaybeHandle(callbacks->pfnDestroySynchronizationObjectCb,
                            MakeRtDevice11(runtime_device_private),
                            MakeRtDevice10(runtime_device_private),
                            &data);
  }
}

template <typename CallbacksT>
void destroy_context_if_present(const CallbacksT* callbacks, void* runtime_device_private, D3DKMT_HANDLE hContext) {
  if (!callbacks || !runtime_device_private || !hContext) {
    return;
  }
  if constexpr (!has_pfnDestroyContextCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroyContextCb) {
      return;
    }
    D3DDDICB_DESTROYCONTEXT data{};
    __if_exists(D3DDDICB_DESTROYCONTEXT::hContext) {
      data.hContext = hContext;
    }
    (void)CallCbMaybeHandle(callbacks->pfnDestroyContextCb,
                            MakeRtDevice11(runtime_device_private),
                            MakeRtDevice10(runtime_device_private),
                            &data);
  }
}

template <typename T, typename = void>
struct has_member_pMonitoredFenceValue : std::false_type {};

template <typename T>
struct has_member_pMonitoredFenceValue<T, std::void_t<decltype(std::declval<T>().pMonitoredFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pFenceValue : std::false_type {};

template <typename T>
struct has_member_pFenceValue<T, std::void_t<decltype(std::declval<T>().pFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDmaBufferPrivateData : std::false_type {};

template <typename T>
struct has_member_pDmaBufferPrivateData<T, std::void_t<decltype(std::declval<T>().pDmaBufferPrivateData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DmaBufferPrivateDataSize : std::false_type {};

template <typename T>
struct has_member_DmaBufferPrivateDataSize<T, std::void_t<decltype(std::declval<T>().DmaBufferPrivateDataSize)>> : std::true_type {};

template <typename CallbacksT, typename FnT>
HRESULT create_context_common(const CallbacksT* callbacks,
                              void* runtime_device_private,
                              FnT fn,
                              D3DKMT_HANDLE hDevice,
                              D3DKMT_HANDLE* hContextOut,
                              D3DKMT_HANDLE* hSyncObjectOut,
                              volatile uint64_t** monitored_fence_value_out,
                              void** dma_private_data_out,
                              UINT* dma_private_data_size_out) {
  if (!callbacks || !runtime_device_private || !fn || !hDevice || !hContextOut || !hSyncObjectOut) {
    return E_INVALIDARG;
  }

  D3DDDICB_CREATECONTEXT data{};
  __if_exists(D3DDDICB_CREATECONTEXT::hDevice) {
    data.hDevice = hDevice;
  }
  __if_exists(D3DDDICB_CREATECONTEXT::NodeOrdinal) {
    data.NodeOrdinal = 0;
  }
  __if_exists(D3DDDICB_CREATECONTEXT::EngineAffinity) {
    data.EngineAffinity = 0;
  }
  __if_exists(D3DDDICB_CREATECONTEXT::pPrivateDriverData) {
    data.pPrivateDriverData = nullptr;
  }
  __if_exists(D3DDDICB_CREATECONTEXT::PrivateDriverDataSize) {
    data.PrivateDriverDataSize = 0;
  }

  const HRESULT hr = CallCbMaybeHandle(fn,
                                       MakeRtDevice11(runtime_device_private),
                                       MakeRtDevice10(runtime_device_private),
                                       &data);
  if (FAILED(hr)) {
    return hr;
  }

  *hContextOut = data.hContext;
  *hSyncObjectOut = data.hSyncObject;

  if (dma_private_data_out) {
    *dma_private_data_out = nullptr;
    if constexpr (has_member_pDmaBufferPrivateData<decltype(data)>::value) {
      *dma_private_data_out = data.pDmaBufferPrivateData;
    }
  }
  if (dma_private_data_size_out) {
    *dma_private_data_size_out = 0;
    if constexpr (has_member_DmaBufferPrivateDataSize<decltype(data)>::value) {
      *dma_private_data_size_out = data.DmaBufferPrivateDataSize;
      if constexpr (has_member_pDmaBufferPrivateData<decltype(data)>::value) {
        if (*dma_private_data_size_out == 0 && data.pDmaBufferPrivateData) {
          // Some WDK vintages include the size field but the runtime may leave it
          // as 0. Treat that as "unknown" and fall back to the fixed AeroGPU
          // Win7 contract size.
          *dma_private_data_size_out = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
        }
      }
    } else if constexpr (has_member_pDmaBufferPrivateData<decltype(data)>::value) {
      // Some WDK vintages expose `pDmaBufferPrivateData` without also carrying a
      // size field. In that case, use the fixed Win7 AeroGPU contract size (as
      // configured via DXGK_DRIVERCAPS::DmaBufferPrivateDataSize).
      if (data.pDmaBufferPrivateData) {
        *dma_private_data_size_out = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
      }
    }
  }

  if (monitored_fence_value_out) {
    *monitored_fence_value_out = nullptr;
    if constexpr (has_member_pMonitoredFenceValue<decltype(data)>::value) {
      *monitored_fence_value_out = reinterpret_cast<volatile uint64_t*>(data.pMonitoredFenceValue);
    } else if constexpr (has_member_pFenceValue<decltype(data)>::value) {
      *monitored_fence_value_out = reinterpret_cast<volatile uint64_t*>(data.pFenceValue);
    }
  }

  return (*hContextOut != 0 && *hSyncObjectOut != 0) ? S_OK : E_FAIL;
}

template <typename CallbacksT>
HRESULT create_context_from_callbacks(const CallbacksT* callbacks,
                                       void* runtime_device_private,
                                       D3DKMT_HANDLE hDevice,
                                       D3DKMT_HANDLE* hContextOut,
                                       D3DKMT_HANDLE* hSyncObjectOut,
                                       volatile uint64_t** monitored_fence_value_out,
                                       void** dma_private_data_out,
                                       UINT* dma_private_data_size_out) {
  if (!callbacks || !runtime_device_private || !hDevice) {
    return E_INVALIDARG;
  }

  // Prefer CreateContextCb2 when present (WDDM 1.1+), fall back to the older
  // entrypoint for other interface versions.
  if constexpr (has_pfnCreateContextCb2<CallbacksT>::value) {
    if (callbacks->pfnCreateContextCb2) {
      return create_context_common(callbacks,
                                   runtime_device_private,
                                   callbacks->pfnCreateContextCb2,
                                   hDevice,
                                   hContextOut,
                                   hSyncObjectOut,
                                   monitored_fence_value_out,
                                   dma_private_data_out,
                                   dma_private_data_size_out);
    }
  }

  if constexpr (has_pfnCreateContextCb<CallbacksT>::value) {
    if (!callbacks->pfnCreateContextCb) {
      return E_FAIL;
    }
    return create_context_common(callbacks,
                                 runtime_device_private,
                                 callbacks->pfnCreateContextCb,
                                 hDevice,
                                 hContextOut,
                                 hSyncObjectOut,
                                 monitored_fence_value_out,
                                 dma_private_data_out,
                                 dma_private_data_size_out);
  }

  return E_NOTIMPL;
}

template <typename T, typename = void>
struct has_member_NewFenceValue : std::false_type {};

template <typename T>
struct has_member_NewFenceValue<T, std::void_t<decltype(std::declval<T>().NewFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SubmissionFenceId : std::false_type {};

template <typename T>
struct has_member_SubmissionFenceId<T, std::void_t<decltype(std::declval<T>().SubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pSubmissionFenceId : std::false_type {};

template <typename T>
struct has_member_pSubmissionFenceId<T, std::void_t<decltype(std::declval<T>().pSubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValue : std::false_type {};

template <typename T>
struct has_member_FenceValue<T, std::void_t<decltype(std::declval<T>().FenceValue)>> : std::true_type {};

template <typename SubmitArgsT>
uint64_t extract_submit_fence(const SubmitArgsT& args) {
  uint64_t fence = 0;
  if constexpr (has_member_NewFenceValue<SubmitArgsT>::value) {
    fence = static_cast<uint64_t>(args.NewFenceValue);
  }
  if constexpr (has_member_FenceValue<SubmitArgsT>::value) {
    if (fence == 0) {
      fence = static_cast<uint64_t>(args.FenceValue);
    }
  }
  if constexpr (has_member_pFenceValue<SubmitArgsT>::value) {
    if (fence == 0 && args.pFenceValue) {
      fence = static_cast<uint64_t>(*args.pFenceValue);
    }
  }
  if constexpr (has_member_SubmissionFenceId<SubmitArgsT>::value) {
    // If both fields exist prefer the 64-bit value when present.
    if (fence == 0) {
      fence = static_cast<uint64_t>(args.SubmissionFenceId);
    }
  }
  if constexpr (has_member_pSubmissionFenceId<SubmitArgsT>::value) {
    if (fence == 0 && args.pSubmissionFenceId) {
      fence = static_cast<uint64_t>(*args.pSubmissionFenceId);
    }
  }
  return fence;
}

template <typename T, typename = void>
struct has_member_hAdapter : std::false_type {};

template <typename T>
struct has_member_hAdapter<T, std::void_t<decltype(std::declval<T>().hAdapter)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hContext : std::false_type {};

template <typename T>
struct has_member_hContext<T, std::void_t<decltype(std::declval<T>().hContext)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ObjectCount : std::false_type {};

template <typename T>
struct has_member_ObjectCount<T, std::void_t<decltype(std::declval<T>().ObjectCount)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ObjectHandleArray : std::false_type {};

template <typename T>
struct has_member_ObjectHandleArray<T, std::void_t<decltype(std::declval<T>().ObjectHandleArray)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hSyncObjects : std::false_type {};

template <typename T>
struct has_member_hSyncObjects<T, std::void_t<decltype(std::declval<T>().hSyncObjects)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValueArray : std::false_type {};

template <typename T>
struct has_member_FenceValueArray<T, std::void_t<decltype(std::declval<T>().FenceValueArray)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Timeout : std::false_type {};

template <typename T>
struct has_member_Timeout<T, std::void_t<decltype(std::declval<T>().Timeout)>> : std::true_type {};

template <typename WaitArgsT>
void fill_wait_for_sync_object_args(WaitArgsT* args,
                                    D3DKMT_HANDLE hContext,
                                    D3DKMT_HANDLE hAdapter,
                                    const D3DKMT_HANDLE* handles,
                                    const UINT64* fence_values,
                                    UINT64 fence_value,
                                    UINT64 timeout) {
  if (!args) {
    return;
  }

  if constexpr (has_member_hContext<WaitArgsT>::value) {
    args->hContext = hContext;
  }
  if constexpr (has_member_hAdapter<WaitArgsT>::value) {
    args->hAdapter = hAdapter;
  }

  if constexpr (has_member_ObjectCount<WaitArgsT>::value) {
    args->ObjectCount = 1;
  }

  // Handle-array field name drift: prefer the array form when present.
  if constexpr (has_member_ObjectHandleArray<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->ObjectHandleArray)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      using Pointee = std::remove_pointer_t<FieldT>;
      using Base = std::remove_const_t<Pointee>;
      args->ObjectHandleArray = reinterpret_cast<FieldT>(const_cast<Base*>(handles));
    } else if constexpr (std::is_array_v<FieldT>) {
      args->ObjectHandleArray[0] = handles ? handles[0] : 0;
    } else {
      args->ObjectHandleArray = handles ? handles[0] : 0;
    }
  } else if constexpr (has_member_hSyncObjects<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->hSyncObjects)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      using Pointee = std::remove_pointer_t<FieldT>;
      using Base = std::remove_const_t<Pointee>;
      args->hSyncObjects = reinterpret_cast<FieldT>(const_cast<Base*>(handles));
    } else if constexpr (std::is_array_v<FieldT>) {
      args->hSyncObjects[0] = handles ? handles[0] : 0;
    } else {
      args->hSyncObjects = handles ? handles[0] : 0;
    }
  }

  // Fence-value field name drift: prefer the array form when present.
  if constexpr (has_member_FenceValueArray<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->FenceValueArray)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      using Pointee = std::remove_pointer_t<FieldT>;
      using Base = std::remove_const_t<Pointee>;
      args->FenceValueArray = reinterpret_cast<FieldT>(const_cast<Base*>(fence_values));
    } else if constexpr (std::is_array_v<FieldT>) {
      args->FenceValueArray[0] = fence_value;
    } else {
      args->FenceValueArray = fence_value;
    }
  } else if constexpr (has_member_FenceValue<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->FenceValue)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      using Pointee = std::remove_pointer_t<FieldT>;
      using Base = std::remove_const_t<Pointee>;
      args->FenceValue = reinterpret_cast<FieldT>(const_cast<Base*>(fence_values));
    } else if constexpr (std::is_array_v<FieldT>) {
      args->FenceValue[0] = fence_value;
    } else {
      args->FenceValue = fence_value;
    }
  }

  if constexpr (has_member_Timeout<WaitArgsT>::value) {
    args->Timeout = timeout;
  }
}

struct SubmissionBuffers {
  void* command_buffer = nullptr;
  UINT command_buffer_bytes = 0;

  // If present, these are returned by the runtime and must be passed to submit.
  void* dma_buffer = nullptr;

  D3DDDI_ALLOCATIONLIST* allocation_list = nullptr;
  UINT allocation_list_entries = 0;

  D3DDDI_PATCHLOCATIONLIST* patch_location_list = nullptr;
  UINT patch_location_list_entries = 0;

  void* dma_private_data = nullptr;
  UINT dma_private_data_bytes = 0;

  // Allocate/deallocate model tracking.
  bool needs_deallocate = false;
  D3DDDICB_ALLOCATE alloc = {};
};

template <typename T, typename = void>
struct has_member_pNewCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pNewCommandBuffer<T, std::void_t<decltype(std::declval<T>().pNewCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewCommandBufferSize : std::false_type {};
template <typename T>
struct has_member_NewCommandBufferSize<T, std::void_t<decltype(std::declval<T>().NewCommandBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewDmaBufferPrivateData : std::false_type {};
template <typename T>
struct has_member_pNewDmaBufferPrivateData<T, std::void_t<decltype(std::declval<T>().pNewDmaBufferPrivateData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewDmaBufferPrivateDataSize : std::false_type {};
template <typename T>
struct has_member_NewDmaBufferPrivateDataSize<T, std::void_t<decltype(std::declval<T>().NewDmaBufferPrivateDataSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewAllocationList : std::false_type {};
template <typename T>
struct has_member_pNewAllocationList<T, std::void_t<decltype(std::declval<T>().pNewAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewAllocationListSize : std::false_type {};
template <typename T>
struct has_member_NewAllocationListSize<T, std::void_t<decltype(std::declval<T>().NewAllocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewPatchLocationList : std::false_type {};
template <typename T>
struct has_member_pNewPatchLocationList<T, std::void_t<decltype(std::declval<T>().pNewPatchLocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewPatchLocationListSize : std::false_type {};
template <typename T>
struct has_member_NewPatchLocationListSize<T, std::void_t<decltype(std::declval<T>().NewPatchLocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CommandBufferSize : std::false_type {};
template <typename T>
struct has_member_CommandBufferSize<T, std::void_t<decltype(std::declval<T>().CommandBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pCommandBuffer<T, std::void_t<decltype(std::declval<T>().pCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocationList : std::false_type {};
template <typename T>
struct has_member_pAllocationList<T, std::void_t<decltype(std::declval<T>().pAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AllocationListSize : std::false_type {};
template <typename T>
struct has_member_AllocationListSize<T, std::void_t<decltype(std::declval<T>().AllocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pPatchLocationList : std::false_type {};
template <typename T>
struct has_member_pPatchLocationList<T, std::void_t<decltype(std::declval<T>().pPatchLocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_PatchLocationListSize : std::false_type {};
template <typename T>
struct has_member_PatchLocationListSize<T, std::void_t<decltype(std::declval<T>().PatchLocationListSize)>> : std::true_type {};

template <typename SubmitArgsT>
void update_buffers_from_submit_args(SubmissionBuffers* buf, const SubmitArgsT& args) {
  if (!buf) {
    return;
  }

  bool updated_cmd_buffer = false;
  if constexpr (has_member_pNewCommandBuffer<SubmitArgsT>::value && has_member_NewCommandBufferSize<SubmitArgsT>::value) {
    if (args.pNewCommandBuffer && args.NewCommandBufferSize) {
      buf->command_buffer = args.pNewCommandBuffer;
      buf->command_buffer_bytes = args.NewCommandBufferSize;
      if (!buf->dma_buffer) {
        buf->dma_buffer = buf->command_buffer;
      }
      updated_cmd_buffer = true;
    }
  }
  if (!updated_cmd_buffer) {
    if constexpr (has_member_pCommandBuffer<SubmitArgsT>::value) {
      if (args.pCommandBuffer) {
        buf->command_buffer = args.pCommandBuffer;
      }
    }
    if constexpr (has_member_CommandBufferSize<SubmitArgsT>::value) {
      if (args.CommandBufferSize) {
        buf->command_buffer_bytes = args.CommandBufferSize;
      }
    }
  }

  bool updated_allocation_list = false;
  if constexpr (has_member_pNewAllocationList<SubmitArgsT>::value && has_member_NewAllocationListSize<SubmitArgsT>::value) {
    if (args.pNewAllocationList && args.NewAllocationListSize) {
      buf->allocation_list = args.pNewAllocationList;
      buf->allocation_list_entries = args.NewAllocationListSize;
      updated_allocation_list = true;
    }
  }
  if (!updated_allocation_list) {
    if constexpr (has_member_pAllocationList<SubmitArgsT>::value) {
      if (args.pAllocationList) {
        buf->allocation_list = args.pAllocationList;
      }
    }
    if constexpr (has_member_AllocationListSize<SubmitArgsT>::value) {
      if (args.AllocationListSize) {
        buf->allocation_list_entries = args.AllocationListSize;
      }
    }
  }

  bool updated_patch_list = false;
  if constexpr (has_member_pNewPatchLocationList<SubmitArgsT>::value && has_member_NewPatchLocationListSize<SubmitArgsT>::value) {
    if (args.pNewPatchLocationList && args.NewPatchLocationListSize) {
      buf->patch_location_list = args.pNewPatchLocationList;
      buf->patch_location_list_entries = args.NewPatchLocationListSize;
      updated_patch_list = true;
    }
  }
  if (!updated_patch_list) {
    if constexpr (has_member_pPatchLocationList<SubmitArgsT>::value) {
      if (args.pPatchLocationList) {
        buf->patch_location_list = args.pPatchLocationList;
      }
    }
    if constexpr (has_member_PatchLocationListSize<SubmitArgsT>::value) {
      if (args.PatchLocationListSize) {
        buf->patch_location_list_entries = args.PatchLocationListSize;
      }
    }
  }

  // pDmaBufferPrivateData is required by the AeroGPU Win7 KMD (DxgkDdiRender /
  // DxgkDdiPresent validate it). The runtime may rotate it alongside the command
  // buffer, so treat it as an in/out field.
  bool updated_dma_priv = false;
  if constexpr (has_member_pNewDmaBufferPrivateData<SubmitArgsT>::value) {
    if (args.pNewDmaBufferPrivateData) {
      buf->dma_private_data = args.pNewDmaBufferPrivateData;
      updated_dma_priv = true;
      if constexpr (has_member_NewDmaBufferPrivateDataSize<SubmitArgsT>::value) {
        if (args.NewDmaBufferPrivateDataSize) {
          buf->dma_private_data_bytes = args.NewDmaBufferPrivateDataSize;
        }
      }
      if (buf->dma_private_data_bytes == 0) {
        buf->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
      }
    }
  }
  if (!updated_dma_priv) {
    if constexpr (has_member_pDmaBufferPrivateData<SubmitArgsT>::value) {
      if (args.pDmaBufferPrivateData) {
        buf->dma_private_data = args.pDmaBufferPrivateData;
      }
    }
    if constexpr (has_member_DmaBufferPrivateDataSize<SubmitArgsT>::value) {
      if (args.DmaBufferPrivateDataSize) {
        buf->dma_private_data_bytes = args.DmaBufferPrivateDataSize;
      }
    }
  }

  if (buf->dma_private_data && buf->dma_private_data_bytes == 0) {
    buf->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  }
}

} // namespace

WddmSubmit::~WddmSubmit() {
  Shutdown();
}

HRESULT WddmSubmit::Init(const D3DDDI_DEVICECALLBACKS* callbacks,
                         void* adapter_handle,
                         void* runtime_device_private,
                         D3DKMT_HANDLE kmt_adapter_for_debug) {
  Shutdown();

  callbacks_ = callbacks;
  adapter_handle_ = adapter_handle;
  runtime_device_private_ = runtime_device_private;
  kmt_adapter_for_debug_ = kmt_adapter_for_debug;

  if (!callbacks_ || !adapter_handle_ || !runtime_device_private_) {
    Shutdown();
    return E_INVALIDARG;
  }

  HRESULT hr = create_device_from_callbacks(callbacks_, adapter_handle_, runtime_device_private_, &hDevice_);
  if (FAILED(hr)) {
    Shutdown();
    return hr;
  }

  hr = create_context_from_callbacks(callbacks_,
                                     runtime_device_private_,
                                     hDevice_,
                                     &hContext_,
                                     &hSyncObject_,
                                     &monitored_fence_value_,
                                     &dma_private_data_,
                                     &dma_private_data_bytes_);
  if (FAILED(hr)) {
    Shutdown();
    return hr;
  }

  const UINT expected_dma_priv_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  if (dma_private_data_bytes_ != 0 && !dma_private_data_) {
    AEROGPU_D3D10_11_LOG("wddm_submit: CreateContext returned DmaBufferPrivateDataSize=%u but pDmaBufferPrivateData=NULL",
                         static_cast<unsigned>(dma_private_data_bytes_));
  } else if (!dma_private_data_ || dma_private_data_bytes_ < expected_dma_priv_bytes) {
    AEROGPU_D3D10_11_LOG("wddm_submit: CreateContext did not provide usable dma private data ptr=%p bytes=%u (need >=%u); "
                         "will rely on Allocate/GetCommandBuffer",
                         dma_private_data_,
                         static_cast<unsigned>(dma_private_data_bytes_),
                         static_cast<unsigned>(expected_dma_priv_bytes));
  }

  return S_OK;
}

void WddmSubmit::Shutdown() {
  if (callbacks_) {
    destroy_sync_object_if_present(callbacks_, runtime_device_private_, hSyncObject_);
    destroy_context_if_present(callbacks_, runtime_device_private_, hContext_);
    destroy_device_if_present(callbacks_, runtime_device_private_, hDevice_);
  }

  callbacks_ = nullptr;
  adapter_handle_ = nullptr;
  runtime_device_private_ = nullptr;
  kmt_adapter_for_debug_ = 0;

  hDevice_ = 0;
  hContext_ = 0;
  hSyncObject_ = 0;
  monitored_fence_value_ = nullptr;
  dma_private_data_ = nullptr;
  dma_private_data_bytes_ = 0;
  last_submitted_fence_ = 0;
  last_completed_fence_ = 0;
}

namespace {

void fill_allocate_request(D3DDDICB_ALLOCATE* alloc, UINT request_bytes, D3DKMT_HANDLE hContext) {
  if (!alloc) {
    return;
  }
  std::memset(alloc, 0, sizeof(*alloc));

  __if_exists(D3DDDICB_ALLOCATE::hContext) {
    alloc->hContext = hContext;
  }
  __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) {
    alloc->DmaBufferSize = request_bytes;
  }
  __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) {
    alloc->CommandBufferSize = request_bytes;
  }
  __if_exists(D3DDDICB_ALLOCATE::AllocationListSize) {
    alloc->AllocationListSize = 0;
  }
  __if_exists(D3DDDICB_ALLOCATE::PatchLocationListSize) {
    alloc->PatchLocationListSize = 0;
  }
}

void extract_alloc_outputs(SubmissionBuffers* out, const D3DDDICB_ALLOCATE& alloc) {
  if (!out) {
    return;
  }

  void* cmd_ptr = nullptr;
  void* dma_ptr = nullptr;
  UINT cap = 0;

  __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) {
    dma_ptr = alloc.pDmaBuffer;
    cmd_ptr = alloc.pDmaBuffer;
  }
  __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) {
    cmd_ptr = alloc.pCommandBuffer;
  }
  __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) {
    cap = alloc.DmaBufferSize;
  }
  __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) {
    if (cap == 0) {
      cap = alloc.CommandBufferSize;
    }
  }

  out->command_buffer = cmd_ptr;
  out->dma_buffer = dma_ptr ? dma_ptr : cmd_ptr;
  out->command_buffer_bytes = cap;

  __if_exists(D3DDDICB_ALLOCATE::pAllocationList) {
    out->allocation_list = alloc.pAllocationList;
  }
  __if_exists(D3DDDICB_ALLOCATE::AllocationListSize) {
    out->allocation_list_entries = alloc.AllocationListSize;
  }

  __if_exists(D3DDDICB_ALLOCATE::pPatchLocationList) {
    out->patch_location_list = alloc.pPatchLocationList;
  }
  __if_exists(D3DDDICB_ALLOCATE::PatchLocationListSize) {
    out->patch_location_list_entries = alloc.PatchLocationListSize;
  }

  __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
    out->dma_private_data = alloc.pDmaBufferPrivateData;
  }
  __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
    out->dma_private_data_bytes = alloc.DmaBufferPrivateDataSize;
  }
  __if_not_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
    if (out->dma_private_data) {
      out->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
    }
  }
  if (out->dma_private_data && out->dma_private_data_bytes == 0) {
    out->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  }
}

void deallocate_buffers(const D3DDDI_DEVICECALLBACKS* callbacks,
                        void* runtime_device_private,
                        D3DKMT_HANDLE hContext,
                        const D3DDDICB_ALLOCATE& alloc) {
  if (!callbacks || !runtime_device_private) {
    return;
  }
  if constexpr (!has_pfnDeallocateCb<D3DDDI_DEVICECALLBACKS>::value) {
    return;
  } else {
    if (!callbacks->pfnDeallocateCb) {
      return;
    }
    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = hContext;
    }
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
      __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) {
        dealloc.pDmaBuffer = alloc.pDmaBuffer;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
      __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) {
        dealloc.pCommandBuffer = alloc.pCommandBuffer;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
      __if_exists(D3DDDICB_ALLOCATE::pAllocationList) {
        dealloc.pAllocationList = alloc.pAllocationList;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
      __if_exists(D3DDDICB_ALLOCATE::pPatchLocationList) {
        dealloc.pPatchLocationList = alloc.pPatchLocationList;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
      __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
        dealloc.pDmaBufferPrivateData = alloc.pDmaBufferPrivateData;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::DmaBufferPrivateDataSize) {
      __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
        dealloc.DmaBufferPrivateDataSize = alloc.DmaBufferPrivateDataSize;
      }
      __if_not_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
        __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
          if (alloc.pDmaBufferPrivateData) {
            dealloc.DmaBufferPrivateDataSize = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
          }
        }
      }
      __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
        if (dealloc.DmaBufferPrivateDataSize == 0 && alloc.pDmaBufferPrivateData) {
          dealloc.DmaBufferPrivateDataSize = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
        }
      }
    }

    (void)CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDevice11(runtime_device_private), MakeRtDevice10(runtime_device_private), &dealloc);
  }
}

HRESULT acquire_submit_buffers_allocate(const D3DDDI_DEVICECALLBACKS* callbacks,
                                       void* runtime_device_private,
                                       D3DKMT_HANDLE hContext,
                                       UINT request_bytes,
                                       SubmissionBuffers* out) {
  if (!callbacks || !runtime_device_private || !out) {
    return E_INVALIDARG;
  }
  *out = SubmissionBuffers{};

  if constexpr (!has_pfnAllocateCb<D3DDDI_DEVICECALLBACKS>::value || !has_pfnDeallocateCb<D3DDDI_DEVICECALLBACKS>::value) {
    return E_NOTIMPL;
  } else {
    if (!callbacks->pfnAllocateCb || !callbacks->pfnDeallocateCb) {
      return E_NOTIMPL;
    }

    fill_allocate_request(&out->alloc, request_bytes, hContext);
    const HRESULT hr = CallCbMaybeHandle(callbacks->pfnAllocateCb,
                                         MakeRtDevice11(runtime_device_private),
                                         MakeRtDevice10(runtime_device_private),
                                         &out->alloc);
    extract_alloc_outputs(out, out->alloc);
    if (FAILED(hr) || !out->command_buffer || out->command_buffer_bytes == 0) {
      // Only deallocate if the runtime actually handed us buffers. Some WDKs
      // return a failure HRESULT without populating out pointers, and calling
      // DeallocateCb in that case is undefined.
      if (out->command_buffer || out->dma_buffer || out->allocation_list || out->patch_location_list || out->dma_private_data) {
        deallocate_buffers(callbacks, runtime_device_private, hContext, out->alloc);
      }
      *out = SubmissionBuffers{};
      return FAILED(hr) ? hr : E_OUTOFMEMORY;
    }

    out->needs_deallocate = true;
    return S_OK;
  }
}

HRESULT acquire_submit_buffers_get_command_buffer(const D3DDDI_DEVICECALLBACKS* callbacks,
                                                  void* runtime_device_private,
                                                  D3DKMT_HANDLE hContext,
                                                  SubmissionBuffers* out) {
  if (!callbacks || !runtime_device_private || !out) {
    return E_INVALIDARG;
  }
  *out = SubmissionBuffers{};

  if constexpr (!has_pfnGetCommandBufferCb<D3DDDI_DEVICECALLBACKS>::value) {
    return E_NOTIMPL;
  } else {
    if (!callbacks->pfnGetCommandBufferCb) {
      return E_NOTIMPL;
    }

    D3DDDICB_GETCOMMANDINFO info{};
    __if_exists(D3DDDICB_GETCOMMANDINFO::hContext) {
      info.hContext = hContext;
    }

    const HRESULT hr = CallCbMaybeHandle(callbacks->pfnGetCommandBufferCb,
                                         MakeRtDevice11(runtime_device_private),
                                         MakeRtDevice10(runtime_device_private),
                                         &info);
    if (FAILED(hr)) {
      return hr;
    }

    __if_exists(D3DDDICB_GETCOMMANDINFO::pCommandBuffer) {
      out->command_buffer = info.pCommandBuffer;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::pDmaBuffer) {
      if (!out->command_buffer) {
        out->command_buffer = info.pDmaBuffer;
      }
      out->dma_buffer = info.pDmaBuffer;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::CommandBufferSize) {
      out->command_buffer_bytes = info.CommandBufferSize;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::DmaBufferSize) {
      if (!out->command_buffer_bytes) {
        out->command_buffer_bytes = info.DmaBufferSize;
      }
    }

    __if_exists(D3DDDICB_GETCOMMANDINFO::pAllocationList) {
      out->allocation_list = info.pAllocationList;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::AllocationListSize) {
      out->allocation_list_entries = info.AllocationListSize;
    }

    __if_exists(D3DDDICB_GETCOMMANDINFO::pPatchLocationList) {
      out->patch_location_list = info.pPatchLocationList;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::PatchLocationListSize) {
      out->patch_location_list_entries = info.PatchLocationListSize;
    }

    __if_exists(D3DDDICB_GETCOMMANDINFO::pDmaBufferPrivateData) {
      out->dma_private_data = info.pDmaBufferPrivateData;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::DmaBufferPrivateDataSize) {
      out->dma_private_data_bytes = info.DmaBufferPrivateDataSize;
    }
    __if_not_exists(D3DDDICB_GETCOMMANDINFO::DmaBufferPrivateDataSize) {
      if (out->dma_private_data) {
        out->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
      }
    }
    if (out->dma_private_data && out->dma_private_data_bytes == 0) {
      out->dma_private_data_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
    }

    if (!out->command_buffer || out->command_buffer_bytes == 0) {
      return E_OUTOFMEMORY;
    }
    if (!out->dma_buffer) {
      out->dma_buffer = out->command_buffer;
    }
    return S_OK;
  }
}

HRESULT submit_chunk(const D3DDDI_DEVICECALLBACKS* callbacks,
                     void* runtime_device_private,
                     D3DKMT_HANDLE hContext,
                     SubmissionBuffers* buf,
                     UINT chunk_size,
                     bool do_present,
                     uint64_t* out_fence) {
  if (out_fence) {
    *out_fence = 0;
  }
  if (!callbacks || !runtime_device_private || !buf || !buf->command_buffer || chunk_size == 0) {
    return E_INVALIDARG;
  }

  HRESULT submit_hr = E_NOTIMPL;
  uint64_t fence = 0;
  const HRESULT status_invalid_parameter = static_cast<HRESULT>(STATUS_INVALID_PARAMETER);

  if (do_present) {
    if constexpr (!has_pfnPresentCb<D3DDDI_DEVICECALLBACKS>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks->pfnPresentCb) {
        return E_NOTIMPL;
      }
      [[maybe_unused]] uint64_t fence_id_tmp = 0;
      [[maybe_unused]] uint64_t fence_value_tmp = 0;
      D3DDDICB_PRESENT present = {};
      __if_exists(D3DDDICB_PRESENT::hContext) {
        present.hContext = hContext;
      }
      __if_exists(D3DDDICB_PRESENT::pDmaBuffer) {
        present.pDmaBuffer = buf->dma_buffer;
      }
      __if_exists(D3DDDICB_PRESENT::pCommandBuffer) {
        present.pCommandBuffer = buf->command_buffer;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferSize) {
        present.DmaBufferSize = chunk_size;
      }
      __if_exists(D3DDDICB_PRESENT::CommandLength) {
        present.CommandLength = chunk_size;
      }
      __if_exists(D3DDDICB_PRESENT::CommandBufferSize) {
        present.CommandBufferSize = buf->command_buffer_bytes;
      }
      __if_exists(D3DDDICB_PRESENT::pAllocationList) {
        present.pAllocationList = buf->allocation_list;
      }
      __if_exists(D3DDDICB_PRESENT::AllocationListSize) {
        present.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_PRESENT::pPatchLocationList) {
        present.pPatchLocationList = buf->patch_location_list;
      }
      __if_exists(D3DDDICB_PRESENT::PatchLocationListSize) {
        present.PatchLocationListSize = 0;
      }
      __if_exists(D3DDDICB_PRESENT::pDmaBufferPrivateData) {
        present.pDmaBufferPrivateData = buf->dma_private_data;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferPrivateDataSize) {
        present.DmaBufferPrivateDataSize = buf->dma_private_data_bytes;
      }
      __if_exists(D3DDDICB_PRESENT::pSubmissionFenceId) {
        present.pSubmissionFenceId = reinterpret_cast<decltype(present.pSubmissionFenceId)>(&fence_id_tmp);
      }
      __if_exists(D3DDDICB_PRESENT::pFenceValue) {
        present.pFenceValue = reinterpret_cast<decltype(present.pFenceValue)>(&fence_value_tmp);
      }

      submit_hr = CallCbMaybeHandle(callbacks->pfnPresentCb,
                                    MakeRtDevice11(runtime_device_private),
                                    MakeRtDevice10(runtime_device_private),
                                    &present);
      if (SUCCEEDED(submit_hr)) {
        fence = extract_submit_fence(present);
        update_buffers_from_submit_args(buf, present);
      } else if (submit_hr == E_INVALIDARG || submit_hr == status_invalid_parameter) {
        AEROGPU_D3D10_11_LOG("wddm_submit: PresentCb invalid parameter hr=0x%08x dma_priv=%p bytes=%u",
                             static_cast<unsigned>(submit_hr),
                             buf->dma_private_data,
                             static_cast<unsigned>(buf->dma_private_data_bytes));
      }
    }
  } else {
    if constexpr (!has_pfnRenderCb<D3DDDI_DEVICECALLBACKS>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks->pfnRenderCb) {
        return E_NOTIMPL;
      }
      [[maybe_unused]] uint64_t fence_id_tmp = 0;
      [[maybe_unused]] uint64_t fence_value_tmp = 0;
      D3DDDICB_RENDER render = {};
      __if_exists(D3DDDICB_RENDER::hContext) {
        render.hContext = hContext;
      }
      __if_exists(D3DDDICB_RENDER::pDmaBuffer) {
        render.pDmaBuffer = buf->dma_buffer;
      }
      __if_exists(D3DDDICB_RENDER::pCommandBuffer) {
        render.pCommandBuffer = buf->command_buffer;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferSize) {
        render.DmaBufferSize = chunk_size;
      }
      __if_exists(D3DDDICB_RENDER::CommandLength) {
        render.CommandLength = chunk_size;
      }
      __if_exists(D3DDDICB_RENDER::CommandBufferSize) {
        render.CommandBufferSize = buf->command_buffer_bytes;
      }
      __if_exists(D3DDDICB_RENDER::pAllocationList) {
        render.pAllocationList = buf->allocation_list;
      }
      __if_exists(D3DDDICB_RENDER::AllocationListSize) {
        render.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_RENDER::pPatchLocationList) {
        render.pPatchLocationList = buf->patch_location_list;
      }
      __if_exists(D3DDDICB_RENDER::PatchLocationListSize) {
        render.PatchLocationListSize = 0;
      }
      __if_exists(D3DDDICB_RENDER::pDmaBufferPrivateData) {
        render.pDmaBufferPrivateData = buf->dma_private_data;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferPrivateDataSize) {
        render.DmaBufferPrivateDataSize = buf->dma_private_data_bytes;
      }
      __if_exists(D3DDDICB_RENDER::pSubmissionFenceId) {
        render.pSubmissionFenceId = reinterpret_cast<decltype(render.pSubmissionFenceId)>(&fence_id_tmp);
      }
      __if_exists(D3DDDICB_RENDER::pFenceValue) {
        render.pFenceValue = reinterpret_cast<decltype(render.pFenceValue)>(&fence_value_tmp);
      }

      submit_hr =
          CallCbMaybeHandle(callbacks->pfnRenderCb, MakeRtDevice11(runtime_device_private), MakeRtDevice10(runtime_device_private), &render);
      if (SUCCEEDED(submit_hr)) {
        fence = extract_submit_fence(render);
        update_buffers_from_submit_args(buf, render);
      } else if (submit_hr == E_INVALIDARG || submit_hr == status_invalid_parameter) {
        AEROGPU_D3D10_11_LOG("wddm_submit: RenderCb invalid parameter hr=0x%08x dma_priv=%p bytes=%u",
                             static_cast<unsigned>(submit_hr),
                             buf->dma_private_data,
                             static_cast<unsigned>(buf->dma_private_data_bytes));
      }
    }
  }

  if (out_fence) {
    *out_fence = fence;
  }
  return submit_hr;
}

} // namespace

HRESULT WddmSubmit::SubmitAeroCmdStream(const uint8_t* stream_bytes,
                                       size_t stream_size,
                                       bool want_present,
                                       uint64_t* out_fence) {
  if (out_fence) {
    *out_fence = 0;
  }
  if (!callbacks_ || !runtime_device_private_ || !hContext_ || !hSyncObject_) {
    return E_FAIL;
  }
  if (!stream_bytes) {
    return E_INVALIDARG;
  }
  if (stream_size < sizeof(aerogpu_cmd_stream_header)) {
    return E_INVALIDARG;
  }
  if (stream_size == sizeof(aerogpu_cmd_stream_header)) {
    return S_OK;
  }

  // Ensure we have at least a render callback for submission.
  if constexpr (has_pfnRenderCb<D3DDDI_DEVICECALLBACKS>::value) {
    if (!callbacks_->pfnRenderCb) {
      return E_FAIL;
    }
  } else {
    return E_NOTIMPL;
  }

  const uint8_t* src = stream_bytes;
  const size_t src_size = stream_size;

  uint64_t last_fence = 0;

  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;
    const size_t request_sz = remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header);
    if (request_sz > static_cast<size_t>(std::numeric_limits<UINT>::max())) {
      return E_OUTOFMEMORY;
    }
    const UINT request_bytes = static_cast<UINT>(request_sz);

    SubmissionBuffers buf{};
    HRESULT hr = E_NOTIMPL;

    // Prefer Allocate/Deallocate when present.
    hr = acquire_submit_buffers_allocate(callbacks_, runtime_device_private_, hContext_, request_bytes, &buf);
    if (hr == E_NOTIMPL) {
      hr = acquire_submit_buffers_get_command_buffer(callbacks_, runtime_device_private_, hContext_, &buf);
    }
    if (FAILED(hr)) {
      return hr;
    }

    const auto release = [&] {
      if (buf.needs_deallocate) {
        deallocate_buffers(callbacks_, runtime_device_private_, hContext_, buf.alloc);
      }
    };

    // DMA buffer private data is a required UMD→KMD ABI for AeroGPU on Win7. The
    // KMD validates that `pDmaBufferPrivateData != NULL`, and dxgkrnl only forwards
    // the pointer when `DmaBufferPrivateDataSize` is non-zero.
    bool used_ctx_dma_priv_ptr_fallback = false;
    bool used_ctx_dma_priv_size_fallback = false;
    if (!buf.dma_private_data && dma_private_data_) {
      buf.dma_private_data = dma_private_data_;
      used_ctx_dma_priv_ptr_fallback = true;
    }
    if (buf.dma_private_data_bytes == 0 && dma_private_data_bytes_ != 0) {
      buf.dma_private_data_bytes = dma_private_data_bytes_;
      used_ctx_dma_priv_size_fallback = true;
    }
    const bool used_ctx_dma_priv_fallback = used_ctx_dma_priv_ptr_fallback || used_ctx_dma_priv_size_fallback;
    if (used_ctx_dma_priv_fallback) {
      static std::once_flag logged_fallback_once;
      std::call_once(logged_fallback_once, [&] {
        AEROGPU_D3D10_11_LOG("wddm_submit: filling missing dma private data ptr/size from CreateContext (ptr=%p bytes=%u)",
                             buf.dma_private_data,
                             static_cast<unsigned>(buf.dma_private_data_bytes));
      });
    }
    const UINT expected_dma_priv_bytes = static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
    if (buf.dma_private_data_bytes != 0 && !buf.dma_private_data) {
      AEROGPU_D3D10_11_LOG("wddm_submit: %s provided dma private data size=%u but ptr=NULL",
                           buf.needs_deallocate ? "AllocateCb" : "GetCommandBufferCb",
                           static_cast<unsigned>(buf.dma_private_data_bytes));
      release();
      return E_FAIL;
    }
    if (!buf.dma_private_data || buf.dma_private_data_bytes < expected_dma_priv_bytes) {
      AEROGPU_D3D10_11_LOG("wddm_submit: %s missing dma private data ptr=%p size=%u (need >=%u)",
                           buf.needs_deallocate ? "AllocateCb" : "GetCommandBufferCb",
                           buf.dma_private_data,
                           static_cast<unsigned>(buf.dma_private_data_bytes),
                           static_cast<unsigned>(expected_dma_priv_bytes));
      release();
      return E_FAIL;
    }
    if (buf.dma_private_data_bytes != expected_dma_priv_bytes) {
      static std::once_flag logged_size_mismatch_once;
      std::call_once(logged_size_mismatch_once, [&] {
        AEROGPU_D3D10_11_LOG("wddm_submit: dma private data size mismatch bytes=%u expected=%u",
                             static_cast<unsigned>(buf.dma_private_data_bytes),
                             static_cast<unsigned>(expected_dma_priv_bytes));
      });
    }
    // Safety: if the runtime reports a larger private-data size than the KMD/UMD
    // contract, clamp to the expected size so dxgkrnl does not copy extra bytes
    // of user-mode memory into kernel-mode buffers.
    if (buf.dma_private_data_bytes > expected_dma_priv_bytes) {
      buf.dma_private_data_bytes = expected_dma_priv_bytes;
    }

    const UINT dma_cap = buf.command_buffer_bytes;
    if (dma_cap < sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_hdr)) {
      release();
      return E_OUTOFMEMORY;
    }

    // Build chunk within dma_cap.
    const size_t chunk_begin = cur;
    size_t chunk_end = cur;
    size_t chunk_size = sizeof(aerogpu_cmd_stream_header);

    while (chunk_end < src_size) {
      const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + chunk_end);
      const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
      if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || chunk_end + pkt_size > src_size) {
        assert(false && "AeroGPU command stream contains an invalid packet");
        break;
      }
      if (chunk_size + pkt_size > dma_cap) {
        break;
      }
      chunk_end += pkt_size;
      chunk_size += pkt_size;
    }

    if (chunk_end == chunk_begin) {
      release();
      return E_OUTOFMEMORY;
    }

    // Copy header + selected packets into the runtime DMA buffer.
    auto* dst = static_cast<uint8_t*>(buf.command_buffer);
    std::memcpy(dst, src, sizeof(aerogpu_cmd_stream_header));
    std::memcpy(dst + sizeof(aerogpu_cmd_stream_header),
                src + chunk_begin,
                chunk_size - sizeof(aerogpu_cmd_stream_header));
    auto* hdr = reinterpret_cast<aerogpu_cmd_stream_header*>(dst);
    hdr->size_bytes = static_cast<uint32_t>(chunk_size);

    // Security: avoid leaking uninitialized user-mode bytes into the kernel-mode
    // copy of the per-DMA-buffer private data. The AeroGPU KMD will overwrite
    // AEROGPU_DMA_PRIV anyway.
    const size_t zero_bytes =
        std::min<size_t>(static_cast<size_t>(buf.dma_private_data_bytes), static_cast<size_t>(expected_dma_priv_bytes));
    if (zero_bytes) {
      std::memset(buf.dma_private_data, 0, zero_bytes);
    }

    const bool is_last_chunk = (chunk_end == src_size);
    bool do_present = false;
    if (want_present && is_last_chunk) {
      if constexpr (has_pfnPresentCb<D3DDDI_DEVICECALLBACKS>::value) {
        do_present = callbacks_->pfnPresentCb != nullptr;
      }
    }

    uint64_t fence = 0;
    const HRESULT submit_hr =
        submit_chunk(callbacks_, runtime_device_private_, hContext_, &buf, static_cast<UINT>(chunk_size), do_present, &fence);
    if (SUCCEEDED(submit_hr) && buf.dma_private_data && buf.dma_private_data_bytes) {
      // Only persist the updated pointer/size when the runtime owns this memory:
      // - GetCommandBuffer path (no Deallocate call), or
      // - CreateContext supplied the pointer (so it is not tied to an AllocateCb
      //   buffer lifetime).
      if (!buf.needs_deallocate || used_ctx_dma_priv_ptr_fallback) {
        dma_private_data_ = buf.dma_private_data;
        dma_private_data_bytes_ = buf.dma_private_data_bytes;
      }
    }
    release();
    if (FAILED(submit_hr)) {
      return submit_hr;
    }

    if (fence != 0) {
      last_fence = fence;
    }
    cur = chunk_end;
  }

  last_submitted_fence_ = std::max(last_submitted_fence_, last_fence);

  if (out_fence) {
    *out_fence = last_fence;
  }
  return S_OK;
}

HRESULT WddmSubmit::WaitForFence(uint64_t fence) {
  // Use the kernel thunk's "infinite" convention (~0ull) rather than treating 0
  // as infinite (0 is used for polling in this module).
  return WaitForFenceWithTimeout(fence, /*timeout_ms=*/~0u);
}

HRESULT WddmSubmit::WaitForFenceWithTimeout(uint64_t fence, uint32_t timeout_ms) {
  if (!callbacks_ || !runtime_device_private_) {
    return E_FAIL;
  }
  if (!hContext_ || !hSyncObject_) {
    return E_FAIL;
  }
  if (fence == 0) {
    return S_OK;
  }

  if (QueryCompletedFence() >= fence) {
    return S_OK;
  }

  D3DKMT_HANDLE handles[1] = {hSyncObject_};
  UINT64 fence_values[1] = {fence};

  const UINT64 timeout =
      (timeout_ms == 0) ? 0ull : (timeout_ms == ~0u ? ~0ull : static_cast<UINT64>(timeout_ms));

  // Prefer the runtime callback (it handles WOW64 thunking correctly).
  if constexpr (has_pfnWaitForSynchronizationObjectCb<D3DDDI_DEVICECALLBACKS>::value) {
    if (callbacks_->pfnWaitForSynchronizationObjectCb) {
      D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
      fill_wait_for_sync_object_args(&args, hContext_, /*hAdapter=*/0, handles, fence_values, fence, timeout);

      const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                           MakeRtDevice11(runtime_device_private_),
                                           MakeRtDevice10(runtime_device_private_),
                                           &args);
      // Different Win7-era WDKs disagree on which HRESULT represents a timeout.
      // Map the common wait-timeout HRESULTs to DXGI_ERROR_WAS_STILL_DRAWING so
      // higher-level D3D code can use this for Map(DO_NOT_WAIT) behavior.
      if (hr == kDxgiErrorWasStillDrawing || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) ||
          hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT) || hr == static_cast<HRESULT>(0x10000102L)) {
        return kDxgiErrorWasStillDrawing;
      }
      if (FAILED(hr)) {
        return hr;
      }

      last_completed_fence_ = std::max(last_completed_fence_, fence);
      (void)QueryCompletedFence();
      return S_OK;
    }
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_wait_for_syncobj) {
    return E_FAIL;
  }

  D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
  fill_wait_for_sync_object_args(&args, hContext_, kmt_adapter_for_debug_, handles, fence_values, fence, timeout);

  const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
  if (st == STATUS_TIMEOUT) {
    return kDxgiErrorWasStillDrawing;
  }
  if (!NtSuccess(st)) {
    return E_FAIL;
  }

  last_completed_fence_ = std::max(last_completed_fence_, fence);
  (void)QueryCompletedFence();
  return S_OK;
}

uint64_t WddmSubmit::QueryCompletedFence() {
  uint64_t completed = last_completed_fence_;

  if (monitored_fence_value_) {
    completed = std::max(completed, static_cast<uint64_t>(*monitored_fence_value_));
  } else if (kmt_adapter_for_debug_) {
    // Debug-only fallback: ask the KMD for its fence tracking state via Escape.
    const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
    if (procs.pfn_escape) {
      aerogpu_escape_query_fence_out q{};
      q.hdr.version = AEROGPU_ESCAPE_VERSION;
      q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
      q.hdr.size = sizeof(q);
      q.hdr.reserved0 = 0;

      D3DKMT_ESCAPE e{};
      e.hAdapter = kmt_adapter_for_debug_;
      e.hDevice = 0;
      e.hContext = 0;
      e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
      e.Flags.Value = 0;
      e.pPrivateDriverData = &q;
      e.PrivateDriverDataSize = sizeof(q);

      const NTSTATUS st = procs.pfn_escape(&e);
      if (NtSuccess(st)) {
        last_submitted_fence_ = std::max(last_submitted_fence_, static_cast<uint64_t>(q.last_submitted_fence));
        completed = std::max(completed, static_cast<uint64_t>(q.last_completed_fence));
      }
    }
  } else if (last_submitted_fence_ != 0) {
    // Conservative fallback: poll the last-submitted fence without relying on a
    // monitored fence CPU VA.
    D3DKMT_HANDLE handles[1] = {hSyncObject_};
    UINT64 fence_values[1] = {last_submitted_fence_};

    bool need_kmt_fallback = true;

    if constexpr (has_pfnWaitForSynchronizationObjectCb<D3DDDI_DEVICECALLBACKS>::value) {
      if (callbacks_ && callbacks_->pfnWaitForSynchronizationObjectCb && runtime_device_private_ && hContext_ && hSyncObject_) {
        D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
        fill_wait_for_sync_object_args(&args, hContext_, /*hAdapter=*/0, handles, fence_values, last_submitted_fence_, /*timeout=*/0);

        const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                             MakeRtDevice11(runtime_device_private_),
                                             MakeRtDevice10(runtime_device_private_),
                                             &args);
        if (SUCCEEDED(hr)) {
          completed = std::max(completed, last_submitted_fence_);
          need_kmt_fallback = false;
        } else if (hr == kDxgiErrorWasStillDrawing || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) || hr == static_cast<HRESULT>(0x10000102L)) {
          need_kmt_fallback = false;
        }
      }
    }

    if (need_kmt_fallback && hSyncObject_) {
      const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
      if (procs.pfn_wait_for_syncobj) {
        D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
        fill_wait_for_sync_object_args(&args,
                                       hContext_,
                                       kmt_adapter_for_debug_,
                                       handles,
                                       fence_values,
                                       last_submitted_fence_,
                                       /*timeout=*/0);

        const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
        if (st != STATUS_TIMEOUT && NtSuccess(st)) {
          completed = std::max(completed, last_submitted_fence_);
        }
      }
    }
  }

  last_completed_fence_ = std::max(last_completed_fence_, completed);
  return completed;
}

} // namespace aerogpu::d3d10_11

#endif // _WIN32 && AEROGPU_UMD_USE_WDK_HEADERS
