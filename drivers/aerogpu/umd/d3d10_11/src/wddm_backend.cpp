// See wddm_backend.h for details.

#include "wddm_backend.h"

#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <type_traits>
#include <utility>
#include <vector>

#if defined(_WIN32)
  #include <windows.h>
#endif

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dumddi.h>

  #include "../../../protocol/aerogpu_cmd.h"
  #include "../../../protocol/aerogpu_wddm_alloc.h"
  #include "../../../protocol/aerogpu_win7_abi.h"

  #ifndef NT_SUCCESS
    #define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
  #endif

  #ifndef STATUS_TIMEOUT
    #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
  #endif
#endif

namespace aerogpu::wddm {
namespace {

constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au);

#if AEROGPU_D3D10_11_UMD_LOG
void LogV(const char* fmt, va_list args) {
  char buf[2048];
  const int n = vsnprintf(buf, sizeof(buf), fmt, args);
  if (n <= 0) {
    return;
  }
#if defined(_WIN32)
  OutputDebugStringA(buf);
#else
  fputs(buf, stderr);
#endif
}

void Log(const char* fmt, ...) {
  va_list args;
  va_start(args, fmt);
  LogV(fmt, args);
  va_end(args);
}
#else
void Log(const char*, ...) {}
#endif

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param_impl {
  using type = Arg0;
};

template <typename Fn>
struct fn_first_param;

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(__stdcall*)(Arg0, Rest...)> : fn_first_param_impl<Ret, Arg0, Rest...> {};

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(*)(Arg0, Rest...)> : fn_first_param_impl<Ret, Arg0, Rest...> {};

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
struct has_member_pfnRenderCb : std::false_type {};

template <typename T>
struct has_member_pfnRenderCb<T, std::void_t<decltype(std::declval<T>().pfnRenderCb)>> : std::true_type {};

template <typename Fn, typename HandleA, typename HandleB, typename HandleC, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn,
                                 HandleA handle_a,
                                 HandleB handle_b,
                                 HandleC handle_c,
                                 Args&&... args) {
  if constexpr (std::is_invocable_v<Fn, HandleA, Args...>) {
    return fn(handle_a, std::forward<Args>(args)...);
  } else if constexpr (std::is_invocable_v<Fn, HandleB, Args...>) {
    return fn(handle_b, std::forward<Args>(args)...);
  } else if constexpr (std::is_invocable_v<Fn, HandleC, Args...>) {
    return fn(handle_c, std::forward<Args>(args)...);
  } else {
    return fn(std::forward<Args>(args)...);
  }
}

template <typename T, typename = void>
struct has_member_pUMCallbacks : std::false_type {};

template <typename T>
struct has_member_pUMCallbacks<T, std::void_t<decltype(std::declval<T>().pUMCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pCallbacks : std::false_type {};

template <typename T>
struct has_member_pCallbacks<T, std::void_t<decltype(std::declval<T>().pCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDeviceCallbacks : std::false_type {};

template <typename T>
struct has_member_pDeviceCallbacks<T, std::void_t<decltype(std::declval<T>().pDeviceCallbacks)>> : std::true_type {};

template <typename T>
const D3DDDI_DEVICECALLBACKS* GetDdiCallbacks(const T& args) {
  if constexpr (has_member_pUMCallbacks<T>::value) {
    if (args.pUMCallbacks) {
      return args.pUMCallbacks;
    }
  }

  // Some WDK vintages expose the shared callbacks directly as `pCallbacks` or
  // `pDeviceCallbacks` (notably for D3D10). Avoid reinterpreting the D3D11 device
  // callback table (which does not contain the WDDM submission entrypoints) by
  // only accepting pointer types that actually have `pfnRenderCb`.
  if constexpr (has_member_pCallbacks<T>::value) {
    using Ptr = decltype(args.pCallbacks);
    if constexpr (std::is_pointer_v<std::remove_reference_t<Ptr>>) {
      using Elem = std::remove_pointer_t<std::remove_reference_t<Ptr>>;
      if constexpr (has_member_pfnRenderCb<Elem>::value) {
        return reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(args.pCallbacks);
      }
    }
  }
  if constexpr (has_member_pDeviceCallbacks<T>::value) {
    using Ptr = decltype(args.pDeviceCallbacks);
    if constexpr (std::is_pointer_v<std::remove_reference_t<Ptr>>) {
      using Elem = std::remove_pointer_t<std::remove_reference_t<Ptr>>;
      if constexpr (has_member_pfnRenderCb<Elem>::value) {
        return reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(args.pDeviceCallbacks);
      }
    }
  }
  return nullptr;
}

template <typename FlagsT, typename = void>
struct has_member_value : std::false_type {};

template <typename FlagsT>
struct has_member_value<FlagsT, std::void_t<decltype(std::declval<FlagsT&>().Value)>> : std::true_type {};

template <typename FlagsT, typename = void>
struct has_member_write_operation : std::false_type {};

template <typename FlagsT>
struct has_member_write_operation<FlagsT, std::void_t<decltype(std::declval<FlagsT&>().WriteOperation)>> : std::true_type {};

template <typename FlagsT>
void SetWriteOperationFlag(FlagsT& flags, bool write) {
  if constexpr (std::is_integral_v<std::remove_reference_t<FlagsT>>) {
    if (write) {
      flags |= 0x1u;
    } else {
      flags &= ~0x1u;
    }
  } else if constexpr (has_member_value<FlagsT>::value) {
    if (write) {
      flags.Value |= 0x1u;
    } else {
      flags.Value &= ~0x1u;
    }
  } else if constexpr (has_member_write_operation<FlagsT>::value) {
    flags.WriteOperation = write ? 1u : 0u;
  }
}

template <typename EntryT, typename = void>
struct has_member_allocation_list_slot_id : std::false_type {};

template <typename EntryT>
struct has_member_allocation_list_slot_id<EntryT, std::void_t<decltype(std::declval<EntryT&>().AllocationListSlotId)>>
    : std::true_type {};

template <typename EntryT, typename = void>
struct has_member_slot_id : std::false_type {};

template <typename EntryT>
struct has_member_slot_id<EntryT, std::void_t<decltype(std::declval<EntryT&>().SlotId)>> : std::true_type {};

template <typename EntryT>
void SetAllocationListSlotId(EntryT& entry, UINT slot_id) {
  if constexpr (has_member_allocation_list_slot_id<EntryT>::value) {
    entry.AllocationListSlotId = slot_id;
  } else if constexpr (has_member_slot_id<EntryT>::value) {
    entry.SlotId = slot_id;
  } else {
    (void)entry;
    (void)slot_id;
  }
}

template <typename EntryT, typename = void>
struct has_member_flags : std::false_type {};

template <typename EntryT>
struct has_member_flags<EntryT, std::void_t<decltype(std::declval<EntryT&>().Flags)>> : std::true_type {};

template <typename EntryT, typename = void>
struct has_member_value_field : std::false_type {};

template <typename EntryT>
struct has_member_value_field<EntryT, std::void_t<decltype(std::declval<EntryT&>().Value)>> : std::true_type {};

template <typename EntryT, typename = void>
struct has_member_write_operation_field : std::false_type {};

template <typename EntryT>
struct has_member_write_operation_field<EntryT, std::void_t<decltype(std::declval<EntryT&>().WriteOperation)>> : std::true_type {};

template <typename EntryT>
void SetWriteOperation(EntryT& entry, bool write) {
  if constexpr (has_member_value_field<EntryT>::value) {
    SetWriteOperationFlag(entry.Value, write);
  } else if constexpr (has_member_flags<EntryT>::value) {
    SetWriteOperationFlag(entry.Flags, write);
  } else if constexpr (has_member_write_operation_field<EntryT>::value) {
    entry.WriteOperation = write ? 1u : 0u;
  }
}

// Cross-process 64-bit counter used to derive 31-bit alloc_id values. Mirrors
// the D3D9 UMD scheme so shared resources across processes avoid alloc_id
// collisions in the KMD allocation table.
uint64_t AllocateSharedAllocIdToken() {
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalAllocIdCounter";
    HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, sizeof(uint64_t), name);
    if (mapping) {
      void* view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(uint64_t));
      if (view) {
        g_mapping = mapping;
        g_view = view;
      } else {
        CloseHandle(mapping);
      }
    }
  }

  if (!g_view) {
    return 0;
  }

  auto* counter = reinterpret_cast<volatile LONG64*>(g_view);
  LONG64 token = InterlockedIncrement64(counter);
  if (token == 0) {
    token = InterlockedIncrement64(counter);
  }
  return static_cast<uint64_t>(token);
}

uint32_t AllocateAllocId() {
  for (int attempt = 0; attempt < 16; ++attempt) {
    const uint64_t token = AllocateSharedAllocIdToken();
    const uint32_t alloc_id = static_cast<uint32_t>(token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
    if (alloc_id != 0) {
      return alloc_id;
    }
  }
  return 0;
}

template <typename CallbacksT>
void DestroySyncObjectIfPresent(const CallbacksT& callbacks, D3DKMT_HANDLE hSyncObject) {
  if constexpr (!has_pfnDestroySynchronizationObjectCb<CallbacksT>::value) {
    (void)callbacks;
    (void)hSyncObject;
    return;
  }

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

template <typename CallbacksT>
void DestroyContextIfPresent(const CallbacksT& callbacks, D3DKMT_HANDLE hContext) {
  if constexpr (!has_pfnDestroyContextCb<CallbacksT>::value) {
    (void)callbacks;
    (void)hContext;
    return;
  }

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

template <typename CallbacksT>
void DestroyDeviceIfPresent(const CallbacksT& callbacks, D3DKMT_HANDLE hDevice) {
  if constexpr (!has_pfnDestroyDeviceCb<CallbacksT>::value) {
    (void)callbacks;
    (void)hDevice;
    return;
  }

  if (!hDevice || !callbacks.pfnDestroyDeviceCb) {
    return;
  }

  using Fn = decltype(callbacks.pfnDestroyDeviceCb);
  using ArgPtr = typename fn_first_param<Fn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  Arg data{};
  data.hDevice = hDevice;
  (void)callbacks.pfnDestroyDeviceCb(static_cast<ArgPtr>(&data));
}

template <typename CallbacksT>
HRESULT CreateKernelDevice(const CallbacksT& callbacks, void* adapter_handle, D3DKMT_HANDLE* out_device) {
  if (!out_device) {
    return E_INVALIDARG;
  }
  *out_device = 0;

  if (!callbacks.pfnCreateDeviceCb) {
    return E_FAIL;
  }

  using Fn = decltype(callbacks.pfnCreateDeviceCb);
  using ArgPtr = typename fn_first_param<Fn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  Arg data{};
  data.hAdapter = adapter_handle;

  const HRESULT hr = callbacks.pfnCreateDeviceCb(static_cast<ArgPtr>(&data));
  if (FAILED(hr)) {
    return hr;
  }

  *out_device = data.hDevice;
  return (*out_device != 0) ? S_OK : E_FAIL;
}

template <typename CallbacksT, typename FnT>
HRESULT CreateKernelContextCommon(const CallbacksT&, FnT fn, D3DKMT_HANDLE hDevice, D3DKMT_HANDLE* out_ctx, D3DKMT_HANDLE* out_sync) {
  if (!out_ctx || !out_sync) {
    return E_INVALIDARG;
  }
  *out_ctx = 0;
  *out_sync = 0;

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

  *out_ctx = data.hContext;
  *out_sync = data.hSyncObject;
  return (*out_ctx != 0 && *out_sync != 0) ? S_OK : E_FAIL;
}

template <typename CallbacksT>
HRESULT CreateKernelContext(const CallbacksT& callbacks, D3DKMT_HANDLE hDevice, D3DKMT_HANDLE* out_ctx, D3DKMT_HANDLE* out_sync) {
  if constexpr (has_pfnCreateContextCb2<CallbacksT>::value) {
    if (callbacks.pfnCreateContextCb2) {
      return CreateKernelContextCommon(callbacks, callbacks.pfnCreateContextCb2, hDevice, out_ctx, out_sync);
    }
  }

  if constexpr (has_pfnCreateContextCb<CallbacksT>::value) {
    if (callbacks.pfnCreateContextCb) {
      return CreateKernelContextCommon(callbacks, callbacks.pfnCreateContextCb, hDevice, out_ctx, out_sync);
    }
  }

  return E_NOTIMPL;
}

#endif  // _WIN32 && AEROGPU_UMD_USE_WDK_HEADERS

}  // namespace

Backend::~Backend() {
  reset();
}

void Backend::reset() {
  last_submitted_fence_ = 0;
  last_completed_fence_ = 0;

  stub_mutex_ = nullptr;
  stub_cv_ = nullptr;

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (ddi_callbacks_) {
    DestroySyncObjectIfPresent(*ddi_callbacks_, km_sync_object_);
    DestroyContextIfPresent(*ddi_callbacks_, km_context_);
    DestroyDeviceIfPresent(*ddi_callbacks_, km_device_);
  }

  adapter_handle_ = nullptr;
  std::memset(&hrt_device11_, 0, sizeof(hrt_device11_));
  std::memset(&hrt_device10_, 0, sizeof(hrt_device10_));
  ddi_callbacks_ = nullptr;

  km_device_ = 0;
  km_context_ = 0;
  km_sync_object_ = 0;
#endif
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

HRESULT Backend::InitFromD3D10CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE& args) {
  reset();

  adapter_handle_ = hAdapter.pDrvPrivate;
  hrt_device10_ = args.hRTDevice;
  std::memset(&hrt_device11_, 0, sizeof(hrt_device11_));

  ddi_callbacks_ = GetDdiCallbacks(args);
  if (!ddi_callbacks_) {
    Log("aerogpu-d3d10_11: missing D3DDDI callbacks in D3D10 CreateDevice\n");
    return E_FAIL;
  }

  HRESULT hr = CreateKernelDevice(*ddi_callbacks_, adapter_handle_, &km_device_);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: CreateDeviceCb failed hr=0x%08lX\n", (unsigned long)hr);
    reset();
    return hr;
  }

  hr = CreateKernelContext(*ddi_callbacks_, km_device_, &km_context_, &km_sync_object_);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: CreateContextCb failed hr=0x%08lX\n", (unsigned long)hr);
    reset();
    return hr;
  }

  Log("aerogpu-d3d10_11: WDDM init (D3D10) hDevice=%u hContext=%u hSync=%u\n",
      (unsigned)km_device_,
      (unsigned)km_context_,
      (unsigned)km_sync_object_);
  return S_OK;
}

HRESULT Backend::InitFromD3D11CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D11DDIARG_CREATEDEVICE& args) {
  reset();

  adapter_handle_ = hAdapter.pDrvPrivate;
  hrt_device11_ = args.hRTDevice;
  std::memset(&hrt_device10_, 0, sizeof(hrt_device10_));
  constexpr size_t kCopyBytes =
      (sizeof(hrt_device10_) < sizeof(args.hRTDevice)) ? sizeof(hrt_device10_) : sizeof(args.hRTDevice);
  std::memcpy(&hrt_device10_, &args.hRTDevice, kCopyBytes);

  ddi_callbacks_ = GetDdiCallbacks(args);
  if (!ddi_callbacks_) {
    Log("aerogpu-d3d10_11: missing D3DDDI callbacks in D3D11 CreateDevice\n");
    return E_FAIL;
  }

  HRESULT hr = CreateKernelDevice(*ddi_callbacks_, adapter_handle_, &km_device_);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: CreateDeviceCb failed hr=0x%08lX\n", (unsigned long)hr);
    reset();
    return hr;
  }

  hr = CreateKernelContext(*ddi_callbacks_, km_device_, &km_context_, &km_sync_object_);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: CreateContextCb failed hr=0x%08lX\n", (unsigned long)hr);
    reset();
    return hr;
  }

  Log("aerogpu-d3d10_11: WDDM init (D3D11) hDevice=%u hContext=%u hSync=%u\n",
      (unsigned)km_device_,
      (unsigned)km_context_,
      (unsigned)km_sync_object_);
  return S_OK;
}

#endif  // _WIN32 && AEROGPU_UMD_USE_WDK_HEADERS

HRESULT Backend::SubmitRender(const void* cmd,
                              size_t cmd_size,
                              const SubmissionAlloc* allocs,
                              size_t alloc_count,
                              uint64_t* fence_out) {
  return SubmitInternal(false, cmd, cmd_size, allocs, alloc_count, fence_out);
}

HRESULT Backend::SubmitPresent(const void* cmd,
                               size_t cmd_size,
                               const SubmissionAlloc* allocs,
                               size_t alloc_count,
                               uint64_t* fence_out) {
  return SubmitInternal(true, cmd, cmd_size, allocs, alloc_count, fence_out);
}

HRESULT Backend::WaitForFence(uint64_t fence_value, uint32_t timeout_ms) {
  if (fence_value == 0) {
    return S_OK;
  }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (!ddi_callbacks_ || !km_sync_object_ || !km_context_) {
    return E_FAIL;
  }

  // Prefer the runtime callback when present.
  if (ddi_callbacks_->pfnWaitForSynchronizationObjectCb) {
    const D3DKMT_HANDLE handles[1] = {km_sync_object_};
    const UINT64 fence_values[1] = {fence_value};

    D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT wait{};
    __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
      wait.hContext = km_context_;
    }
    wait.ObjectCount = 1;
    wait.ObjectHandleArray = handles;
    wait.FenceValueArray = fence_values;

    const UINT64 infinite = ~0ull;
    const UINT64 ms = (timeout_ms == INFINITE) ? infinite : static_cast<UINT64>(timeout_ms);
    wait.Timeout = ms;

    const void* hrt_ptr = nullptr;
    __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
      hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
    }

    const HRESULT hr = CallCbMaybeHandle(ddi_callbacks_->pfnWaitForSynchronizationObjectCb,
                                         hrt_device11_,
                                         hrt_device10_,
                                         reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                                         &wait);
    if (hr == kDxgiErrorWasStillDrawing) {
      return hr;
    }
    if (FAILED(hr)) {
      Log("aerogpu-d3d10_11: WaitForSynchronizationObjectCb failed hr=0x%08lX (fence=%llu)\n",
          (unsigned long)hr,
          (unsigned long long)fence_value);
      return hr;
    }

    last_completed_fence_ = std::max(last_completed_fence_, fence_value);
    return S_OK;
  }

  // Fallback: direct kernel thunk.
  const D3DKMT_HANDLE handles[1] = {km_sync_object_};
  const UINT64 fence_values[1] = {fence_value};

  D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
  args.ObjectCount = 1;
  args.ObjectHandleArray = handles;
  args.FenceValueArray = fence_values;

  const UINT64 infinite = ~0ull;
  const UINT64 ms = (timeout_ms == INFINITE) ? infinite : static_cast<UINT64>(timeout_ms);
  args.Timeout = ms;

  const NTSTATUS st = D3DKMTWaitForSynchronizationObject(&args);
  if (st == STATUS_TIMEOUT) {
    return kDxgiErrorWasStillDrawing;
  }
  if (!NT_SUCCESS(st)) {
    Log("aerogpu-d3d10_11: D3DKMTWaitForSynchronizationObject failed st=0x%08lX (fence=%llu)\n",
        (unsigned long)st,
        (unsigned long long)fence_value);
    return E_FAIL;
  }

  last_completed_fence_ = std::max(last_completed_fence_, fence_value);
  return S_OK;
#else
  // Stub: treat all work as completed immediately.
  if (!stub_mutex_ || !stub_cv_) {
    static std::mutex m;
    static std::condition_variable cv;
    stub_mutex_ = &m;
    stub_cv_ = &cv;
  }

  std::unique_lock<std::mutex> lock(*stub_mutex_);
  if (last_completed_fence_ < fence_value) {
    stub_cv_->wait(lock, [&] { return last_completed_fence_ >= fence_value; });
  }
  return S_OK;
#endif
}

#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)

HRESULT Backend::CreateAllocation(uint64_t size_bytes, AllocationHandle* out_handle) {
  if (!out_handle) {
    return E_INVALIDARG;
  }
  *out_handle = 0;
  static std::atomic<uint32_t> next_handle{1};
  *out_handle = next_handle.fetch_add(1);
  (void)size_bytes;
  return S_OK;
}

HRESULT Backend::DestroyAllocation(AllocationHandle handle) {
  (void)handle;
  return S_OK;
}

#endif

HRESULT Backend::LockAllocation(AllocationHandle handle,
                                uint64_t offset_bytes,
                                uint64_t size_bytes,
                                bool read_only,
                                bool do_not_wait,
                                bool discard,
                                bool no_overwrite,
                                LockedRange* out) {
  if (!handle || !out) {
    return E_INVALIDARG;
  }
  *out = {};

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (!ddi_callbacks_ || !ddi_callbacks_->pfnLockCb) {
    return E_FAIL;
  }

  D3DDDICB_LOCK lock{};
  lock.hAllocation = static_cast<D3DKMT_HANDLE>(handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock.SubresourceIndex = 0;
  }

  // Translate into lock flags. Member names vary by WDK version.
  __if_exists(D3DDDICB_LOCK::Flags) {
    if (read_only) {
      __if_exists(decltype(lock.Flags)::ReadOnly) { lock.Flags.ReadOnly = 1; }
    } else {
      __if_exists(decltype(lock.Flags)::ReadOnly) { lock.Flags.ReadOnly = 0; }
      __if_exists(decltype(lock.Flags)::WriteOnly) { lock.Flags.WriteOnly = 1; }
      __if_exists(decltype(lock.Flags)::Write) { lock.Flags.Write = 1; }
    }

    if (discard) {
      __if_exists(decltype(lock.Flags)::Discard) { lock.Flags.Discard = 1; }
    }
    if (no_overwrite) {
      __if_exists(decltype(lock.Flags)::NoOverwrite) { lock.Flags.NoOverwrite = 1; }
    }

    if (do_not_wait) {
      __if_exists(decltype(lock.Flags)::DoNotWait) { lock.Flags.DoNotWait = 1; }
      __if_exists(decltype(lock.Flags)::DonotWait) { lock.Flags.DonotWait = 1; }
    }
  }

  const void* hrt_ptr = nullptr;
  __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
    hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
  }

  const HRESULT hr =
      CallCbMaybeHandle(ddi_callbacks_->pfnLockCb, hrt_device11_, hrt_device10_, reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)), &lock);
  if (hr == kDxgiErrorWasStillDrawing) {
    return hr;
  }
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: LockCb failed hr=0x%08lX\n", (unsigned long)hr);
    return hr;
  }

  void* base = nullptr;
  __if_exists(D3DDDICB_LOCK::pData) { base = lock.pData; }
  if (!base) {
    return E_FAIL;
  }

  out->data = static_cast<uint8_t*>(base) + static_cast<size_t>(offset_bytes);
  (void)size_bytes;

  __if_exists(D3DDDICB_LOCK::Pitch) { out->row_pitch = lock.Pitch; }
  __if_exists(D3DDDICB_LOCK::SlicePitch) { out->depth_pitch = lock.SlicePitch; }
  return S_OK;
#else
  (void)offset_bytes;
  (void)size_bytes;
  (void)read_only;
  (void)do_not_wait;
  (void)discard;
  (void)no_overwrite;
  out->data = nullptr;
  return S_OK;
#endif
}

HRESULT Backend::UnlockAllocation(AllocationHandle handle) {
  if (!handle) {
    return S_OK;
  }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (!ddi_callbacks_ || !ddi_callbacks_->pfnUnlockCb) {
    return E_FAIL;
  }

  D3DDDICB_UNLOCK unlock{};
  unlock.hAllocation = static_cast<D3DKMT_HANDLE>(handle);
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
    unlock.SubresourceIndex = 0;
  }

  const void* hrt_ptr = nullptr;
  __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
    hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
  }

  const HRESULT hr = CallCbMaybeHandle(ddi_callbacks_->pfnUnlockCb,
                                       hrt_device11_,
                                       hrt_device10_,
                                       reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                                       &unlock);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: UnlockCb failed hr=0x%08lX\n", (unsigned long)hr);
  }
  return hr;
#else
  return S_OK;
#endif
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

HRESULT Backend::CreateAllocation(D3D10DDI_HRTRESOURCE hrt_resource,
                                  const AllocationDesc& desc,
                                  AllocationHandle* out_handle,
                                  KernelHandle* out_km_resource,
                                  uint32_t* out_alloc_id,
                                  uint64_t* out_share_token,
                                  HANDLE* out_shared_handle) {
  if (!out_handle || !out_km_resource || !out_alloc_id || !out_share_token || !out_shared_handle) {
    return E_INVALIDARG;
  }

  *out_handle = 0;
  *out_km_resource = 0;
  *out_alloc_id = 0;
  *out_share_token = 0;
  *out_shared_handle = nullptr;

  if (!ddi_callbacks_ || !ddi_callbacks_->pfnAllocateCb) {
    return E_FAIL;
  }
  if (desc.size_bytes == 0) {
    return E_INVALIDARG;
  }

  const uint32_t alloc_id = AllocateAllocId();
  if (!alloc_id) {
    Log("aerogpu-d3d10_11: failed to allocate alloc_id\n");
    return E_FAIL;
  }

  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = alloc_id;
  priv.flags = desc.shared ? AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED : 0;
  priv.share_token = desc.shared ? static_cast<uint64_t>(alloc_id) : 0;
  priv.size_bytes = desc.size_bytes;
  priv.reserved0 = 0;

  D3DDDI_ALLOCATIONINFO alloc_info{};
  std::memset(&alloc_info, 0, sizeof(alloc_info));
  alloc_info.Size = desc.size_bytes;
  alloc_info.Alignment = 0;
  alloc_info.pPrivateDriverData = &priv;
  alloc_info.PrivateDriverDataSize = sizeof(priv);

  if (desc.primary) {
    __if_exists(decltype(alloc_info.Flags)::Primary) { alloc_info.Flags.Primary = 1; }
  }
  if (desc.cpu_visible) {
    __if_exists(decltype(alloc_info.Flags)::CpuVisible) { alloc_info.Flags.CpuVisible = 1; }
  }
  if (desc.render_target) {
    __if_exists(decltype(alloc_info.Flags)::RenderTarget) { alloc_info.Flags.RenderTarget = 1; }
  }

  D3DDDICB_ALLOCATE alloc{};
  std::memset(&alloc, 0, sizeof(alloc));
  __if_exists(D3DDDICB_ALLOCATE::hResource) {
    // Copy the runtime resource handle bits into the callback struct. Handle
    // wrapper types are pointer-sized; memcpy avoids header-version mismatch.
    std::memset(&alloc.hResource, 0, sizeof(alloc.hResource));
    constexpr size_t kCopy = (sizeof(alloc.hResource) < sizeof(hrt_resource)) ? sizeof(alloc.hResource) : sizeof(hrt_resource);
    std::memcpy(&alloc.hResource, &hrt_resource, kCopy);
  }
  __if_exists(D3DDDICB_ALLOCATE::NumAllocations) { alloc.NumAllocations = 1; }
  __if_exists(D3DDDICB_ALLOCATE::pAllocationInfo) { alloc.pAllocationInfo = &alloc_info; }

  __if_exists(D3DDDICB_ALLOCATE::Flags) {
    if (desc.primary) {
      __if_exists(decltype(alloc.Flags)::Primary) { alloc.Flags.Primary = 1; }
    }
    if (desc.shared) {
      __if_exists(decltype(alloc.Flags)::CreateShared) { alloc.Flags.CreateShared = 1; }
    }
  }

  const void* hrt_ptr = nullptr;
  __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
    hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
  }

  const HRESULT hr = CallCbMaybeHandle(ddi_callbacks_->pfnAllocateCb,
                                       hrt_device11_,
                                       hrt_device10_,
                                       reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                                       &alloc);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: AllocateCb(resource) failed hr=0x%08lX\n", (unsigned long)hr);
    return hr;
  }

  const D3DKMT_HANDLE hAlloc = alloc_info.hAllocation;
  if (!hAlloc) {
    return E_FAIL;
  }

  KernelHandle km_resource = 0;
  __if_exists(D3DDDICB_ALLOCATE::hKMResource) { km_resource = static_cast<KernelHandle>(alloc.hKMResource); }

  HANDLE shared_handle = nullptr;
  __if_exists(D3DDDICB_ALLOCATE::hSection) { shared_handle = alloc.hSection; }

  *out_handle = static_cast<AllocationHandle>(hAlloc);
  *out_km_resource = km_resource;
  *out_alloc_id = alloc_id;
  *out_share_token = priv.share_token;
  *out_shared_handle = shared_handle;
  return S_OK;
}

HRESULT Backend::DestroyAllocation(D3D10DDI_HRTRESOURCE hrt_resource, KernelHandle km_resource, AllocationHandle handle) {
  if (!handle) {
    return S_OK;
  }
  if (!ddi_callbacks_ || !ddi_callbacks_->pfnDeallocateCb) {
    return E_FAIL;
  }

  const D3DKMT_HANDLE hAlloc = static_cast<D3DKMT_HANDLE>(handle);

  D3DDDICB_DEALLOCATE dealloc{};
  std::memset(&dealloc, 0, sizeof(dealloc));
  __if_exists(D3DDDICB_DEALLOCATE::hResource) {
    std::memset(&dealloc.hResource, 0, sizeof(dealloc.hResource));
    constexpr size_t kCopy = (sizeof(dealloc.hResource) < sizeof(hrt_resource)) ? sizeof(dealloc.hResource) : sizeof(hrt_resource);
    std::memcpy(&dealloc.hResource, &hrt_resource, kCopy);
  }
  __if_exists(D3DDDICB_DEALLOCATE::hKMResource) { dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(km_resource); }
  __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) { dealloc.NumAllocations = 1; }
  __if_exists(D3DDDICB_DEALLOCATE::phAllocations) { dealloc.phAllocations = &hAlloc; }

  const void* hrt_ptr = nullptr;
  __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
    hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
  }

  const HRESULT hr = CallCbMaybeHandle(ddi_callbacks_->pfnDeallocateCb,
                                       hrt_device11_,
                                       hrt_device10_,
                                       reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                                       &dealloc);
  if (FAILED(hr)) {
    Log("aerogpu-d3d10_11: DeallocateCb(resource) failed hr=0x%08lX\n", (unsigned long)hr);
  }
  return hr;
}

#endif

HRESULT Backend::SubmitInternal(bool want_present,
                                const void* cmd,
                                size_t cmd_size,
                                const SubmissionAlloc* allocs,
                                size_t alloc_count,
                                uint64_t* fence_out) {
  if (fence_out) {
    *fence_out = 0;
  }
  if (!cmd || cmd_size == 0) {
    return S_OK;
  }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (!ddi_callbacks_ || !ddi_callbacks_->pfnAllocateCb || !ddi_callbacks_->pfnRenderCb || !ddi_callbacks_->pfnDeallocateCb) {
    Log("aerogpu-d3d10_11: missing submission callbacks\n");
    return E_FAIL;
  }
  if (!km_context_) {
    Log("aerogpu-d3d10_11: Submit without a kernel context\n");
    return E_FAIL;
  }

  const uint8_t* src = static_cast<const uint8_t*>(cmd);
  const size_t src_size = cmd_size;
  if (src_size < sizeof(aerogpu_cmd_stream_header)) {
    return E_INVALIDARG;
  }

  std::vector<D3DDDI_ALLOCATIONLIST> allocation_list;
  allocation_list.reserve(alloc_count);

  for (size_t i = 0; i < alloc_count; ++i) {
    const auto hAlloc = static_cast<D3DKMT_HANDLE>(allocs[i].hAllocation);
    if (!hAlloc) {
      continue;
    }

    bool found = false;
    for (auto& entry : allocation_list) {
      if (entry.hAllocation == hAlloc) {
        if (allocs[i].write) {
          SetWriteOperation(entry, true);
        }
        found = true;
        break;
      }
    }
    if (found) {
      continue;
    }

    D3DDDI_ALLOCATIONLIST entry{};
    std::memset(&entry, 0, sizeof(entry));
    entry.hAllocation = hAlloc;
    SetWriteOperation(entry, allocs[i].write);
    SetAllocationListSlotId(entry, static_cast<UINT>(allocation_list.size()));
    allocation_list.push_back(entry);
  }

  uint64_t last_fence = 0;

  const void* hrt_ptr = nullptr;
  __if_exists(D3D11DDI_HRTDEVICE::pDrvPrivate) {
    hrt_ptr = hrt_device11_.pDrvPrivate ? hrt_device11_.pDrvPrivate : hrt_device10_.pDrvPrivate;
  }

  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;
    const UINT request_bytes = static_cast<UINT>(remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header));

    D3DDDICB_ALLOCATE alloc{};
    std::memset(&alloc, 0, sizeof(alloc));
    __if_exists(D3DDDICB_ALLOCATE::hContext) { alloc.hContext = km_context_; }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) { alloc.DmaBufferSize = request_bytes; }
    __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) { alloc.CommandBufferSize = request_bytes; }
    __if_exists(D3DDDICB_ALLOCATE::AllocationListSize) { alloc.AllocationListSize = static_cast<UINT>(allocation_list.size()); }
    __if_exists(D3DDDICB_ALLOCATE::PatchLocationListSize) { alloc.PatchLocationListSize = 0; }

    const HRESULT alloc_hr =
        CallCbMaybeHandle(ddi_callbacks_->pfnAllocateCb,
                          hrt_device11_,
                          hrt_device10_,
                          reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                          &alloc);

    void* dma_ptr = nullptr;
    UINT dma_cap = 0;
    __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) { dma_ptr = alloc.pDmaBuffer; }
    __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) { dma_ptr = alloc.pCommandBuffer; }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) { dma_cap = alloc.DmaBufferSize; }
    __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) { dma_cap = alloc.CommandBufferSize; }

    void* priv_ptr = nullptr;
    UINT priv_size = 0;
    __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) { priv_ptr = alloc.pDmaBufferPrivateData; }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) { priv_size = alloc.DmaBufferPrivateDataSize; }

    const UINT list_cap = [&]() -> UINT {
      UINT cap = 0;
      __if_exists(D3DDDICB_ALLOCATE::AllocationListSize) { cap = alloc.AllocationListSize; }
      return cap;
    }();

    if (FAILED(alloc_hr) || !dma_ptr || dma_cap == 0) {
      Log("aerogpu-d3d10_11: AllocateCb(DMA) failed hr=0x%08lX\n", (unsigned long)alloc_hr);
      return FAILED(alloc_hr) ? alloc_hr : E_OUTOFMEMORY;
    }

    if (!priv_ptr || priv_size < AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
      Log("aerogpu-d3d10_11: AllocateCb did not provide pDmaBufferPrivateData (ptr=%p size=%u)\n", priv_ptr, priv_size);
      return E_FAIL;
    }

    if (!allocation_list.empty()) {
      __if_exists(D3DDDICB_ALLOCATE::pAllocationList) {
        if (alloc.pAllocationList) {
          if (list_cap < allocation_list.size()) {
            Log("aerogpu-d3d10_11: runtime allocation list too small (cap=%u need=%zu)\n",
                (unsigned)list_cap,
                allocation_list.size());
            return E_OUTOFMEMORY;
          }
          std::memcpy(alloc.pAllocationList, allocation_list.data(), allocation_list.size() * sizeof(D3DDDI_ALLOCATIONLIST));
        }
      }
    }

    const size_t dma_cap_bytes = static_cast<size_t>(dma_cap);
    if (dma_cap_bytes < sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_hdr)) {
      return E_OUTOFMEMORY;
    }

    // Select as many packets as will fit.
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
      if (chunk_size + pkt_size > dma_cap_bytes) {
        break;
      }
      chunk_end += pkt_size;
      chunk_size += pkt_size;
    }

    if (chunk_end == chunk_begin) {
      return E_OUTOFMEMORY;
    }

    // Copy header + chosen packets.
    auto* dst = static_cast<uint8_t*>(dma_ptr);
    std::memcpy(dst, src, sizeof(aerogpu_cmd_stream_header));
    std::memcpy(dst + sizeof(aerogpu_cmd_stream_header), src + chunk_begin, chunk_size - sizeof(aerogpu_cmd_stream_header));
    auto* hdr = reinterpret_cast<aerogpu_cmd_stream_header*>(dst);
    hdr->size_bytes = static_cast<uint32_t>(chunk_size);

    const bool is_last_chunk = (chunk_end == src_size);
    const bool do_present = want_present && is_last_chunk && ddi_callbacks_->pfnPresentCb != nullptr;

    HRESULT submit_hr = S_OK;
    uint64_t submission_fence = 0;

    if (do_present) {
      D3DDDICB_PRESENT pres{};
      std::memset(&pres, 0, sizeof(pres));
      __if_exists(D3DDDICB_PRESENT::hContext) { pres.hContext = km_context_; }
      __if_exists(D3DDDICB_PRESENT::pDmaBuffer) { pres.pDmaBuffer = dma_ptr; }
      __if_exists(D3DDDICB_PRESENT::pCommandBuffer) { pres.pCommandBuffer = dma_ptr; }
      __if_exists(D3DDDICB_PRESENT::DmaBufferSize) { pres.DmaBufferSize = static_cast<UINT>(chunk_size); }
      __if_exists(D3DDDICB_PRESENT::CommandLength) { pres.CommandLength = static_cast<UINT>(chunk_size); }
      __if_exists(D3DDDICB_PRESENT::pAllocationList) { pres.pAllocationList = alloc.pAllocationList; }
      __if_exists(D3DDDICB_PRESENT::AllocationListSize) { pres.AllocationListSize = static_cast<UINT>(allocation_list.size()); }
      __if_exists(D3DDDICB_PRESENT::pPatchLocationList) { pres.pPatchLocationList = alloc.pPatchLocationList; }
      __if_exists(D3DDDICB_PRESENT::PatchLocationListSize) { pres.PatchLocationListSize = 0; }
      __if_exists(D3DDDICB_PRESENT::pDmaBufferPrivateData) { pres.pDmaBufferPrivateData = priv_ptr; }
      __if_exists(D3DDDICB_PRESENT::DmaBufferPrivateDataSize) { pres.DmaBufferPrivateDataSize = priv_size; }

      submit_hr =
          CallCbMaybeHandle(ddi_callbacks_->pfnPresentCb,
                            hrt_device11_,
                            hrt_device10_,
                            reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                            &pres);
      __if_exists(D3DDDICB_PRESENT::NewFenceValue) { submission_fence = static_cast<uint64_t>(pres.NewFenceValue); }
      __if_exists(D3DDDICB_PRESENT::SubmissionFenceId) { submission_fence = static_cast<uint64_t>(pres.SubmissionFenceId); }
    } else {
      D3DDDICB_RENDER render{};
      std::memset(&render, 0, sizeof(render));
      __if_exists(D3DDDICB_RENDER::hContext) { render.hContext = km_context_; }
      __if_exists(D3DDDICB_RENDER::pDmaBuffer) { render.pDmaBuffer = dma_ptr; }
      __if_exists(D3DDDICB_RENDER::pCommandBuffer) { render.pCommandBuffer = dma_ptr; }
      __if_exists(D3DDDICB_RENDER::DmaBufferSize) { render.DmaBufferSize = static_cast<UINT>(chunk_size); }
      __if_exists(D3DDDICB_RENDER::CommandLength) { render.CommandLength = static_cast<UINT>(chunk_size); }
      __if_exists(D3DDDICB_RENDER::pAllocationList) { render.pAllocationList = alloc.pAllocationList; }
      __if_exists(D3DDDICB_RENDER::AllocationListSize) { render.AllocationListSize = static_cast<UINT>(allocation_list.size()); }
      __if_exists(D3DDDICB_RENDER::pPatchLocationList) { render.pPatchLocationList = alloc.pPatchLocationList; }
      __if_exists(D3DDDICB_RENDER::PatchLocationListSize) { render.PatchLocationListSize = 0; }
      __if_exists(D3DDDICB_RENDER::pDmaBufferPrivateData) { render.pDmaBufferPrivateData = priv_ptr; }
      __if_exists(D3DDDICB_RENDER::DmaBufferPrivateDataSize) { render.DmaBufferPrivateDataSize = priv_size; }

      submit_hr =
          CallCbMaybeHandle(ddi_callbacks_->pfnRenderCb,
                            hrt_device11_,
                            hrt_device10_,
                            reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                            &render);
      __if_exists(D3DDDICB_RENDER::NewFenceValue) { submission_fence = static_cast<uint64_t>(render.NewFenceValue); }
      __if_exists(D3DDDICB_RENDER::SubmissionFenceId) { submission_fence = static_cast<uint64_t>(render.SubmissionFenceId); }
    }

    // Free buffers regardless of submission success.
    {
      D3DDDICB_DEALLOCATE dealloc{};
      std::memset(&dealloc, 0, sizeof(dealloc));
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) { dealloc.pDmaBuffer = dma_ptr; }
      __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) { dealloc.pCommandBuffer = dma_ptr; }
      __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) { dealloc.pAllocationList = alloc.pAllocationList; }
      __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) { dealloc.pPatchLocationList = alloc.pPatchLocationList; }
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) { dealloc.pDmaBufferPrivateData = priv_ptr; }
      (void)CallCbMaybeHandle(ddi_callbacks_->pfnDeallocateCb,
                              hrt_device11_,
                              hrt_device10_,
                              reinterpret_cast<HANDLE>(const_cast<void*>(hrt_ptr)),
                              &dealloc);
    }

    if (FAILED(submit_hr)) {
      Log("aerogpu-d3d10_11: %sCb failed hr=0x%08lX\n", do_present ? "Present" : "Render", (unsigned long)submit_hr);
      return submit_hr;
    }

    if (submission_fence != 0) {
      last_fence = submission_fence;
    }

    cur = chunk_end;
  }

  if (last_fence != 0) {
    last_submitted_fence_ = std::max(last_submitted_fence_, last_fence);
    if (fence_out) {
      *fence_out = last_fence;
    }
  }

  Log("aerogpu-d3d10_11: submit %s cmd_bytes=%zu allocs=%zu fence=%llu\n",
      want_present ? "present" : "render",
      src_size,
      allocation_list.size(),
      static_cast<unsigned long long>(last_fence));
  return S_OK;
#else
  // Stub: advance fence immediately and signal.
  if (!stub_mutex_ || !stub_cv_) {
    static std::mutex m;
    static std::condition_variable cv;
    stub_mutex_ = &m;
    stub_cv_ = &cv;
  }

  std::lock_guard<std::mutex> lock(*stub_mutex_);
  const uint64_t fence = last_submitted_fence_ + 1;
  last_submitted_fence_ = fence;
  last_completed_fence_ = fence;
  if (fence_out) {
    *fence_out = fence;
  }
  stub_cv_->notify_all();
  (void)want_present;
  (void)allocs;
  (void)alloc_count;
  return S_OK;
#endif
}

}  // namespace aerogpu::wddm
