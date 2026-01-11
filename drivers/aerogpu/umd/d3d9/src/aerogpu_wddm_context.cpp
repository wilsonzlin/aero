#include "aerogpu_wddm_context.h"

#include <cstring>
#include <type_traits>
#include <utility>

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

#endif

} // namespace

void WddmContext::reset_submission_buffers() {
  command_buffer_bytes_used = 0;
  allocation_list_entries_used = 0;
  patch_location_entries_used = 0;

  if (!pCommandBuffer || CommandBufferSize < sizeof(aerogpu_cmd_stream_header)) {
    return;
  }

  std::memset(pCommandBuffer, 0, sizeof(aerogpu_cmd_stream_header));

  auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(pCommandBuffer);
  stream->magic = AEROGPU_CMD_STREAM_MAGIC;
  stream->abi_version = AEROGPU_ABI_VERSION_U32;
  stream->size_bytes = sizeof(aerogpu_cmd_stream_header);
  stream->flags = AEROGPU_CMD_STREAM_FLAG_NONE;
  stream->reserved0 = 0;
  stream->reserved1 = 0;

  command_buffer_bytes_used = sizeof(aerogpu_cmd_stream_header);
}

void WddmContext::destroy(const WddmDeviceCallbacks& callbacks) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  if constexpr (has_pfnDestroySynchronizationObjectCb<WddmDeviceCallbacks>::value) {
    if (hSyncObject && callbacks.pfnDestroySynchronizationObjectCb) {
      using Fn = decltype(callbacks.pfnDestroySynchronizationObjectCb);
      using ArgPtr = typename fn_first_param<Fn>::type;
      std::remove_pointer_t<ArgPtr> data{};
      data.hSyncObject = hSyncObject;
      (void)callbacks.pfnDestroySynchronizationObjectCb(&data);
    }
  }

  if constexpr (has_pfnDestroyContextCb<WddmDeviceCallbacks>::value) {
    if (hContext && callbacks.pfnDestroyContextCb) {
      using Fn = decltype(callbacks.pfnDestroyContextCb);
      using ArgPtr = typename fn_first_param<Fn>::type;
      std::remove_pointer_t<ArgPtr> data{};
      data.hContext = hContext;
      (void)callbacks.pfnDestroyContextCb(&data);
    }
  }
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
  if (!hDeviceOut) {
    return E_INVALIDARG;
  }
  *hDeviceOut = 0;

  if constexpr (!has_pfnCreateDeviceCb<WddmDeviceCallbacks>::value) {
    return E_NOTIMPL;
  } else {
    if (!callbacks.pfnCreateDeviceCb) {
      return E_FAIL;
    }

    using Fn = decltype(callbacks.pfnCreateDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    std::remove_pointer_t<ArgPtr> data{};
    data.hAdapter = hAdapter;

    HRESULT hr = callbacks.pfnCreateDeviceCb(&data);
    if (FAILED(hr)) {
      return hr;
    }

    *hDeviceOut = data.hDevice;
    return (*hDeviceOut != 0) ? S_OK : E_FAIL;
  }
}

void wddm_destroy_device(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice) {
  if (!hDevice) {
    return;
  }

  if constexpr (!has_pfnDestroyDeviceCb<WddmDeviceCallbacks>::value) {
    return;
  } else {
    if (!callbacks.pfnDestroyDeviceCb) {
      return;
    }
    using Fn = decltype(callbacks.pfnDestroyDeviceCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    std::remove_pointer_t<ArgPtr> data{};
    data.hDevice = hDevice;
    (void)callbacks.pfnDestroyDeviceCb(&data);
  }
}

HRESULT wddm_create_context(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice, WddmContext* ctxOut) {
  if (!ctxOut) {
    return E_INVALIDARG;
  }

  *ctxOut = WddmContext{};

  if (!hDevice) {
    return E_INVALIDARG;
  }

  // Prefer the v2 CreateContext callback when present (WDDM 1.1+), but fall back
  // to the original entrypoint for older interface versions.
  if constexpr (has_pfnCreateContextCb2<WddmDeviceCallbacks>::value) {
    if (callbacks.pfnCreateContextCb2) {
      using Fn = decltype(callbacks.pfnCreateContextCb2);
      using ArgPtr = typename fn_first_param<Fn>::type;
      std::remove_pointer_t<ArgPtr> data{};
      data.hDevice = hDevice;
      data.NodeOrdinal = 0;
      data.EngineAffinity = 0;
      std::memset(&data.Flags, 0, sizeof(data.Flags));
      data.pPrivateDriverData = nullptr;
      data.PrivateDriverDataSize = 0;

      HRESULT hr = callbacks.pfnCreateContextCb2(&data);
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
  }

  if constexpr (has_pfnCreateContextCb<WddmDeviceCallbacks>::value) {
    if (!callbacks.pfnCreateContextCb) {
      return E_FAIL;
    }

    using Fn = decltype(callbacks.pfnCreateContextCb);
    using ArgPtr = typename fn_first_param<Fn>::type;
    std::remove_pointer_t<ArgPtr> data{};
    data.hDevice = hDevice;
    data.NodeOrdinal = 0;
    data.EngineAffinity = 0;
    std::memset(&data.Flags, 0, sizeof(data.Flags));
    data.pPrivateDriverData = nullptr;
    data.PrivateDriverDataSize = 0;

    HRESULT hr = callbacks.pfnCreateContextCb(&data);
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

  return E_NOTIMPL;
}

#endif

} // namespace aerogpu
