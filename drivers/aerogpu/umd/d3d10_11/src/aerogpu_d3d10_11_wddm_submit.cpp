#include "aerogpu_d3d10_11_wddm_submit.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstring>
#include <limits>
#include <mutex>
#include <type_traits>
#include <utility>

#include <windows.h>

#include "aerogpu_d3d10_11_internal.h"

#include "../../../protocol/aerogpu_cmd.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"
#include "../../../protocol/aerogpu_win7_abi.h"

#include "../../common/aerogpu_wddm_submit_buffer_utils.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_11_wddm_alloc_list.h"

#ifndef FAILED
  #define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

namespace aerogpu::d3d10_11 {
namespace {

uint64_t ReadMonitoredFenceValue(volatile uint64_t* ptr) {
  if (!ptr) {
    return 0;
  }
#if defined(_M_IX86)
  // 32-bit UMDs can observe torn 64-bit reads if the monitored fence value is
  // updated concurrently. Avoid that by reading the high 32 bits twice around a
  // low 32-bit read and retrying if the high part changes.
  //
  // This avoids interlocked primitives that might attempt to write to the fence
  // page (some stacks map it read-only).
  const std::uintptr_t addr = reinterpret_cast<std::uintptr_t>(ptr);
  if ((addr & 3u) == 0) {
    volatile uint32_t* p32 = reinterpret_cast<volatile uint32_t*>(ptr);
    for (;;) {
      const uint32_t hi1 = p32[1];
      const uint32_t lo = p32[0];
      const uint32_t hi2 = p32[1];
      if (hi1 == hi2) {
        return (static_cast<uint64_t>(hi2) << 32) | static_cast<uint64_t>(lo);
      }
    }
  }
#endif
  return static_cast<uint64_t>(*ptr);
}

// -----------------------------------------------------------------------------
// WDDM allocation-list tracking (Win7 / WDDM 1.1)
// -----------------------------------------------------------------------------
//
// AeroGPU uses a "no patch list" submission strategy:
// - Commands reference allocations via stable 32-bit `alloc_id` values.
// - `alloc_id` is carried in the per-allocation private driver data blob and
//   copied by the KMD into `DXGK_ALLOCATION::AllocationId`.
// - The KMD builds a per-submit allocation table from the WDDM allocation list,
//   keyed by `AllocationId`, so the host can resolve `alloc_id -> GPA/size`.
// - Since we do not use patch relocations, the allocation-list slot id can be a
//   dense 0..N-1 sequence and does not need to match `alloc_id`.

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

template <typename T>
std::uintptr_t d3d_handle_to_uintptr(T value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<std::uintptr_t>(value);
  } else {
    return static_cast<std::uintptr_t>(value);
  }
}

template <typename T>
T uintptr_to_d3d_handle(std::uintptr_t value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<T>(value);
  } else {
    return static_cast<T>(value);
  }
}

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
    using FieldT = std::remove_reference_t<decltype(args->hContext)>;
    args->hContext = uintptr_to_d3d_handle<FieldT>(d3d_handle_to_uintptr(hContext));
  }
  if constexpr (has_member_hAdapter<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->hAdapter)>;
    args->hAdapter = uintptr_to_d3d_handle<FieldT>(d3d_handle_to_uintptr(hAdapter));
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
      using ElemT = std::remove_reference_t<decltype(args->ObjectHandleArray[0])>;
      const std::uintptr_t handle_value = handles ? d3d_handle_to_uintptr(handles[0]) : 0;
      args->ObjectHandleArray[0] = uintptr_to_d3d_handle<ElemT>(handle_value);
    } else {
      using ElemT = std::remove_reference_t<FieldT>;
      const std::uintptr_t handle_value = handles ? d3d_handle_to_uintptr(handles[0]) : 0;
      args->ObjectHandleArray = uintptr_to_d3d_handle<ElemT>(handle_value);
    }
  } else if constexpr (has_member_hSyncObjects<WaitArgsT>::value) {
    using FieldT = std::remove_reference_t<decltype(args->hSyncObjects)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      using Pointee = std::remove_pointer_t<FieldT>;
      using Base = std::remove_const_t<Pointee>;
      args->hSyncObjects = reinterpret_cast<FieldT>(const_cast<Base*>(handles));
    } else if constexpr (std::is_array_v<FieldT>) {
      using ElemT = std::remove_reference_t<decltype(args->hSyncObjects[0])>;
      const std::uintptr_t handle_value = handles ? d3d_handle_to_uintptr(handles[0]) : 0;
      args->hSyncObjects[0] = uintptr_to_d3d_handle<ElemT>(handle_value);
    } else {
      using ElemT = std::remove_reference_t<FieldT>;
      const std::uintptr_t handle_value = handles ? d3d_handle_to_uintptr(handles[0]) : 0;
      args->hSyncObjects = uintptr_to_d3d_handle<ElemT>(handle_value);
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
struct has_member_NumAllocations : std::false_type {};
template <typename T>
struct has_member_NumAllocations<T, std::void_t<decltype(std::declval<T>().NumAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pPatchLocationList : std::false_type {};
template <typename T>
struct has_member_pPatchLocationList<T, std::void_t<decltype(std::declval<T>().pPatchLocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_PatchLocationListSize : std::false_type {};
template <typename T>
struct has_member_PatchLocationListSize<T, std::void_t<decltype(std::declval<T>().PatchLocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NumPatchLocations : std::false_type {};
template <typename T>
struct has_member_NumPatchLocations<T, std::void_t<decltype(std::declval<T>().NumPatchLocations)>> : std::true_type {};

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
    // Only update the cached allocation-list *capacity* when the submit args
    // struct explicitly splits "capacity" vs "entries used" via `NumAllocations`.
    if constexpr (has_member_AllocationListSize<SubmitArgsT>::value && has_member_NumAllocations<SubmitArgsT>::value) {
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
    // Same semantics as the allocation list: only treat PatchLocationListSize as
    // a capacity field when `NumPatchLocations` exists alongside it.
    if constexpr (has_member_PatchLocationListSize<SubmitArgsT>::value && has_member_NumPatchLocations<SubmitArgsT>::value) {
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

WddmSubmit::~WddmSubmit() noexcept {
  // Destructors are implicitly `noexcept`; be defensive so a misbehaving runtime
  // callback cannot trigger `std::terminate` during device teardown.
  try {
    Shutdown();
  } catch (...) {
  }
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

void fill_allocate_request(D3DDDICB_ALLOCATE* alloc, UINT request_bytes, UINT allocation_list_entries, D3DKMT_HANDLE hContext) {
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
    alloc->AllocationListSize = allocation_list_entries;
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
  bool cap_from_dma_size = false;

  __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) {
    dma_ptr = alloc.pDmaBuffer;
    cmd_ptr = alloc.pDmaBuffer;
  }
  __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) {
    cmd_ptr = alloc.pCommandBuffer;
  }
  __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) {
    if (alloc.CommandBufferSize) {
      cap = alloc.CommandBufferSize;
    }
  }
  __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) {
    if (cap == 0) {
      cap = alloc.DmaBufferSize;
      cap_from_dma_size = true;
    }
  }

  out->command_buffer = cmd_ptr;
  out->dma_buffer = dma_ptr ? dma_ptr : cmd_ptr;
  out->command_buffer_bytes = cap_from_dma_size
                                  ? AdjustCommandBufferSizeFromDmaBuffer(out->dma_buffer, out->command_buffer, cap)
                                  : cap;

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
                                        UINT allocation_list_entries,
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

    fill_allocate_request(&out->alloc, request_bytes, allocation_list_entries, hContext);
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
    bool cap_from_dma_size = false;
    __if_exists(D3DDDICB_GETCOMMANDINFO::CommandBufferSize) {
      out->command_buffer_bytes = info.CommandBufferSize;
    }
    __if_exists(D3DDDICB_GETCOMMANDINFO::DmaBufferSize) {
      if (!out->command_buffer_bytes) {
        out->command_buffer_bytes = info.DmaBufferSize;
        cap_from_dma_size = true;
      }
    }
    if (cap_from_dma_size) {
      out->command_buffer_bytes =
          AdjustCommandBufferSizeFromDmaBuffer(out->dma_buffer, out->command_buffer, out->command_buffer_bytes);
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
                     UINT allocation_list_size,
                     bool do_present,
                     uint64_t* out_fence) {
  if (out_fence) {
    *out_fence = 0;
  }
  if (!callbacks || !runtime_device_private || !buf || !buf->command_buffer || chunk_size == 0) {
    return E_INVALIDARG;
  }
  if (allocation_list_size != 0 && (!buf->allocation_list || buf->allocation_list_entries < allocation_list_size)) {
    AEROGPU_D3D10_11_LOG("wddm_submit: allocation list missing/too small (ptr=%p cap=%u used=%u)",
                         buf->allocation_list,
                         static_cast<unsigned>(buf->allocation_list_entries),
                         static_cast<unsigned>(allocation_list_size));
    return E_OUTOFMEMORY;
  }
  const UINT allocations_used = allocation_list_size;

  const UINT patch_locations_used = 0;

  HRESULT submit_hr = E_NOTIMPL;
  uint64_t fence = 0;
  const HRESULT status_invalid_parameter = static_cast<HRESULT>(kStatusInvalidParameter);

  const auto fill_submit_lists = [&](auto& args) {
    using ArgsT = std::remove_reference_t<decltype(args)>;

    if constexpr (has_member_pAllocationList<ArgsT>::value) {
      args.pAllocationList = buf->allocation_list;
    }
    if constexpr (has_member_AllocationListSize<ArgsT>::value) {
      if constexpr (has_member_NumAllocations<ArgsT>::value) {
        // Capacity field.
        args.AllocationListSize = buf->allocation_list_entries;
      } else {
        // Legacy structs: AllocationListSize is the used count.
        args.AllocationListSize = allocations_used;
      }
    }
    if constexpr (has_member_NumAllocations<ArgsT>::value) {
      args.NumAllocations = allocations_used;
    }

    if constexpr (has_member_pPatchLocationList<ArgsT>::value) {
      args.pPatchLocationList = buf->patch_location_list_entries ? buf->patch_location_list : nullptr;
    }
    if constexpr (has_member_PatchLocationListSize<ArgsT>::value) {
      if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
        // Capacity field.
        args.PatchLocationListSize = buf->patch_location_list_entries;
      } else {
        // Used count.
        args.PatchLocationListSize = patch_locations_used;
      }
    }
    if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
      args.NumPatchLocations = patch_locations_used;
    }
  };

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
      fill_submit_lists(present);
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
        if (allocations_used != 0 && buf->dma_private_data &&
            buf->dma_private_data_bytes >= static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES)) {
          AEROGPU_DMA_PRIV priv{};
          std::memcpy(&priv, buf->dma_private_data, sizeof(priv));
          if (priv.MetaHandle == 0) {
            static std::atomic<uint32_t> g_missing_meta_logs{0};
            const uint32_t n = g_missing_meta_logs.fetch_add(1, std::memory_order_relaxed);
            if ((n < 8) || ((n & 1023u) == 0)) {
              AEROGPU_D3D10_11_LOG("wddm_submit: present missing MetaHandle (allocs=%u)", static_cast<unsigned>(allocations_used));
            }
          }
        }
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
      fill_submit_lists(render);
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
        if (allocations_used != 0 && buf->dma_private_data &&
            buf->dma_private_data_bytes >= static_cast<UINT>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES)) {
          AEROGPU_DMA_PRIV priv{};
          std::memcpy(&priv, buf->dma_private_data, sizeof(priv));
          if (priv.MetaHandle == 0) {
            static std::atomic<uint32_t> g_missing_meta_logs{0};
            const uint32_t n = g_missing_meta_logs.fetch_add(1, std::memory_order_relaxed);
            if ((n < 8) || ((n & 1023u) == 0)) {
              AEROGPU_D3D10_11_LOG("wddm_submit: render missing MetaHandle (allocs=%u)", static_cast<unsigned>(allocations_used));
            }
          }
        }
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
                                         const WddmSubmitAllocation* allocations,
                                         uint32_t allocation_count,
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
  if (allocation_count != 0 && !allocations) {
    return E_INVALIDARG;
  }

  const auto* stream_header = reinterpret_cast<const aerogpu_cmd_stream_header*>(stream_bytes);
  if (stream_header->magic != AEROGPU_CMD_STREAM_MAGIC) {
    AEROGPU_D3D10_11_LOG("wddm_submit: invalid cmd stream magic=0x%08x",
                         static_cast<unsigned>(stream_header->magic));
    return E_INVALIDARG;
  }
  if (stream_header->abi_version != AEROGPU_ABI_VERSION_U32) {
    AEROGPU_D3D10_11_LOG("wddm_submit: unsupported cmd stream abi_version=0x%08x expected=0x%08x",
                         static_cast<unsigned>(stream_header->abi_version),
                         static_cast<unsigned>(AEROGPU_ABI_VERSION_U32));
    return E_INVALIDARG;
  }
  // Forward-compat: allow the caller to pass a buffer larger than the command stream declared
  // size (for example, a fixed-capacity DMA buffer). The stream header carries the actual bytes
  // used. Ignore any trailing bytes.
  const size_t declared_stream_size = static_cast<size_t>(stream_header->size_bytes);
  if (declared_stream_size < sizeof(aerogpu_cmd_stream_header) || declared_stream_size > stream_size) {
    AEROGPU_D3D10_11_LOG("wddm_submit: cmd stream size mismatch header=%u buffer=%llu",
                         static_cast<unsigned>(stream_header->size_bytes),
                         static_cast<unsigned long long>(stream_size));
    return E_INVALIDARG;
  }
  if (declared_stream_size == sizeof(aerogpu_cmd_stream_header)) {
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
  const size_t src_size = declared_stream_size;

  // Validate the packet list so we never submit a truncated/invalid stream.
  size_t validate_off = sizeof(aerogpu_cmd_stream_header);
  while (validate_off < src_size) {
    if (src_size - validate_off < sizeof(aerogpu_cmd_hdr)) {
      AEROGPU_D3D10_11_LOG("wddm_submit: truncated packet header at offset=%llu",
                           static_cast<unsigned long long>(validate_off));
      return E_INVALIDARG;
    }
    const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + validate_off);
    const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
    if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || pkt_size > src_size - validate_off) {
      AEROGPU_D3D10_11_LOG("wddm_submit: invalid packet at offset=%llu size=%llu remaining=%llu",
                           static_cast<unsigned long long>(validate_off),
                           static_cast<unsigned long long>(pkt_size),
                           static_cast<unsigned long long>(src_size - validate_off));
      return E_INVALIDARG;
    }
    validate_off += pkt_size;
  }
  if (validate_off != src_size) {
    AEROGPU_D3D10_11_LOG("wddm_submit: packet walk ended at offset=%llu expected=%llu",
                         static_cast<unsigned long long>(validate_off),
                         static_cast<unsigned long long>(src_size));
    return E_INVALIDARG;
  }

  uint64_t last_fence = 0;

  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;
    const size_t request_sz = remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header);
    if (request_sz > static_cast<size_t>(std::numeric_limits<UINT>::max())) {
      return E_OUTOFMEMORY;
    }
    const UINT request_bytes = static_cast<UINT>(request_sz);
    const UINT allocation_list_entries = static_cast<UINT>(allocation_count);

    SubmissionBuffers buf{};
    HRESULT hr = E_NOTIMPL;

    // Prefer Allocate/Deallocate when present.
    hr = acquire_submit_buffers_allocate(callbacks_, runtime_device_private_, hContext_, request_bytes, allocation_list_entries, &buf);
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

    UINT used_allocation_list_entries = 0;
    if (allocation_count != 0) {
      if (!buf.allocation_list || buf.allocation_list_entries < allocation_list_entries) {
        AEROGPU_D3D10_11_LOG("wddm_submit: %s missing allocation list ptr=%p entries=%u (need >=%u)",
                             buf.needs_deallocate ? "AllocateCb" : "GetCommandBufferCb",
                             buf.allocation_list,
                             static_cast<unsigned>(buf.allocation_list_entries),
                             static_cast<unsigned>(allocation_list_entries));
        release();
        return E_OUTOFMEMORY;
      }

      used_allocation_list_entries = allocation_list_entries;
      for (UINT i = 0; i < used_allocation_list_entries; ++i) {
        const uint32_t handle_u32 = allocations[i].allocation_handle;
        const bool write = allocations[i].write != 0;
        if (handle_u32 == 0) {
          release();
          return E_INVALIDARG;
        }
        aerogpu::wddm::InitAllocationListEntry(
            buf.allocation_list[i],
            static_cast<decltype(buf.allocation_list[i].hAllocation)>(handle_u32),
            i,
            /*write=*/write);
      }
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
      if (src_size - chunk_end < sizeof(aerogpu_cmd_hdr)) {
        // Stream was validated above, so this should be unreachable.
        release();
        return E_INVALIDARG;
      }
      const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + chunk_end);
      const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
      if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || chunk_end + pkt_size > src_size) {
        // Stream was validated above, so this should be unreachable.
        release();
        return E_INVALIDARG;
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

    const bool is_last_chunk = (chunk_end == src_size);
    bool do_present = false;
    if (want_present && is_last_chunk) {
      if constexpr (has_pfnPresentCb<D3DDDI_DEVICECALLBACKS>::value) {
        do_present = callbacks_->pfnPresentCb != nullptr;
      }
    }

    // Security: avoid leaking uninitialized user-mode bytes into the kernel-mode
    // copy of the per-DMA-buffer private data.
    //
    // The AeroGPU KMD overwrites AEROGPU_DMA_PRIV in DxgkDdiRender/DxgkDdiPresent,
    // but some submission paths may bypass those hooks and jump straight to
    // DxgkDdiSubmitCommand. Stamp a deterministic AEROGPU_DMA_PRIV header so the
    // KMD can still distinguish PRESENT vs RENDER submissions in that case.
    const size_t zero_bytes =
        std::min<size_t>(static_cast<size_t>(buf.dma_private_data_bytes), static_cast<size_t>(expected_dma_priv_bytes));
    if (zero_bytes) {
      std::memset(buf.dma_private_data, 0, zero_bytes);
    }
    if (buf.dma_private_data && buf.dma_private_data_bytes >= sizeof(AEROGPU_DMA_PRIV)) {
      auto* priv = reinterpret_cast<AEROGPU_DMA_PRIV*>(buf.dma_private_data);
      priv->Type = do_present ? AEROGPU_SUBMIT_PRESENT : AEROGPU_SUBMIT_RENDER;
      priv->Reserved0 = 0;
      priv->MetaHandle = 0;
    }

    uint64_t fence = 0;
    const HRESULT submit_hr =
        submit_chunk(callbacks_, runtime_device_private_, hContext_, &buf, static_cast<UINT>(chunk_size), used_allocation_list_entries, do_present, &fence);
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
  return WaitForFenceWithTimeout(fence, /*timeout_ms=*/kAeroGpuTimeoutMsInfinite);
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
      (timeout_ms == 0) ? 0ull
                        : (timeout_ms == kAeroGpuTimeoutMsInfinite ? kAeroGpuTimeoutU64Infinite
                                                                   : static_cast<UINT64>(timeout_ms));

  // Prefer the runtime callback (it handles WOW64 thunking correctly).
  if constexpr (has_pfnWaitForSynchronizationObjectCb<D3DDDI_DEVICECALLBACKS>::value) {
    if (callbacks_->pfnWaitForSynchronizationObjectCb) {
      D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
      // Some Win7-era WDK structs include an `hAdapter` field in the wait args.
      // Provide the kernel adapter handle when available so both the runtime
      // callback and the direct KMT thunk have enough context.
      fill_wait_for_sync_object_args(&args, hContext_, kmt_adapter_for_debug_, handles, fence_values, fence, timeout);

      const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                           MakeRtDevice11(runtime_device_private_),
                                           MakeRtDevice10(runtime_device_private_),
                                           &args);
      // Different Win7-era WDKs disagree on which HRESULT represents a timeout.
      // Map the common wait-timeout HRESULTs to DXGI_ERROR_WAS_STILL_DRAWING so
      // higher-level D3D code can use this for Map(DO_NOT_WAIT) behavior.
      if (hr == kDxgiErrorWasStillDrawing || hr == kHrWaitTimeout || hr == kHrErrorTimeout ||
          hr == kHrNtStatusTimeout || hr == kHrNtStatusGraphicsGpuBusy ||
          (timeout_ms == 0 && hr == kHrPending)) {
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
  if (st == kStatusTimeout) {
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
    completed = std::max(completed, ReadMonitoredFenceValue(monitored_fence_value_));
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
         fill_wait_for_sync_object_args(&args,
                                         hContext_,
                                         kmt_adapter_for_debug_,
                                         handles,
                                         fence_values,
                                         last_submitted_fence_,
                                         /*timeout=*/0);

        const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                             MakeRtDevice11(runtime_device_private_),
                                             MakeRtDevice10(runtime_device_private_),
                                             &args);
        // NOTE: `HRESULT_FROM_NT(STATUS_TIMEOUT)` (0x10000102) is a *success*
        // HRESULT, so do not rely solely on `SUCCEEDED/FAILED` when interpreting
        // wait results.
        if (hr == kDxgiErrorWasStillDrawing || hr == kHrWaitTimeout || hr == kHrErrorTimeout ||
            hr == kHrNtStatusTimeout || hr == kHrNtStatusGraphicsGpuBusy ||
            hr == kHrPending) {
          need_kmt_fallback = false;
        } else if (SUCCEEDED(hr)) {
          completed = std::max(completed, last_submitted_fence_);
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
        if (st != kStatusTimeout && NtSuccess(st)) {
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
