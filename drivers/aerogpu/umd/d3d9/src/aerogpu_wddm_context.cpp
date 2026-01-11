#include "aerogpu_wddm_context.h"

#include <cstring>
#include <type_traits>
#include <utility>

#include "aerogpu_cmd_stream_writer.h"

namespace aerogpu {
namespace {

template <typename T, typename = void>
struct has_pfnDestroySynchronizationObjectCb : std::false_type {};

template <typename T>
struct has_pfnDestroySynchronizationObjectCb<T, std::void_t<decltype(std::declval<T>().pfnDestroySynchronizationObjectCb)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_pfnDestroyContextCb : std::false_type {};

template <typename T>
struct has_pfnDestroyContextCb<T, std::void_t<decltype(std::declval<T>().pfnDestroyContextCb)>> : std::true_type {};

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)

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

template <typename CallbacksT>
void destroy_sync_object_if_present(const CallbacksT& callbacks, WddmHandle hSyncObject) {
  if constexpr (has_pfnDestroySynchronizationObjectCb<CallbacksT>::value) {
    if (!hSyncObject || !callbacks.pfnDestroySynchronizationObjectCb) {
      return;
    }

    using Fn = decltype(callbacks.pfnDestroySynchronizationObjectCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hSyncObject = hSyncObject;
    (void)callbacks.pfnDestroySynchronizationObjectCb(static_cast<ArgPtr>(&data));
  }
}

template <typename CallbacksT>
void destroy_context_if_present(const CallbacksT& callbacks, WddmHandle hContext) {
  if constexpr (has_pfnDestroyContextCb<CallbacksT>::value) {
    if (!hContext || !callbacks.pfnDestroyContextCb) {
      return;
    }

    using Fn = decltype(callbacks.pfnDestroyContextCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hContext = hContext;
    (void)callbacks.pfnDestroyContextCb(static_cast<ArgPtr>(&data));
  }
}

template <typename CallbacksT>
HRESULT create_device_from_callbacks(const CallbacksT& callbacks, void* hAdapter, WddmHandle* hDeviceOut) {
  if constexpr (!has_pfnCreateDeviceCb<CallbacksT>::value) {
    (void)callbacks;
    (void)hAdapter;
    (void)hDeviceOut;
    return E_NOTIMPL;
  } else {
    if (!hDeviceOut) {
      return E_INVALIDARG;
    }
    *hDeviceOut = 0;

    if (!callbacks.pfnCreateDeviceCb) {
      return E_FAIL;
    }

    using Fn = decltype(callbacks.pfnCreateDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hAdapter = hAdapter;

    HRESULT hr = callbacks.pfnCreateDeviceCb(static_cast<ArgPtr>(&data));
    if (FAILED(hr)) {
      return hr;
    }

    *hDeviceOut = data.hDevice;
    return (*hDeviceOut != 0) ? S_OK : E_FAIL;
  }
}

template <typename CallbacksT>
void destroy_device_if_present(const CallbacksT& callbacks, WddmHandle hDevice) {
  if constexpr (has_pfnDestroyDeviceCb<CallbacksT>::value) {
    if (!hDevice || !callbacks.pfnDestroyDeviceCb) {
      return;
    }

    using Fn = decltype(callbacks.pfnDestroyDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
    Arg data{};
    data.hDevice = hDevice;
    (void)callbacks.pfnDestroyDeviceCb(static_cast<ArgPtr>(&data));
  } else {
    (void)callbacks;
    (void)hDevice;
  }
}

template <typename CallbacksT, typename FnT>
HRESULT create_context_common(const CallbacksT& callbacks, FnT fn, WddmHandle hDevice, WddmContext* ctxOut) {
  using ArgPtr = typename fn_first_param<FnT>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  Arg data{};
  data.hDevice = hDevice;
  data.NodeOrdinal = 0;
  data.EngineAffinity = 0;
  std::memset(&data.Flags, 0, sizeof(data.Flags));
  data.pPrivateDriverData = nullptr;
  data.PrivateDriverDataSize = 0;

  HRESULT hr = fn(static_cast<ArgPtr>(&data));
  if (FAILED(hr)) {
    return hr;
  }

  ctxOut->hContext = data.hContext;
  ctxOut->hSyncObject = data.hSyncObject;
  ctxOut->pCommandBuffer = static_cast<uint8_t*>(data.pCommandBuffer);
  ctxOut->CommandBufferSize = data.CommandBufferSize;
  ctxOut->pAllocationList = data.pAllocationList;
  ctxOut->AllocationListSize = data.AllocationListSize;
  ctxOut->pPatchLocationList = data.pPatchLocationList;
  ctxOut->PatchLocationListSize = data.PatchLocationListSize;
  ctxOut->reset_submission_buffers();
  return S_OK;
}

template <typename CallbacksT>
HRESULT create_context_from_callbacks(const CallbacksT& callbacks, WddmHandle hDevice, WddmContext* ctxOut) {
  // Prefer the v2 CreateContext callback when present (WDDM 1.1+), but fall back
  // to the original entrypoint for older interface versions.
  if constexpr (has_pfnCreateContextCb2<CallbacksT>::value) {
    if (callbacks.pfnCreateContextCb2) {
      return create_context_common(callbacks, callbacks.pfnCreateContextCb2, hDevice, ctxOut);
    }
  }

  if constexpr (has_pfnCreateContextCb<CallbacksT>::value) {
    if (callbacks.pfnCreateContextCb) {
      return create_context_common(callbacks, callbacks.pfnCreateContextCb, hDevice, ctxOut);
    }
    return E_FAIL;
  }

  return E_NOTIMPL;
}

#endif

} // namespace

void WddmContext::reset_submission_buffers() {
  command_buffer_bytes_used = 0;
  allocation_list_entries_used = 0;
  patch_location_entries_used = 0;

  if (!pCommandBuffer || CommandBufferSize < sizeof(aerogpu_cmd_stream_header)) {
    return;
  }

  // Always initialize the command buffer with a valid AeroGPU stream header so
  // the KMD/emulator can parse the DMA stream even if the submission is empty.
  SpanCmdStreamWriter writer(pCommandBuffer, CommandBufferSize);
  writer.reset();
  command_buffer_bytes_used = static_cast<uint32_t>(writer.bytes_used());
}

void WddmContext::destroy(const WddmDeviceCallbacks& callbacks) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  destroy_sync_object_if_present(callbacks, hSyncObject);
  destroy_context_if_present(callbacks, hContext);
#else
  (void)callbacks;
#endif

  hContext = 0;
  hSyncObject = 0;
  pCommandBuffer = nullptr;
  CommandBufferSize = 0;
  pAllocationList = nullptr;
  AllocationListSize = 0;
  pPatchLocationList = nullptr;
  PatchLocationListSize = 0;
  command_buffer_bytes_used = 0;
  allocation_list_entries_used = 0;
  patch_location_entries_used = 0;
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)

HRESULT wddm_create_device(const WddmDeviceCallbacks& callbacks, void* hAdapter, WddmHandle* hDeviceOut) {
  return create_device_from_callbacks(callbacks, hAdapter, hDeviceOut);
}

void wddm_destroy_device(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice) {
  destroy_device_if_present(callbacks, hDevice);
}

HRESULT wddm_create_context(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice, WddmContext* ctxOut) {
  if (!ctxOut) {
    return E_INVALIDARG;
  }

  *ctxOut = WddmContext{};

  if (!hDevice) {
    return E_INVALIDARG;
  }
  return create_context_from_callbacks(callbacks, hDevice, ctxOut);
}

#endif

} // namespace aerogpu
