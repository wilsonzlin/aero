#include "aerogpu_d3d10_11_wddm_submit.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <algorithm>
#include <cassert>
#include <cstring>
#include <limits>
#include <type_traits>
#include <utility>

#include <windows.h>

#include "../../../protocol/aerogpu_cmd.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"

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
HRESULT create_device_from_callbacks(const CallbacksT* callbacks, void* adapter_handle, D3DKMT_HANDLE* hDeviceOut) {
  if (!hDeviceOut) {
    return E_INVALIDARG;
  }
  *hDeviceOut = 0;
  if (!callbacks) {
    return E_INVALIDARG;
  }

  if constexpr (!has_pfnCreateDeviceCb<CallbacksT>::value) {
    (void)adapter_handle;
    return E_NOTIMPL;
  } else {
    if (!callbacks->pfnCreateDeviceCb) {
      return E_FAIL;
    }

    using Fn = decltype(callbacks->pfnCreateDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;

    Arg data{};
    data.hAdapter = adapter_handle;

    const HRESULT hr = callbacks->pfnCreateDeviceCb(static_cast<ArgPtr>(&data));
    if (FAILED(hr)) {
      return hr;
    }
    *hDeviceOut = data.hDevice;
    return (*hDeviceOut != 0) ? S_OK : E_FAIL;
  }
}

template <typename CallbacksT>
void destroy_device_if_present(const CallbacksT* callbacks, D3DKMT_HANDLE hDevice) {
  if (!callbacks || !hDevice) {
    return;
  }
  if constexpr (!has_pfnDestroyDeviceCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroyDeviceCb) {
      return;
    }
    using Fn = decltype(callbacks->pfnDestroyDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hDevice = hDevice;
    (void)callbacks->pfnDestroyDeviceCb(static_cast<ArgPtr>(&data));
  }
}

template <typename CallbacksT>
void destroy_sync_object_if_present(const CallbacksT* callbacks, D3DKMT_HANDLE hSyncObject) {
  if (!callbacks || !hSyncObject) {
    return;
  }
  if constexpr (!has_pfnDestroySynchronizationObjectCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroySynchronizationObjectCb) {
      return;
    }
    using Fn = decltype(callbacks->pfnDestroySynchronizationObjectCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hSyncObject = hSyncObject;
    (void)callbacks->pfnDestroySynchronizationObjectCb(static_cast<ArgPtr>(&data));
  }
}

template <typename CallbacksT>
void destroy_context_if_present(const CallbacksT* callbacks, D3DKMT_HANDLE hContext) {
  if (!callbacks || !hContext) {
    return;
  }
  if constexpr (!has_pfnDestroyContextCb<CallbacksT>::value) {
    return;
  } else {
    if (!callbacks->pfnDestroyContextCb) {
      return;
    }
    using Fn = decltype(callbacks->pfnDestroyContextCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hContext = hContext;
    (void)callbacks->pfnDestroyContextCb(static_cast<ArgPtr>(&data));
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

template <typename CallbacksT, typename FnT>
HRESULT create_context_common(const CallbacksT* callbacks,
                             FnT fn,
                             D3DKMT_HANDLE hDevice,
                             D3DKMT_HANDLE* hContextOut,
                             D3DKMT_HANDLE* hSyncObjectOut,
                             volatile uint64_t** monitored_fence_value_out) {
  if (!callbacks || !fn || !hDevice || !hContextOut || !hSyncObjectOut) {
    return E_INVALIDARG;
  }

  using ArgPtr = typename fn_first_param<FnT>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  Arg data{};
  data.hDevice = hDevice;
  data.NodeOrdinal = 0;
  data.EngineAffinity = 0;
  std::memset(&data.Flags, 0, sizeof(data.Flags));
  data.pPrivateDriverData = nullptr;
  data.PrivateDriverDataSize = 0;

  const HRESULT hr = fn(static_cast<ArgPtr>(&data));
  if (FAILED(hr)) {
    return hr;
  }

  *hContextOut = data.hContext;
  *hSyncObjectOut = data.hSyncObject;

  if (monitored_fence_value_out) {
    *monitored_fence_value_out = nullptr;
    if constexpr (has_member_pMonitoredFenceValue<Arg>::value) {
      *monitored_fence_value_out = reinterpret_cast<volatile uint64_t*>(data.pMonitoredFenceValue);
    } else if constexpr (has_member_pFenceValue<Arg>::value) {
      *monitored_fence_value_out = reinterpret_cast<volatile uint64_t*>(data.pFenceValue);
    }
  }

  return (*hContextOut != 0 && *hSyncObjectOut != 0) ? S_OK : E_FAIL;
}

template <typename CallbacksT>
HRESULT create_context_from_callbacks(const CallbacksT* callbacks,
                                      D3DKMT_HANDLE hDevice,
                                      D3DKMT_HANDLE* hContextOut,
                                      D3DKMT_HANDLE* hSyncObjectOut,
                                      volatile uint64_t** monitored_fence_value_out) {
  if (!callbacks || !hDevice) {
    return E_INVALIDARG;
  }

  // Prefer CreateContextCb2 when present (WDDM 1.1+), fall back to the older
  // entrypoint for other interface versions.
  if constexpr (has_pfnCreateContextCb2<CallbacksT>::value) {
    if (callbacks->pfnCreateContextCb2) {
      return create_context_common(callbacks, callbacks->pfnCreateContextCb2, hDevice, hContextOut, hSyncObjectOut, monitored_fence_value_out);
    }
  }

  if constexpr (has_pfnCreateContextCb<CallbacksT>::value) {
    if (!callbacks->pfnCreateContextCb) {
      return E_FAIL;
    }
    return create_context_common(callbacks, callbacks->pfnCreateContextCb, hDevice, hContextOut, hSyncObjectOut, monitored_fence_value_out);
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

template <typename SubmitArgsT>
uint64_t extract_submit_fence(const SubmitArgsT& args) {
  uint64_t fence = 0;
  if constexpr (has_member_NewFenceValue<SubmitArgsT>::value) {
    fence = static_cast<uint64_t>(args.NewFenceValue);
  }
  if constexpr (has_member_SubmissionFenceId<SubmitArgsT>::value) {
    // If both fields exist prefer the 64-bit value when present.
    if (fence == 0) {
      fence = static_cast<uint64_t>(args.SubmissionFenceId);
    }
  }
  return fence;
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

  HRESULT hr = create_device_from_callbacks(callbacks_, adapter_handle_, &hDevice_);
  if (FAILED(hr)) {
    Shutdown();
    return hr;
  }

  hr = create_context_from_callbacks(callbacks_, hDevice_, &hContext_, &hSyncObject_, &monitored_fence_value_);
  if (FAILED(hr)) {
    Shutdown();
    return hr;
  }

  return S_OK;
}

void WddmSubmit::Shutdown() {
  if (callbacks_) {
    destroy_sync_object_if_present(callbacks_, hSyncObject_);
    destroy_context_if_present(callbacks_, hContext_);
    destroy_device_if_present(callbacks_, hDevice_);
  }

  callbacks_ = nullptr;
  adapter_handle_ = nullptr;
  runtime_device_private_ = nullptr;
  kmt_adapter_for_debug_ = 0;

  hDevice_ = 0;
  hContext_ = 0;
  hSyncObject_ = 0;
  monitored_fence_value_ = nullptr;
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
}

void deallocate_buffers(const D3DDDI_DEVICECALLBACKS* callbacks, void* runtime_device_private, const D3DDDICB_ALLOCATE& alloc) {
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
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
      dealloc.pDmaBuffer = alloc.pDmaBuffer;
    }
    __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
      dealloc.pCommandBuffer = alloc.pCommandBuffer;
    }
    __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
      dealloc.pAllocationList = alloc.pAllocationList;
    }
    __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
      dealloc.pPatchLocationList = alloc.pPatchLocationList;
    }
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
      dealloc.pDmaBufferPrivateData = alloc.pDmaBufferPrivateData;
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
        deallocate_buffers(callbacks, runtime_device_private, out->alloc);
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
                     const SubmissionBuffers& buf,
                     UINT chunk_size,
                     bool do_present,
                     uint64_t* out_fence) {
  if (out_fence) {
    *out_fence = 0;
  }
  if (!callbacks || !runtime_device_private || !buf.command_buffer || chunk_size == 0) {
    return E_INVALIDARG;
  }

  HRESULT submit_hr = E_NOTIMPL;
  uint64_t fence = 0;

  if (do_present) {
    if constexpr (!has_pfnPresentCb<D3DDDI_DEVICECALLBACKS>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks->pfnPresentCb) {
        return E_NOTIMPL;
      }
      D3DDDICB_PRESENT present = {};
      __if_exists(D3DDDICB_PRESENT::hContext) {
        present.hContext = hContext;
      }
      __if_exists(D3DDDICB_PRESENT::pDmaBuffer) {
        present.pDmaBuffer = buf.dma_buffer;
      }
      __if_exists(D3DDDICB_PRESENT::pCommandBuffer) {
        present.pCommandBuffer = buf.command_buffer;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferSize) {
        present.DmaBufferSize = chunk_size;
      }
      __if_exists(D3DDDICB_PRESENT::CommandLength) {
        present.CommandLength = chunk_size;
      }
      __if_exists(D3DDDICB_PRESENT::CommandBufferSize) {
        present.CommandBufferSize = buf.command_buffer_bytes;
      }
      __if_exists(D3DDDICB_PRESENT::pAllocationList) {
        present.pAllocationList = buf.allocation_list;
      }
      __if_exists(D3DDDICB_PRESENT::AllocationListSize) {
        present.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_PRESENT::pPatchLocationList) {
        present.pPatchLocationList = buf.patch_location_list;
      }
      __if_exists(D3DDDICB_PRESENT::PatchLocationListSize) {
        present.PatchLocationListSize = 0;
      }
      __if_exists(D3DDDICB_PRESENT::pDmaBufferPrivateData) {
        present.pDmaBufferPrivateData = buf.dma_private_data;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferPrivateDataSize) {
        present.DmaBufferPrivateDataSize = buf.dma_private_data_bytes;
      }

      submit_hr = CallCbMaybeHandle(callbacks->pfnPresentCb,
                                    MakeRtDevice11(runtime_device_private),
                                    MakeRtDevice10(runtime_device_private),
                                    &present);
      if (SUCCEEDED(submit_hr)) {
        fence = extract_submit_fence(present);
      }
    }
  } else {
    if constexpr (!has_pfnRenderCb<D3DDDI_DEVICECALLBACKS>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks->pfnRenderCb) {
        return E_NOTIMPL;
      }
      D3DDDICB_RENDER render = {};
      __if_exists(D3DDDICB_RENDER::hContext) {
        render.hContext = hContext;
      }
      __if_exists(D3DDDICB_RENDER::pDmaBuffer) {
        render.pDmaBuffer = buf.dma_buffer;
      }
      __if_exists(D3DDDICB_RENDER::pCommandBuffer) {
        render.pCommandBuffer = buf.command_buffer;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferSize) {
        render.DmaBufferSize = chunk_size;
      }
      __if_exists(D3DDDICB_RENDER::CommandLength) {
        render.CommandLength = chunk_size;
      }
      __if_exists(D3DDDICB_RENDER::CommandBufferSize) {
        render.CommandBufferSize = buf.command_buffer_bytes;
      }
      __if_exists(D3DDDICB_RENDER::pAllocationList) {
        render.pAllocationList = buf.allocation_list;
      }
      __if_exists(D3DDDICB_RENDER::AllocationListSize) {
        render.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_RENDER::pPatchLocationList) {
        render.pPatchLocationList = buf.patch_location_list;
      }
      __if_exists(D3DDDICB_RENDER::PatchLocationListSize) {
        render.PatchLocationListSize = 0;
      }
      __if_exists(D3DDDICB_RENDER::pDmaBufferPrivateData) {
        render.pDmaBufferPrivateData = buf.dma_private_data;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferPrivateDataSize) {
        render.DmaBufferPrivateDataSize = buf.dma_private_data_bytes;
      }

      submit_hr =
          CallCbMaybeHandle(callbacks->pfnRenderCb, MakeRtDevice11(runtime_device_private), MakeRtDevice10(runtime_device_private), &render);
      if (SUCCEEDED(submit_hr)) {
        fence = extract_submit_fence(render);
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
        deallocate_buffers(callbacks_, runtime_device_private_, buf.alloc);
      }
    };

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

    const bool is_last_chunk = (chunk_end == src_size);
    bool do_present = false;
    if (want_present && is_last_chunk) {
      if constexpr (has_pfnPresentCb<D3DDDI_DEVICECALLBACKS>::value) {
        do_present = callbacks_->pfnPresentCb != nullptr;
      }
    }

    uint64_t fence = 0;
    const HRESULT submit_hr =
        submit_chunk(callbacks_, runtime_device_private_, hContext_, buf, static_cast<UINT>(chunk_size), do_present, &fence);
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

  const D3DKMT_HANDLE handles[1] = {hSyncObject_};
  const UINT64 fence_values[1] = {fence};

  const UINT64 timeout =
      (timeout_ms == 0) ? 0ull : (timeout_ms == ~0u ? ~0ull : static_cast<UINT64>(timeout_ms));

  // Prefer the runtime callback (it handles WOW64 thunking correctly).
  if constexpr (has_pfnWaitForSynchronizationObjectCb<D3DDDI_DEVICECALLBACKS>::value) {
    if (callbacks_->pfnWaitForSynchronizationObjectCb) {
      D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
        args.hContext = hContext_;
      }
      args.ObjectCount = 1;
      args.ObjectHandleArray = handles;
      args.FenceValueArray = fence_values;
      args.Timeout = timeout;

      const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                           MakeRtDevice11(runtime_device_private_),
                                           MakeRtDevice10(runtime_device_private_),
                                           &args);
      // Different Win7-era WDKs disagree on which HRESULT represents a timeout.
      // Map the common wait-timeout HRESULTs to DXGI_ERROR_WAS_STILL_DRAWING so
      // higher-level D3D code can use this for Map(DO_NOT_WAIT) behavior.
      if (hr == kDxgiErrorWasStillDrawing || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) || hr == static_cast<HRESULT>(0x10000102L)) {
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
  args.ObjectCount = 1;
  args.ObjectHandleArray = handles;
  args.FenceValueArray = fence_values;
  args.Timeout = timeout;

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
    const D3DKMT_HANDLE handles[1] = {hSyncObject_};
    const UINT64 fence_values[1] = {last_submitted_fence_};

    bool need_kmt_fallback = true;

    if constexpr (has_pfnWaitForSynchronizationObjectCb<D3DDDI_DEVICECALLBACKS>::value) {
      if (callbacks_ && callbacks_->pfnWaitForSynchronizationObjectCb && runtime_device_private_ && hContext_ && hSyncObject_) {
        D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
        __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
          args.hContext = hContext_;
        }
        args.ObjectCount = 1;
        args.ObjectHandleArray = handles;
        args.FenceValueArray = fence_values;
        args.Timeout = 0; // poll

        const HRESULT hr = CallCbMaybeHandle(callbacks_->pfnWaitForSynchronizationObjectCb,
                                             MakeRtDevice11(runtime_device_private_),
                                             MakeRtDevice10(runtime_device_private_),
                                             &args);
        if (SUCCEEDED(hr)) {
          completed = std::max(completed, last_submitted_fence_);
          need_kmt_fallback = false;
        } else if (hr == kDxgiErrorWasStillDrawing) {
          need_kmt_fallback = false;
        }
      }
    }

    if (need_kmt_fallback && hSyncObject_) {
      const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
      if (procs.pfn_wait_for_syncobj) {
        D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
        args.ObjectCount = 1;
        args.ObjectHandleArray = handles;
        args.FenceValueArray = fence_values;
        args.Timeout = 0; // poll

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
