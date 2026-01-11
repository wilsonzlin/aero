#include "../include/aerogpu_d3d9_umd.h"

#include <array>
#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstring>
#include <cwchar>
#include <initializer_list>
#include <memory>
#include <thread>
#include <type_traits>
#include <unordered_map>
#include <utility>

#if defined(_WIN32)
  #include <d3d9types.h>
#endif

#ifndef D3DVS_VERSION
  #define D3DVS_VERSION(major, minor) (0xFFFE0000u | ((major) << 8) | (minor))
#endif

#ifndef D3DPS_VERSION
  #define D3DPS_VERSION(major, minor) (0xFFFF0000u | ((major) << 8) | (minor))
#endif

#include "aerogpu_d3d9_caps.h"
#include "aerogpu_d3d9_blit.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_submit.h"
#include "aerogpu_d3d9_wdk_abi_asserts.h"
#include "aerogpu_win7_abi.h"
#include "aerogpu_log.h"
#include "aerogpu_alloc.h"
#include "aerogpu_trace.h"

namespace {

template <typename T, typename = void>
struct has_interface_version_member : std::false_type {};

template <typename T>
struct has_interface_version_member<T, std::void_t<decltype(std::declval<T>().InterfaceVersion)>> : std::true_type {};

template <typename T>
UINT get_interface_version(const T* open) {
  if (!open) {
    return 0;
  }
  if constexpr (has_interface_version_member<T>::value) {
    return open->InterfaceVersion;
  }
  return open->Interface;
}

template <typename T, typename = void>
struct has_adapter_callbacks2_member : std::false_type {};

template <typename T>
struct has_adapter_callbacks2_member<T, std::void_t<decltype(std::declval<T>().pAdapterCallbacks2)>> : std::true_type {};

template <typename T>
D3DDDI_ADAPTERCALLBACKS2* get_adapter_callbacks2(T* open) {
  if (!open) {
    return nullptr;
  }
  if constexpr (has_adapter_callbacks2_member<T>::value) {
    return open->pAdapterCallbacks2;
  }
  return nullptr;
}

template <typename T, typename = void>
struct has_vid_pn_source_id_member : std::false_type {};

template <typename T>
struct has_vid_pn_source_id_member<T, std::void_t<decltype(std::declval<T>().VidPnSourceId)>> : std::true_type {};

template <typename T>
void set_vid_pn_source_id(T* open, UINT vid_pn_source_id) {
  if (!open) {
    return;
  }
  if constexpr (has_vid_pn_source_id_member<T>::value) {
    open->VidPnSourceId = vid_pn_source_id;
  } else {
    (void)vid_pn_source_id;
  }
}

} // namespace

namespace aerogpu {
namespace {

#define AEROGPU_D3D9_STUB_LOG_ONCE()                 \
  do {                                               \
    static std::once_flag aerogpu_once;              \
    const char* fn = __func__;                       \
    std::call_once(aerogpu_once, [fn] {              \
      aerogpu::logf("aerogpu-d3d9: stub %s\n", fn);  \
    });                                              \
  } while (0)

constexpr int32_t kMinGpuThreadPriority = -7;
constexpr int32_t kMaxGpuThreadPriority = 7;

// D3DERR_INVALIDCALL (0x8876086C) is returned by the UMD for invalid arguments.
constexpr HRESULT kD3DErrInvalidCall = 0x8876086CUL;

// S_PRESENT_OCCLUDED (0x08760868) is returned by CheckDeviceState/PresentEx when
// the target window is occluded/minimized. Prefer the SDK macro when available
// but provide a fallback so repo builds don't need d3d9.h.
#if defined(S_PRESENT_OCCLUDED)
constexpr HRESULT kSPresentOccluded = S_PRESENT_OCCLUDED;
#else
constexpr HRESULT kSPresentOccluded = 0x08760868L;
#endif

// D3D9 API/UMD query constants (numeric values from d3d9types.h).
constexpr uint32_t kD3DQueryTypeEvent = 8u;
constexpr uint32_t kD3DIssueEnd = 0x1u;
constexpr uint32_t kD3DIssueBegin = 0x2u;
constexpr uint32_t kD3DGetDataFlush = 0x1u;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// D3DPRESENT_* flags (numeric values from d3d9.h). We only need DONOTWAIT for
// max-frame-latency throttling.
constexpr uint32_t kD3dPresentDoNotWait = 0x00000001u; // D3DPRESENT_DONOTWAIT
constexpr uint32_t kD3dPresentIntervalImmediate = 0x80000000u; // D3DPRESENT_INTERVAL_IMMEDIATE

// D3DERR_WASSTILLDRAWING (0x8876021C). Returned by PresentEx when DONOTWAIT is
// specified and the present is throttled.
constexpr HRESULT kD3dErrWasStillDrawing = static_cast<HRESULT>(-2005532132);

constexpr uint32_t kMaxFrameLatencyMin = 1;
constexpr uint32_t kMaxFrameLatencyMax = 16;

// Bounded wait for PresentEx throttling. This must be finite to avoid hangs in
// DWM/PresentEx call sites if the GPU stops making forward progress.
constexpr uint32_t kPresentThrottleMaxWaitMs = 100;

// Some WDDM/D3D9 callback structs may not expose `SubmissionFenceId`/`NewFenceValue`
// depending on the WDK header vintage. When the runtime does not provide a
// per-submission fence value via the callback out-params, we fall back to
// querying the AeroGPU KMD fence counters via D3DKMTEscape so we still return a
// real fence value for the submission.

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
// Some D3D9 UMD DDI members vary across WDK header vintages. Use compile-time
// detection (SFINAE) so the UMD can populate as many entrypoints as possible
// without hard-failing compilation when a member is absent.
//
// This mirrors the approach in `tools/wdk_abi_probe/`.
#define AEROGPU_DEFINE_HAS_MEMBER(member)                                                      \
  template <typename T, typename = void>                                                       \
  struct aerogpu_has_member_##member : std::false_type {};                                      \
  template <typename T>                                                                        \
  struct aerogpu_has_member_##member<T, std::void_t<decltype(&T::member)>> : std::true_type {}

AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource);
AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource2);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetFVF);
AEROGPU_DEFINE_HAS_MEMBER(pfnBeginScene);
AEROGPU_DEFINE_HAS_MEMBER(pfnEndScene);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawPrimitive2);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawIndexedPrimitive2);
AEROGPU_DEFINE_HAS_MEMBER(pfnWaitForVBlank);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetGPUThreadPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetGPUThreadPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnCheckResourceResidency);
AEROGPU_DEFINE_HAS_MEMBER(pfnQueryResourceResidency);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetDisplayModeEx);
AEROGPU_DEFINE_HAS_MEMBER(pfnComposeRects);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawPrimitiveUP);

// Fixed function / legacy state paths (commonly hit by DWM + simple D3D9 apps).
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTextureStageState);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTransform);
AEROGPU_DEFINE_HAS_MEMBER(pfnMultiplyTransform);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetClipPlane);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetShaderConstI);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetShaderConstB);

#undef AEROGPU_DEFINE_HAS_MEMBER
#endif

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
template <typename T, typename = void>
struct aerogpu_has_member_pDrvPrivate : std::false_type {};

template <typename T>
struct aerogpu_has_member_pDrvPrivate<T, std::void_t<decltype(std::declval<T>().pDrvPrivate)>> : std::true_type {};

template <typename T>
uint64_t d3d9_stub_trace_arg(const T& v) {
  if constexpr (aerogpu_has_member_pDrvPrivate<T>::value) {
    return d3d9_trace_arg_ptr(v.pDrvPrivate);
  } else if constexpr (std::is_pointer_v<T>) {
    return d3d9_trace_arg_ptr(v);
  } else if constexpr (std::is_enum_v<T>) {
    using Under = std::underlying_type_t<T>;
    return static_cast<uint64_t>(static_cast<Under>(v));
  } else if constexpr (std::is_integral_v<T>) {
    return static_cast<uint64_t>(v);
  } else {
    return 0;
  }
}

template <typename... Args>
std::array<uint64_t, 4> d3d9_stub_trace_args(const Args&... args) {
  std::array<uint64_t, 4> out{};
  size_t i = 0;
  (void)std::initializer_list<int>{
      (i < out.size() ? (out[i++] = d3d9_stub_trace_arg(args), 0) : 0)...};
  return out;
}

#define AEROGPU_D3D9_DEFINE_DDI_STUB(member, trace_func, stub_hr)                                \
  template <typename Fn>                                                                         \
  struct aerogpu_d3d9_stub_##member;                                                             \
  template <typename Ret, typename... Args>                                                      \
  struct aerogpu_d3d9_stub_##member<Ret(__stdcall*)(Args...)> {                                  \
    static Ret __stdcall member(Args... args) {                                                   \
      AEROGPU_D3D9_STUB_LOG_ONCE();                                                              \
      const auto packed = d3d9_stub_trace_args(args...);                                         \
      D3d9TraceCall trace(trace_func, packed[0], packed[1], packed[2], packed[3]);               \
      if constexpr (std::is_same_v<Ret, void>) {                                                  \
        (void)trace.ret(stub_hr);                                                                 \
        return;                                                                                   \
      }                                                                                           \
      if constexpr (std::is_same_v<Ret, HRESULT>) {                                               \
        return trace.ret(stub_hr);                                                                \
      }                                                                                           \
      (void)trace.ret(stub_hr);                                                                   \
      return Ret{};                                                                               \
    }                                                                                             \
  };                                                                                              \
  template <typename Ret, typename... Args>                                                      \
  struct aerogpu_d3d9_stub_##member<Ret(*)(Args...)> {                                            \
    static Ret member(Args... args) {                                                             \
      AEROGPU_D3D9_STUB_LOG_ONCE();                                                              \
      const auto packed = d3d9_stub_trace_args(args...);                                         \
      D3d9TraceCall trace(trace_func, packed[0], packed[1], packed[2], packed[3]);               \
      if constexpr (std::is_same_v<Ret, void>) {                                                  \
        (void)trace.ret(stub_hr);                                                                 \
        return;                                                                                   \
      }                                                                                           \
      if constexpr (std::is_same_v<Ret, HRESULT>) {                                               \
        return trace.ret(stub_hr);                                                                \
      }                                                                                           \
      (void)trace.ret(stub_hr);                                                                   \
      return Ret{};                                                                               \
    }                                                                                             \
  }

// Stubbed entrypoints: keep these non-NULL so the Win7 runtime can call into the
// UMD without crashing. See `drivers/aerogpu/umd/d3d9/README.md`.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetTextureStageState, D3d9TraceFunc::DeviceSetTextureStageState, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetTransform, D3d9TraceFunc::DeviceSetTransform, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnMultiplyTransform, D3d9TraceFunc::DeviceMultiplyTransform, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetClipPlane, D3d9TraceFunc::DeviceSetClipPlane, S_OK);

// Shader constant paths (int/bool) are not implemented yet; treat as a no-op to
// keep DWM alive while we bring up shader translation.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetShaderConstI, D3d9TraceFunc::DeviceSetShaderConstI, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetShaderConstB, D3d9TraceFunc::DeviceSetShaderConstB, S_OK);

#undef AEROGPU_D3D9_DEFINE_DDI_STUB
#endif

uint64_t monotonic_ms() {
#if defined(_WIN32)
  return static_cast<uint64_t>(GetTickCount64());
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

uint64_t qpc_now() {
#if defined(_WIN32)
  LARGE_INTEGER li;
  QueryPerformanceCounter(&li);
  return static_cast<uint64_t>(li.QuadPart);
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

void sleep_ms(uint32_t ms) {
#if defined(_WIN32)
  Sleep(ms);
#else
  std::this_thread::sleep_for(std::chrono::milliseconds(ms));
#endif
}

struct FenceSnapshot {
  uint64_t last_submitted = 0;
  uint64_t last_completed = 0;
};

#if defined(_WIN32)

// Best-effort HDC -> adapter LUID translation.
//
// Win7's D3D9 runtime and DWM may open the same adapter using both the HDC and
// LUID paths. Returning a stable LUID from OpenAdapterFromHdc is critical so our
// adapter cache (keyed by LUID) maps both opens to the same Adapter instance.
using NTSTATUS = LONG;

constexpr bool nt_success(NTSTATUS st) {
  return st >= 0;
}

struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  UINT hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
};

struct D3DKMT_CLOSEADAPTER {
  UINT hAdapter;
};

using PFND3DKMTOpenAdapterFromHdc = NTSTATUS(__stdcall*)(D3DKMT_OPENADAPTERFROMHDC* pData);
using PFND3DKMTCloseAdapter = NTSTATUS(__stdcall*)(D3DKMT_CLOSEADAPTER* pData);

bool get_luid_from_hdc(HDC hdc, LUID* luid_out) {
  if (!hdc || !luid_out) {
    return false;
  }

  HMODULE gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!gdi32) {
    return false;
  }

  auto* open_adapter_from_hdc =
      reinterpret_cast<PFND3DKMTOpenAdapterFromHdc>(GetProcAddress(gdi32, "D3DKMTOpenAdapterFromHdc"));
  auto* close_adapter =
      reinterpret_cast<PFND3DKMTCloseAdapter>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
  if (!open_adapter_from_hdc || !close_adapter) {
    FreeLibrary(gdi32);
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC open{};
  open.hDc = hdc;
  open.hAdapter = 0;
  std::memset(&open.AdapterLuid, 0, sizeof(open.AdapterLuid));
  open.VidPnSourceId = 0;

  const NTSTATUS st = open_adapter_from_hdc(&open);
  if (!nt_success(st) || open.hAdapter == 0) {
    FreeLibrary(gdi32);
    return false;
  }

  *luid_out = open.AdapterLuid;

  D3DKMT_CLOSEADAPTER close{};
  close.hAdapter = open.hAdapter;
  close_adapter(&close);

  FreeLibrary(gdi32);
  return true;
}

#endif

FenceSnapshot refresh_fence_snapshot(Adapter* adapter) {
  FenceSnapshot snap{};
  if (!adapter) {
    return snap;
  }

#if defined(_WIN32)
  // DWM and many D3D9Ex clients poll EVENT queries in tight loops. Querying the
  // KMD fence counter (last completed) requires a D3DKMTEscape call, so throttle
  // it to at most once per millisecond tick to avoid burning CPU in the kernel.
  //
  // Note: we intentionally do *not* use the escape's \"last submitted\" fence as
  // a per-submission fence ID when polling. Under multi-process workloads (DWM +
  // apps) it is global and can be dominated by another process's submissions.
  // Per-submission fence IDs must come from the runtime callbacks (e.g.
  // SubmissionFenceId / NewFenceValue).
  const uint64_t now_ms = monotonic_ms();
  bool should_query_kmd = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->last_kmd_fence_query_ms != now_ms) {
      adapter->last_kmd_fence_query_ms = now_ms;
      should_query_kmd = true;
    }
  }

  if (should_query_kmd && adapter->kmd_query_available.load(std::memory_order_acquire)) {
    uint64_t completed = 0;
    if (adapter->kmd_query.QueryFence(/*last_submitted=*/nullptr, &completed)) {
      bool updated = false;
      {
        std::lock_guard<std::mutex> lock(adapter->fence_mutex);
        const uint64_t prev_completed = adapter->completed_fence;
        adapter->completed_fence = std::max<uint64_t>(adapter->completed_fence, completed);
        updated = (adapter->completed_fence != prev_completed);
      }
      if (updated) {
        adapter->fence_cv.notify_all();
      }
    } else {
      adapter->kmd_query_available.store(false, std::memory_order_release);
    }
  }
#endif

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    snap.last_submitted = adapter->last_submitted_fence;
    snap.last_completed = adapter->completed_fence;
  }
  return snap;
}

void retire_completed_presents_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return;
  }

  const uint64_t completed = refresh_fence_snapshot(dev->adapter).last_completed;
  while (!dev->inflight_present_fences.empty() && dev->inflight_present_fences.front() <= completed) {
    dev->inflight_present_fences.pop_front();
  }
}

enum class FenceWaitResult {
  Complete,
  NotReady,
  Failed,
};

#if defined(_WIN32)
using AerogpuNtStatus = LONG;

constexpr AerogpuNtStatus kStatusSuccess = 0x00000000L;
constexpr AerogpuNtStatus kStatusTimeout = 0x00000102L;

#pragma pack(push, 8)
struct AerogpuD3DKMTWaitForSynchronizationObject {
  UINT ObjectCount;
  union {
    const WddmHandle* ObjectHandleArray;
    WddmHandle hSyncObjects;
  };
  union {
    const uint64_t* FenceValueArray;
    uint64_t FenceValue;
  };
  uint64_t Timeout;
};
#pragma pack(pop)

static_assert(std::is_standard_layout<AerogpuD3DKMTWaitForSynchronizationObject>::value,
              "D3DKMT wait args must have a stable ABI");
#if defined(_WIN64)
static_assert(sizeof(AerogpuD3DKMTWaitForSynchronizationObject) == 32, "Unexpected D3DKMT wait args size (x64)");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectCount) == 0, "Unexpected ObjectCount offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == 8,
              "Unexpected ObjectHandleArray offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == 16, "Unexpected FenceValueArray offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, Timeout) == 24, "Unexpected Timeout offset");
#else
static_assert(sizeof(AerogpuD3DKMTWaitForSynchronizationObject) == 24, "Unexpected D3DKMT wait args size (x86)");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectCount) == 0, "Unexpected ObjectCount offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == 4,
              "Unexpected ObjectHandleArray offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == 8, "Unexpected FenceValueArray offset");
static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, Timeout) == 16, "Unexpected Timeout offset");
#endif

using PFND3DKMTWaitForSynchronizationObject =
    AerogpuNtStatus(WINAPI*)(AerogpuD3DKMTWaitForSynchronizationObject* pData);

PFND3DKMTWaitForSynchronizationObject load_d3dkmt_wait_for_sync_object() {
  static PFND3DKMTWaitForSynchronizationObject fn = []() -> PFND3DKMTWaitForSynchronizationObject {
    HMODULE gdi32 = GetModuleHandleW(L"gdi32.dll");
    if (!gdi32) {
      gdi32 = LoadLibraryW(L"gdi32.dll");
    }
    if (!gdi32) {
      return nullptr;
    }
    return reinterpret_cast<PFND3DKMTWaitForSynchronizationObject>(
        GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject"));
  }();
  return fn;
}
#endif

FenceWaitResult wait_for_fence(Device* dev, uint64_t fence_value, uint32_t timeout_ms) {
  if (!dev || !dev->adapter) {
    return FenceWaitResult::Failed;
  }
  if (fence_value == 0) {
    return FenceWaitResult::Complete;
  }

  Adapter* adapter = dev->adapter;

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->completed_fence >= fence_value) {
      return FenceWaitResult::Complete;
    }
  }

#if defined(_WIN32)
  // For bounded waits, prefer letting the kernel wait on the WDDM sync object.
  // This avoids user-mode polling loops (Sleep(1) + repeated fence queries).
  if (timeout_ms != 0) {
    const WddmHandle sync_object = dev->wddm_context.hSyncObject;
    if (sync_object != 0) {
      auto* wait_fn = load_d3dkmt_wait_for_sync_object();
      if (wait_fn) {
        AerogpuD3DKMTWaitForSynchronizationObject args{};
        args.ObjectCount = 1;
        args.hSyncObjects = sync_object;
        args.FenceValue = fence_value;
        args.Timeout = timeout_ms;

        const AerogpuNtStatus st = wait_fn(&args);
        if (st == kStatusSuccess) {
          {
            std::lock_guard<std::mutex> lock(adapter->fence_mutex);
            adapter->completed_fence = std::max(adapter->completed_fence, fence_value);
          }
          adapter->fence_cv.notify_all();
          return FenceWaitResult::Complete;
        }
        if (st == kStatusTimeout) {
          return FenceWaitResult::NotReady;
        }
      }
    }
  }
#endif

  // Fast path: for polling callers (GetData), avoid per-call kernel waits. We
  // prefer querying the KMD fence counters (throttled inside
  // refresh_fence_snapshot) so tight polling loops don't spam syscalls.
  if (timeout_ms == 0) {
    if (refresh_fence_snapshot(adapter).last_completed >= fence_value) {
      return FenceWaitResult::Complete;
    }

#if defined(_WIN32)
    // If the KMD fence query path is unavailable, fall back to polling the WDDM
    // sync object once. This keeps EVENT queries functional even if the escape
    // path is missing.
    if (!adapter->kmd_query_available.load(std::memory_order_acquire)) {
      const WddmHandle sync_object = dev->wddm_context.hSyncObject;
      if (sync_object != 0) {
        auto* wait_fn = load_d3dkmt_wait_for_sync_object();
        if (wait_fn) {
          AerogpuD3DKMTWaitForSynchronizationObject args{};
          args.ObjectCount = 1;
          args.hSyncObjects = sync_object;
          args.FenceValue = fence_value;
          args.Timeout = 0; // poll

          const AerogpuNtStatus st = wait_fn(&args);
          if (st == kStatusSuccess) {
            {
              std::lock_guard<std::mutex> lock(adapter->fence_mutex);
              adapter->completed_fence = std::max(adapter->completed_fence, fence_value);
            }
            adapter->fence_cv.notify_all();
            return FenceWaitResult::Complete;
          }
        }
      }
    }
#endif

    return FenceWaitResult::NotReady;
  }

  const uint64_t deadline = monotonic_ms() + timeout_ms;
  while (monotonic_ms() < deadline) {
    if (refresh_fence_snapshot(adapter).last_completed >= fence_value) {
      return FenceWaitResult::Complete;
    }

    sleep_ms(1);
  }

  return (refresh_fence_snapshot(adapter).last_completed >= fence_value) ? FenceWaitResult::Complete
                                                                        : FenceWaitResult::NotReady;
}

HRESULT throttle_presents_locked(Device* dev, uint32_t d3d9_present_flags) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (!dev->adapter) {
    return E_FAIL;
  }

  // Clamp in case callers pass unexpected values.
  if (dev->max_frame_latency < kMaxFrameLatencyMin) {
    dev->max_frame_latency = kMaxFrameLatencyMin;
  }
  if (dev->max_frame_latency > kMaxFrameLatencyMax) {
    dev->max_frame_latency = kMaxFrameLatencyMax;
  }

  retire_completed_presents_locked(dev);

  if (dev->inflight_present_fences.size() < dev->max_frame_latency) {
    return S_OK;
  }

  const bool dont_wait = (d3d9_present_flags & kD3dPresentDoNotWait) != 0;
  if (dont_wait) {
    return kD3dErrWasStillDrawing;
  }

  // Wait for at least one present fence to retire, but never indefinitely.
  const uint64_t deadline = monotonic_ms() + kPresentThrottleMaxWaitMs;
  while (dev->inflight_present_fences.size() >= dev->max_frame_latency) {
    const uint64_t now = monotonic_ms();
    if (now >= deadline) {
      // Forward progress failed; drop the oldest fence to ensure PresentEx
      // returns quickly. This preserves overall system responsiveness at the
      // expense of perfect throttling accuracy under GPU hangs.
      dev->inflight_present_fences.pop_front();
      break;
    }

    const uint64_t oldest = dev->inflight_present_fences.front();
    const uint32_t time_left = static_cast<uint32_t>(std::min<uint64_t>(deadline - now, kPresentThrottleMaxWaitMs));
    (void)wait_for_fence(dev, oldest, time_left);
    retire_completed_presents_locked(dev);
  }

  return S_OK;
}

uint32_t d3d9_format_to_aerogpu(uint32_t d3d9_format) {
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8
    case 21u:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case 22u:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    // D3DFMT_A8B8G8R8
    case 32u:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    // D3DFMT_D24S8
    case 75u:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

// D3DLOCK_* flags (numeric values from d3d9.h). Only the bits we care about are
// defined here to keep the UMD self-contained.
constexpr uint32_t kD3DLOCK_READONLY = 0x00000010u;

// D3DPOOL_* (numeric values from d3d9.h).
constexpr uint32_t kD3DPOOL_DEFAULT = 0u;
constexpr uint32_t kD3DPOOL_SYSTEMMEM = 2u;

uint32_t d3d9_stage_to_aerogpu_stage(AEROGPU_D3D9DDI_SHADER_STAGE stage) {
  return (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? AEROGPU_SHADER_STAGE_VERTEX : AEROGPU_SHADER_STAGE_PIXEL;
}

uint32_t d3d9_index_format_to_aerogpu(AEROGPU_D3D9DDI_INDEX_FORMAT fmt) {
  return (fmt == AEROGPU_D3D9DDI_INDEX_FORMAT_U32) ? AEROGPU_INDEX_FORMAT_UINT32 : AEROGPU_INDEX_FORMAT_UINT16;
}

// D3DUSAGE_* subset (numeric values from d3d9types.h).
constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;

uint32_t d3d9_usage_to_aerogpu_usage_flags(uint32_t usage) {
  uint32_t flags = AEROGPU_RESOURCE_USAGE_TEXTURE;
  if (usage & kD3DUsageRenderTarget) {
    flags |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (usage & kD3DUsageDepthStencil) {
    flags |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return flags;
}

uint32_t d3d9_prim_to_topology(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim) {
  switch (prim) {
    case AEROGPU_D3D9DDI_PRIM_POINTLIST:
      return AEROGPU_TOPOLOGY_POINTLIST;
    case AEROGPU_D3D9DDI_PRIM_LINELIST:
      return AEROGPU_TOPOLOGY_LINELIST;
    case AEROGPU_D3D9DDI_PRIM_LINESTRIP:
      return AEROGPU_TOPOLOGY_LINESTRIP;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLESTRIP:
      return AEROGPU_TOPOLOGY_TRIANGLESTRIP;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLEFAN:
      return AEROGPU_TOPOLOGY_TRIANGLEFAN;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLELIST:
    default:
      return AEROGPU_TOPOLOGY_TRIANGLELIST;
  }
}

uint32_t vertex_count_from_primitive(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim, uint32_t primitive_count) {
  switch (prim) {
    case AEROGPU_D3D9DDI_PRIM_POINTLIST:
      return primitive_count;
    case AEROGPU_D3D9DDI_PRIM_LINELIST:
      return primitive_count * 2;
    case AEROGPU_D3D9DDI_PRIM_LINESTRIP:
      return primitive_count + 1;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLELIST:
      return primitive_count * 3;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLESTRIP:
    case AEROGPU_D3D9DDI_PRIM_TRIANGLEFAN:
      return primitive_count + 2;
    default:
      return primitive_count * 3;
  }
}

uint32_t index_count_from_primitive(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim, uint32_t primitive_count) {
  // Indexed draws follow the same primitive->index expansion rules.
  return vertex_count_from_primitive(prim, primitive_count);
}

// -----------------------------------------------------------------------------
// Minimal fixed-function (FVF) support (bring-up)
// -----------------------------------------------------------------------------

constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;

constexpr uint32_t kSupportedFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;

#pragma pack(push, 1)
struct D3DVERTEXELEMENT9_COMPAT {
  uint16_t Stream;
  uint16_t Offset;
  uint8_t Type;
  uint8_t Method;
  uint8_t Usage;
  uint8_t UsageIndex;
};
#pragma pack(pop)

static_assert(sizeof(D3DVERTEXELEMENT9_COMPAT) == 8, "D3DVERTEXELEMENT9 must be 8 bytes");

constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

// -----------------------------------------------------------------------------
// Handle helpers
// -----------------------------------------------------------------------------

Adapter* as_adapter(AEROGPU_D3D9DDI_HADAPTER hAdapter) {
  return reinterpret_cast<Adapter*>(hAdapter.pDrvPrivate);
}

Device* as_device(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  return reinterpret_cast<Device*>(hDevice.pDrvPrivate);
}

Resource* as_resource(AEROGPU_D3D9DDI_HRESOURCE hRes) {
  return reinterpret_cast<Resource*>(hRes.pDrvPrivate);
}

SwapChain* as_swapchain(AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain) {
  return reinterpret_cast<SwapChain*>(hSwapChain.pDrvPrivate);
}

Shader* as_shader(AEROGPU_D3D9DDI_HSHADER hShader) {
  return reinterpret_cast<Shader*>(hShader.pDrvPrivate);
}

VertexDecl* as_vertex_decl(AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  return reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
}

Query* as_query(AEROGPU_D3D9DDI_HQUERY hQuery) {
  return reinterpret_cast<Query*>(hQuery.pDrvPrivate);
}

// Forward-declared so helpers can opportunistically split submissions when the
// runtime-provided DMA buffer / allocation list is full.
uint64_t submit(Device* dev, bool is_present = false);

// -----------------------------------------------------------------------------
// Command emission helpers (protocol: drivers/aerogpu/protocol/aerogpu_cmd.h)
// -----------------------------------------------------------------------------

bool ensure_cmd_space(Device* dev, size_t bytes_needed) {
  if (!dev) {
    return false;
  }
  if (!dev->adapter) {
    return false;
  }

  if (dev->cmd.bytes_remaining() >= bytes_needed) {
    return true;
  }

  // If the current submission is non-empty, flush it and retry.
  if (!dev->cmd.empty()) {
    (void)submit(dev);
  }

  return dev->cmd.bytes_remaining() >= bytes_needed;
}

template <typename T>
T* append_fixed_locked(Device* dev, uint32_t opcode) {
  const size_t needed = align_up(sizeof(T), 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendFixed<T>(opcode);
}

template <typename HeaderT>
HeaderT* append_with_payload_locked(Device* dev, uint32_t opcode, const void* payload, size_t payload_size) {
  const size_t needed = align_up(sizeof(HeaderT) + payload_size, 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendWithPayload<HeaderT>(opcode, payload, payload_size);
}

HRESULT track_resource_allocation_locked(Device* dev, Resource* res, bool write) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  // Only track allocations when running on the WDDM path. Repo/compat builds
  // don't have WDDM allocation handles or runtime-provided allocation lists.
  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

  if (res->backing_alloc_id == 0) {
    // backing_alloc_id==0 denotes a host-allocated resource (no guest allocation
    // table entry required).
    return S_OK;
  }

  if (res->wddm_hAllocation == 0) {
    logf("aerogpu-d3d9: missing WDDM hAllocation for resource handle=%u alloc_id=%u\n",
         res->handle,
         res->backing_alloc_id);
    return E_FAIL;
  }

  AllocRef ref{};
  if (write) {
    ref = dev->alloc_list_tracker.track_render_target_write(res->wddm_hAllocation, res->backing_alloc_id);
  } else if (res->kind == ResourceKind::Buffer) {
    ref = dev->alloc_list_tracker.track_buffer_read(res->wddm_hAllocation, res->backing_alloc_id);
  } else {
    ref = dev->alloc_list_tracker.track_texture_read(res->wddm_hAllocation, res->backing_alloc_id);
  }

  if (ref.status == AllocRefStatus::kNeedFlush) {
    // Split the submission and retry.
    (void)submit(dev);

    if (write) {
      ref = dev->alloc_list_tracker.track_render_target_write(res->wddm_hAllocation, res->backing_alloc_id);
    } else if (res->kind == ResourceKind::Buffer) {
      ref = dev->alloc_list_tracker.track_buffer_read(res->wddm_hAllocation, res->backing_alloc_id);
    } else {
      ref = dev->alloc_list_tracker.track_texture_read(res->wddm_hAllocation, res->backing_alloc_id);
    }
  }

  if (ref.status != AllocRefStatus::kOk) {
    logf("aerogpu-d3d9: failed to track allocation (handle=%u alloc_id=%u status=%u)\n",
         res->handle,
         res->backing_alloc_id,
         static_cast<uint32_t>(ref.status));
    return E_FAIL;
  }

  return S_OK;
}

HRESULT track_draw_state_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

  for (uint32_t i = 0; i < 4; i++) {
    if (dev->render_targets[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (hr < 0) {
      return hr;
    }
  }

  for (uint32_t i = 0; i < 16; i++) {
    if (dev->textures[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->textures[i], /*write=*/false);
      if (hr < 0) {
        return hr;
      }
    }
  }

  for (uint32_t i = 0; i < 16; i++) {
    if (dev->streams[i].vb) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->streams[i].vb, /*write=*/false);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->index_buffer) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->index_buffer, /*write=*/false);
    if (hr < 0) {
      return hr;
    }
  }

  return S_OK;
}

HRESULT track_render_targets_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

  for (uint32_t i = 0; i < 4; i++) {
    if (dev->render_targets[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (hr < 0) {
      return hr;
    }
  }

  return S_OK;
}

bool emit_set_render_targets_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_targets>(dev, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }

  // The host executor rejects gapped render-target bindings (a null RT followed
  // by a non-null RT). Clamp to the contiguous prefix to avoid emitting a packet
  // that would abort command-stream execution.
  uint32_t color_count = 0;
  while (color_count < 4 && dev->render_targets[color_count]) {
    color_count++;
  }
  for (uint32_t i = color_count; i < 4; ++i) {
    dev->render_targets[i] = nullptr;
  }

  cmd->color_count = color_count;
  cmd->depth_stencil = dev->depth_stencil ? dev->depth_stencil->handle : 0;

  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  for (uint32_t i = 0; i < color_count; i++) {
    cmd->colors[i] = dev->render_targets[i] ? dev->render_targets[i]->handle : 0;
  }
  return true;
}

bool emit_bind_shaders_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_bind_shaders>(dev, AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    return false;
  }
  cmd->vs = dev->vs ? dev->vs->handle : 0;
  cmd->ps = dev->ps ? dev->ps->handle : 0;
  cmd->cs = 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_topology_locked(Device* dev, uint32_t topology) {
  if (dev->topology == topology) {
    return true;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_primitive_topology>(dev, AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    return false;
  }
  dev->topology = topology;
  cmd->topology = topology;
  cmd->reserved0 = 0;
  return true;
}

bool emit_create_resource_locked(Device* dev, Resource* res) {
  if (!dev || !res) {
    return false;
  }

  if (res->kind == ResourceKind::Buffer) {
    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_create_buffer), 4))) {
      return false;
    }
    if (track_resource_allocation_locked(dev, res, /*write=*/false) < 0) {
      return false;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_create_buffer>(dev, AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      return false;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER | AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;
    return true;
  }

  if (res->kind == ResourceKind::Surface || res->kind == ResourceKind::Texture2D) {
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_create_texture2d), 4))) {
      return false;
    }
    if (track_resource_allocation_locked(dev, res, /*write=*/false) < 0) {
      return false;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_create_texture2d>(dev, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      return false;
    }
    cmd->texture_handle = res->handle;
    cmd->usage_flags = d3d9_usage_to_aerogpu_usage_flags(res->usage);
    cmd->format = d3d9_format_to_aerogpu(res->format);
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;
    return true;
  }
  return false;
}

bool emit_destroy_resource_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_resource>(dev, AEROGPU_CMD_DESTROY_RESOURCE);
  if (!cmd) {
    return false;
  }
  cmd->resource_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_export_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_export_shared_surface>(dev, AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  if (!cmd) {
    return false;
  }
  logf("aerogpu-d3d9: export shared surface handle=%u share_token=0x%llx\n",
       static_cast<unsigned>(res->handle),
       static_cast<unsigned long long>(res->share_token));
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
  return true;
}

bool emit_import_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_import_shared_surface>(dev, AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!cmd) {
    return false;
  }
  logf("aerogpu-d3d9: import shared surface out_handle=%u share_token=0x%llx\n",
       static_cast<unsigned>(res->handle),
       static_cast<unsigned long long>(res->share_token));
  cmd->out_resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
  return true;
}

bool emit_create_shader_locked(Device* dev, Shader* sh) {
  if (!dev || !sh) {
    return false;
  }

  auto* cmd = append_with_payload_locked<aerogpu_cmd_create_shader_dxbc>(
      dev,
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->bytecode.data(), sh->bytecode.size());
  if (!cmd) {
    return false;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = d3d9_stage_to_aerogpu_stage(sh->stage);
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->bytecode.size());
  cmd->reserved0 = 0;
  return true;
}

bool emit_destroy_shader_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_shader>(dev, AEROGPU_CMD_DESTROY_SHADER);
  if (!cmd) {
    return false;
  }
  cmd->shader_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_create_input_layout_locked(Device* dev, VertexDecl* decl) {
  if (!dev || !decl) {
    return false;
  }

  auto* cmd = append_with_payload_locked<aerogpu_cmd_create_input_layout>(
      dev,
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, decl->blob.data(), decl->blob.size());
  if (!cmd) {
    return false;
  }
  cmd->input_layout_handle = decl->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(decl->blob.size());
  cmd->reserved0 = 0;
  return true;
}

bool emit_destroy_input_layout_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_input_layout>(dev, AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  if (!cmd) {
    return false;
  }
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_input_layout_locked(Device* dev, VertexDecl* decl) {
  if (!dev) {
    return false;
  }
  if (dev->vertex_decl == decl) {
    return true;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_input_layout>(dev, AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    return false;
  }

  dev->vertex_decl = decl;
  cmd->input_layout_handle = decl ? decl->handle : 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_stream_source_locked(
    Device* dev,
    uint32_t stream,
    Resource* vb,
    uint32_t offset_bytes,
    uint32_t stride_bytes) {
  if (!dev || stream >= 16) {
    return false;
  }

  DeviceStateStream& ss = dev->streams[stream];
  if (ss.vb == vb && ss.offset_bytes == offset_bytes && ss.stride_bytes == stride_bytes) {
    return true;
  }

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = vb ? vb->handle : 0;
  binding.stride_bytes = stride_bytes;
  binding.offset_bytes = offset_bytes;
  binding.reserved0 = 0;

  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
      dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
  if (!cmd) {
    return false;
  }
  cmd->start_slot = stream;
  cmd->buffer_count = 1;

  ss.vb = vb;
  ss.offset_bytes = offset_bytes;
  ss.stride_bytes = stride_bytes;
  return true;
}

Shader* create_internal_shader_locked(
    Device* dev,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    const void* bytecode,
    uint32_t bytecode_size) {
  if (!dev || !dev->adapter || !bytecode || bytecode_size == 0) {
    return nullptr;
  }

  auto sh = std::make_unique<Shader>();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = stage;
  try {
    sh->bytecode.resize(bytecode_size);
  } catch (...) {
    return nullptr;
  }
  std::memcpy(sh->bytecode.data(), bytecode, bytecode_size);

  if (!emit_create_shader_locked(dev, sh.get())) {
    return nullptr;
  }
  return sh.release();
}

VertexDecl* create_internal_vertex_decl_locked(Device* dev, const void* pDecl, uint32_t decl_size) {
  if (!dev || !dev->adapter || !pDecl || decl_size == 0) {
    return nullptr;
  }

  auto decl = std::make_unique<VertexDecl>();
  decl->handle = allocate_global_handle(dev->adapter);
  try {
    decl->blob.resize(decl_size);
  } catch (...) {
    return nullptr;
  }
  std::memcpy(decl->blob.data(), pDecl, decl_size);

  if (!emit_create_input_layout_locked(dev, decl.get())) {
    return nullptr;
  }
  return decl.release();
}

HRESULT ensure_fixedfunc_pipeline_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  if (dev->fvf != kSupportedFvfXyzrhwDiffuse) {
    return E_NOTIMPL;
  }

  if (!dev->fixedfunc_vs) {
    const void* vs_bytes = fixedfunc::kVsPassthroughPosColor;
    const uint32_t vs_size = static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor));
    dev->fixedfunc_vs = create_internal_shader_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_VS, vs_bytes, vs_size);
    if (!dev->fixedfunc_vs) {
      return E_OUTOFMEMORY;
    }
  }
  if (!dev->fixedfunc_ps) {
    const void* ps_bytes = fixedfunc::kPsPassthroughColor;
    const uint32_t ps_size = static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor));
    dev->fixedfunc_ps = create_internal_shader_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_PS, ps_bytes, ps_size);
    if (!dev->fixedfunc_ps) {
      return E_OUTOFMEMORY;
    }
  }

  // Ensure the FVF-derived declaration is bound.
  if (dev->fvf_vertex_decl) {
    if (!emit_set_input_layout_locked(dev, dev->fvf_vertex_decl)) {
      return E_OUTOFMEMORY;
    }
  }

  // Bind the fixed-function shaders iff the app did not set explicit shaders.
  if (!dev->user_vs && !dev->user_ps) {
    if (dev->vs != dev->fixedfunc_vs || dev->ps != dev->fixedfunc_ps) {
      Shader* prev_vs = dev->vs;
      Shader* prev_ps = dev->ps;
      dev->vs = dev->fixedfunc_vs;
      dev->ps = dev->fixedfunc_ps;
      if (!emit_bind_shaders_locked(dev)) {
        dev->vs = prev_vs;
        dev->ps = prev_ps;
        return E_OUTOFMEMORY;
      }
    }
  }

  return S_OK;
}

HRESULT ensure_up_vertex_buffer_locked(Device* dev, uint32_t required_size) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  if (required_size == 0) {
    return E_INVALIDARG;
  }

  const uint32_t current_size = dev->up_vertex_buffer ? dev->up_vertex_buffer->size_bytes : 0;
  if (dev->up_vertex_buffer && current_size >= required_size) {
    return S_OK;
  }

  // Grow to the next power-of-two-ish size to avoid reallocating every draw.
  uint32_t new_size = current_size ? current_size : 4096u;
  while (new_size < required_size) {
    new_size = (new_size > (0x7FFFFFFFu / 2)) ? required_size : (new_size * 2);
  }

  auto vb = std::make_unique<Resource>();
  vb->handle = allocate_global_handle(dev->adapter);
  vb->kind = ResourceKind::Buffer;
  vb->size_bytes = new_size;
  try {
    vb->storage.resize(new_size);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_create_resource_locked(dev, vb.get())) {
    return E_OUTOFMEMORY;
  }

  Resource* old = dev->up_vertex_buffer;
  dev->up_vertex_buffer = vb.release();
  if (old) {
    (void)emit_destroy_resource_locked(dev, old->handle);
    delete old;
  }
  return S_OK;
}

HRESULT ensure_up_index_buffer_locked(Device* dev, uint32_t required_size) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  if (required_size == 0) {
    return E_INVALIDARG;
  }

  const uint32_t current_size = dev->up_index_buffer ? dev->up_index_buffer->size_bytes : 0;
  if (dev->up_index_buffer && current_size >= required_size) {
    return S_OK;
  }

  uint32_t new_size = current_size ? current_size : 2048u;
  while (new_size < required_size) {
    new_size = (new_size > (0x7FFFFFFFu / 2)) ? required_size : (new_size * 2);
  }

  auto ib = std::make_unique<Resource>();
  ib->handle = allocate_global_handle(dev->adapter);
  ib->kind = ResourceKind::Buffer;
  ib->size_bytes = new_size;
  try {
    ib->storage.resize(new_size);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_create_resource_locked(dev, ib.get())) {
    return E_OUTOFMEMORY;
  }

  Resource* old = dev->up_index_buffer;
  dev->up_index_buffer = ib.release();
  if (old) {
    (void)emit_destroy_resource_locked(dev, old->handle);
    delete old;
  }
  return S_OK;
}

HRESULT emit_upload_buffer_locked(Device* dev, Resource* res, const void* data, uint32_t size_bytes) {
  if (!dev || !res || !data || size_bytes == 0) {
    return E_INVALIDARG;
  }
  if (size_bytes > res->size_bytes) {
    return E_INVALIDARG;
  }

  // Keep a CPU copy for debug/validation and for fixed-function emulation that
  // reads from buffers.
  if (res->storage.size() < size_bytes) {
    try {
      res->storage.resize(size_bytes);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
  }
  std::memcpy(res->storage.data(), data, size_bytes);

  const uint8_t* src = reinterpret_cast<const uint8_t*>(data);
  uint32_t remaining = size_bytes;
  uint32_t cur_offset = 0;

  while (remaining) {
    // Ensure we can fit at least a minimal upload packet (header + 1 byte).
    const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + 1, 4);
    if (!ensure_cmd_space(dev, min_needed)) {
      return E_OUTOFMEMORY;
    }

    // Uploads write into the destination buffer. Track its backing allocation
    // so the KMD alloc table contains the mapping for guest-backed resources.
    // (For internal host-only buffers backing_alloc_id==0, this is a no-op.)
    HRESULT track_hr = track_resource_allocation_locked(dev, res, /*write=*/true);
    if (FAILED(track_hr)) {
      return track_hr;
    }

    // Allocation tracking may have split/flushed the submission; ensure we
    // still have room for at least a minimal upload packet before sizing the
    // next chunk.
    if (!ensure_cmd_space(dev, min_needed)) {
      return E_OUTOFMEMORY;
    }

    const size_t avail = dev->cmd.bytes_remaining();
    size_t chunk = 0;
    if (avail > sizeof(aerogpu_cmd_upload_resource)) {
      chunk = std::min<size_t>(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
    }
    while (chunk && align_up(sizeof(aerogpu_cmd_upload_resource) + chunk, 4) > avail) {
      chunk--;
    }
    if (!chunk) {
      // Should only happen if the command buffer is extremely small; try a forced
      // submit and retry.
      submit(dev);
      continue;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_upload_resource>(
        dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }

    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = cur_offset;
    cmd->size_bytes = chunk;

    src += chunk;
    cur_offset += static_cast<uint32_t>(chunk);
    remaining -= static_cast<uint32_t>(chunk);
  }
  return S_OK;
}

float read_f32_unaligned(const uint8_t* p) {
  float v = 0.0f;
  std::memcpy(&v, p, sizeof(v));
  return v;
}

void write_f32_unaligned(uint8_t* p, float v) {
  std::memcpy(p, &v, sizeof(v));
}

void get_viewport_dims_locked(Device* dev, float* out_x, float* out_y, float* out_w, float* out_h) {
  float x = dev->viewport.x;
  float y = dev->viewport.y;
  float w = dev->viewport.w;
  float h = dev->viewport.h;

  if (w <= 0.0f || h <= 0.0f) {
    // Some apps rely on the default viewport. Use the current render target as a
    // conservative fallback.
    if (dev->render_targets[0]) {
      w = static_cast<float>(std::max(1u, dev->render_targets[0]->width));
      h = static_cast<float>(std::max(1u, dev->render_targets[0]->height));
      x = 0.0f;
      y = 0.0f;
    }
  }
  if (w <= 0.0f) {
    w = 1.0f;
  }
  if (h <= 0.0f) {
    h = 1.0f;
  }

  *out_x = x;
  *out_y = y;
  *out_w = w;
  *out_h = h;
}

HRESULT convert_xyzrhw_to_clipspace_locked(
    Device* dev,
    const void* src_vertices,
    uint32_t stride_bytes,
    uint32_t vertex_count,
    std::vector<uint8_t>* out_bytes) {
  if (!out_bytes) {
    return E_INVALIDARG;
  }
  out_bytes->clear();
  if (!dev || !src_vertices || stride_bytes < 20 || vertex_count == 0) {
    return E_INVALIDARG;
  }

  float vp_x = 0.0f;
  float vp_y = 0.0f;
  float vp_w = 1.0f;
  float vp_h = 1.0f;
  get_viewport_dims_locked(dev, &vp_x, &vp_y, &vp_w, &vp_h);

  const uint64_t total_bytes_u64 = static_cast<uint64_t>(stride_bytes) * static_cast<uint64_t>(vertex_count);
  if (total_bytes_u64 == 0 || total_bytes_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }
  try {
    out_bytes->resize(static_cast<size_t>(total_bytes_u64));
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  const uint8_t* src_base = reinterpret_cast<const uint8_t*>(src_vertices);
  uint8_t* dst_base = out_bytes->data();

  for (uint32_t i = 0; i < vertex_count; i++) {
    const uint8_t* src = src_base + static_cast<size_t>(i) * stride_bytes;
    uint8_t* dst = dst_base + static_cast<size_t>(i) * stride_bytes;

    // Preserve any trailing fields (diffuse color etc).
    std::memcpy(dst, src, stride_bytes);

    const float x = read_f32_unaligned(src + 0);
    const float y = read_f32_unaligned(src + 4);
    const float z = read_f32_unaligned(src + 8);
    const float rhw = read_f32_unaligned(src + 12);

    const float w = (rhw != 0.0f) ? (1.0f / rhw) : 1.0f;
    // D3D9's viewport transform uses a -0.5 pixel center convention. Invert it
    // so typical D3D9 pre-transformed vertex coordinates line up with pixel
    // centers.
    const float ndc_x = ((x + 0.5f - vp_x) / vp_w) * 2.0f - 1.0f;
    const float ndc_y = 1.0f - ((y + 0.5f - vp_y) / vp_h) * 2.0f;
    const float ndc_z = z;

    write_f32_unaligned(dst + 0, ndc_x * w);
    write_f32_unaligned(dst + 4, ndc_y * w);
    write_f32_unaligned(dst + 8, ndc_z * w);
    write_f32_unaligned(dst + 12, w);
  }
  return S_OK;
}

// -----------------------------------------------------------------------------
// Submission
// -----------------------------------------------------------------------------
//
// Shared allocations must use stable `alloc_id` values that are extremely
// unlikely to collide across guest processes: DWM can reference many redirected
// surfaces from different processes in a single submission, and the KMD's
// per-submit allocation table is keyed by `alloc_id`.
//
// The D3D9 UMD uses a best-effort cross-process monotonic counter (implemented
// via a named file mapping) to derive 31-bit alloc_id values for shared
// allocations.
//
// The mapping name is intentionally stable and generic: the UMD can be loaded
// into multiple guest processes (and different in-guest UMD versions may
// coexist briefly), and the same counter is also reused to allocate protocol
// object handles (see `allocate_global_handle()`).
uint64_t allocate_shared_alloc_id_token(Adapter* adapter) {
  if (!adapter) {
    return 0;
  }

#if defined(_WIN32)
  {
    std::lock_guard<std::mutex> lock(adapter->share_token_mutex);

    if (!adapter->share_token_view) {
      wchar_t name[128];
      // Keep the object name stable across processes within a session.
      swprintf(name,
               sizeof(name) / sizeof(name[0]),
               L"Local\\AeroGPU.GlobalHandleCounter");

      HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, sizeof(uint64_t), name);
      if (mapping) {
        void* view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(uint64_t));
        if (view) {
          adapter->share_token_mapping = mapping;
          adapter->share_token_view = view;
        } else {
          CloseHandle(mapping);
        }
      }
    }

    if (adapter->share_token_view) {
      auto* counter = reinterpret_cast<volatile LONG64*>(adapter->share_token_view);
      LONG64 token = InterlockedIncrement64(counter);
      const uint32_t alloc_id =
          static_cast<uint32_t>(static_cast<uint64_t>(token) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      if (alloc_id == 0) {
        token = InterlockedIncrement64(counter);
      }
      return static_cast<uint64_t>(token);
    }
  }

  // If we fail to set up the cross-process allocator (should be rare), fall
  // back to a per-process counter and fold PID bits into the *low* bits. Call
  // sites that derive a 31-bit `alloc_id` via
  // `token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX` must still get a cross-process-stable
  // identifier (DWM can reference many shared allocations from different
  // processes in a single submission).
  //
  // Note: This scheme is only used if CreateFileMapping/MapViewOfFile fail.
  // The named mapping is the preferred allocator because it is monotonic across
  // processes and avoids PID reuse/sequence wrap concerns in long sessions.
  const uint32_t pid = static_cast<uint32_t>(GetCurrentProcessId());
  const uint32_t pid_bits = (pid >> 2) & 0x1FFFFu;
  uint32_t seq = static_cast<uint32_t>(
      adapter->next_share_token.fetch_add(1, std::memory_order_relaxed)) &
                 0x3FFFu;
  if (seq == 0) {
    seq = static_cast<uint32_t>(
        adapter->next_share_token.fetch_add(1, std::memory_order_relaxed)) &
          0x3FFFu;
  }
  const uint32_t alloc_id = (pid_bits << 14) | seq;
  return static_cast<uint64_t>(alloc_id);
#else
  (void)adapter;
  static std::atomic<uint64_t> next_token{1};
  return next_token.fetch_add(1);
#endif
}

namespace {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
template <typename T, typename = void>
struct has_pfnRenderCb : std::false_type {};
template <typename T>
struct has_pfnRenderCb<T, std::void_t<decltype(std::declval<T>().pfnRenderCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnPresentCb : std::false_type {};
template <typename T>
struct has_pfnPresentCb<T, std::void_t<decltype(std::declval<T>().pfnPresentCb)>> : std::true_type {};

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
struct has_member_hContext : std::false_type {};
template <typename T>
struct has_member_hContext<T, std::void_t<decltype(std::declval<T>().hContext)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hDevice : std::false_type {};
template <typename T>
struct has_member_hDevice<T, std::void_t<decltype(std::declval<T>().hDevice)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pCommandBuffer<T, std::void_t<decltype(std::declval<T>().pCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDmaBuffer : std::false_type {};
template <typename T>
struct has_member_pDmaBuffer<T, std::void_t<decltype(std::declval<T>().pDmaBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CommandLength : std::false_type {};
template <typename T>
struct has_member_CommandLength<T, std::void_t<decltype(std::declval<T>().CommandLength)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CommandBufferSize : std::false_type {};
template <typename T>
struct has_member_CommandBufferSize<T, std::void_t<decltype(std::declval<T>().CommandBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DmaBufferSize : std::false_type {};
template <typename T>
struct has_member_DmaBufferSize<T, std::void_t<decltype(std::declval<T>().DmaBufferSize)>> : std::true_type {};

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

template <typename T, typename = void>
struct has_member_Flags : std::false_type {};
template <typename T>
struct has_member_Flags<T, std::void_t<decltype(std::declval<T>().Flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Present : std::false_type {};
template <typename T>
struct has_member_Present<T, std::void_t<decltype(std::declval<T>().Present)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pNewCommandBuffer<T, std::void_t<decltype(std::declval<T>().pNewCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewCommandBufferSize : std::false_type {};
template <typename T>
struct has_member_NewCommandBufferSize<T, std::void_t<decltype(std::declval<T>().NewCommandBufferSize)>> : std::true_type {};

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
struct has_member_SubmissionFenceId : std::false_type {};
template <typename T>
struct has_member_SubmissionFenceId<T, std::void_t<decltype(std::declval<T>().SubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewFenceValue : std::false_type {};
template <typename T>
struct has_member_NewFenceValue<T, std::void_t<decltype(std::declval<T>().NewFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pSubmissionFenceId : std::false_type {};
template <typename T>
struct has_member_pSubmissionFenceId<T, std::void_t<decltype(std::declval<T>().pSubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValue : std::false_type {};
template <typename T>
struct has_member_FenceValue<T, std::void_t<decltype(std::declval<T>().FenceValue)>> : std::true_type {};

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
struct has_member_DmaBufferPrivateDataSize<T, std::void_t<decltype(std::declval<T>().DmaBufferPrivateDataSize)>>
    : std::true_type {};

template <typename ArgsT>
void fill_submit_args(ArgsT& args, Device* dev, uint32_t command_length_bytes, bool is_present) {
  const bool patch_list_available = (dev->wddm_context.pPatchLocationList != nullptr);
  if constexpr (has_member_hDevice<ArgsT>::value) {
    args.hDevice = dev->wddm_device;
  }
  if constexpr (has_member_hContext<ArgsT>::value) {
    args.hContext = dev->wddm_context.hContext;
  }
  if constexpr (has_member_pCommandBuffer<ArgsT>::value) {
    args.pCommandBuffer = dev->wddm_context.pCommandBuffer;
  }
  if constexpr (has_member_pDmaBuffer<ArgsT>::value) {
    args.pDmaBuffer = dev->wddm_context.pCommandBuffer;
  }
  if constexpr (has_member_CommandLength<ArgsT>::value) {
    args.CommandLength = command_length_bytes;
  }
  if constexpr (has_member_CommandBufferSize<ArgsT>::value) {
    args.CommandBufferSize = dev->wddm_context.CommandBufferSize;
  }
  if constexpr (has_member_DmaBufferSize<ArgsT>::value) {
    args.DmaBufferSize = dev->wddm_context.CommandBufferSize;
  }
  if constexpr (has_member_pAllocationList<ArgsT>::value) {
    args.pAllocationList = dev->wddm_context.pAllocationList;
  }
  if constexpr (has_member_AllocationListSize<ArgsT>::value) {
    // DDI structs disagree on whether AllocationListSize means "capacity" or
    // "entries used". When NumAllocations is present, treat AllocationListSize
    // as the capacity returned by CreateContext. Otherwise fall back to the used
    // count (legacy submit structs with only a single size field).
    if constexpr (has_member_NumAllocations<ArgsT>::value) {
      args.AllocationListSize = dev->wddm_context.AllocationListSize;
    } else {
      args.AllocationListSize = dev->wddm_context.allocation_list_entries_used;
    }
  }
  if constexpr (has_member_NumAllocations<ArgsT>::value) {
    args.NumAllocations = dev->wddm_context.allocation_list_entries_used;
  }
  if constexpr (has_member_pPatchLocationList<ArgsT>::value) {
    args.pPatchLocationList = patch_list_available ? dev->wddm_context.pPatchLocationList : nullptr;
  }
  if constexpr (has_member_PatchLocationListSize<ArgsT>::value) {
    if (patch_list_available) {
      if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
        args.PatchLocationListSize = dev->wddm_context.PatchLocationListSize;
      } else {
        args.PatchLocationListSize = dev->wddm_context.patch_location_entries_used;
      }
    } else {
      args.PatchLocationListSize = 0;
    }
  }
  if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
    args.NumPatchLocations = patch_list_available ? dev->wddm_context.patch_location_entries_used : 0;
  }
  if constexpr (has_member_pDmaBufferPrivateData<ArgsT>::value) {
    args.pDmaBufferPrivateData = dev->wddm_context.pDmaBufferPrivateData;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<ArgsT>::value) {
    args.DmaBufferPrivateDataSize = dev->wddm_context.DmaBufferPrivateDataSize;
  }

  // Some WDDM callback arg structs include flags distinguishing render vs present.
  // If such flags are present, populate them so present submissions prefer the
  // DxgkDdiPresent path when routed via RenderCb fallback.
  if constexpr (has_member_Flags<ArgsT>::value) {
    using FlagsT = std::remove_reference_t<decltype(args.Flags)>;
    if constexpr (has_member_Present<FlagsT>::value) {
      args.Flags.Present = is_present ? 1 : 0;
    }
  }
}

template <typename ArgsT>
void update_context_from_submit_args(Device* dev, const ArgsT& args) {
  bool updated_cmd_buffer = false;
  if constexpr (has_member_pNewCommandBuffer<ArgsT>::value && has_member_NewCommandBufferSize<ArgsT>::value) {
    if (args.pNewCommandBuffer && args.NewCommandBufferSize) {
      dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(args.pNewCommandBuffer);
      dev->wddm_context.CommandBufferSize = args.NewCommandBufferSize;
      updated_cmd_buffer = true;
    }
  }

  if (!updated_cmd_buffer) {
    if constexpr (has_member_pCommandBuffer<ArgsT>::value) {
      if (args.pCommandBuffer) {
        dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(args.pCommandBuffer);
      }
    }
    if constexpr (has_member_CommandBufferSize<ArgsT>::value) {
      if (args.CommandBufferSize) {
        dev->wddm_context.CommandBufferSize = args.CommandBufferSize;
      }
    }
  }

  bool updated_allocation_list = false;
  if constexpr (has_member_pNewAllocationList<ArgsT>::value && has_member_NewAllocationListSize<ArgsT>::value) {
    if (args.pNewAllocationList && args.NewAllocationListSize) {
      dev->wddm_context.pAllocationList = args.pNewAllocationList;
      dev->wddm_context.AllocationListSize = args.NewAllocationListSize;
      updated_allocation_list = true;
    }
  }

  if (!updated_allocation_list) {
    if constexpr (has_member_pAllocationList<ArgsT>::value) {
      if (args.pAllocationList) {
        dev->wddm_context.pAllocationList = args.pAllocationList;
      }
    }
    if constexpr (has_member_AllocationListSize<ArgsT>::value && has_member_NumAllocations<ArgsT>::value) {
      if (args.AllocationListSize) {
        dev->wddm_context.AllocationListSize = args.AllocationListSize;
      }
    }
  }

  bool updated_patch_list = false;
  if constexpr (has_member_pNewPatchLocationList<ArgsT>::value && has_member_NewPatchLocationListSize<ArgsT>::value) {
    if (args.pNewPatchLocationList && args.NewPatchLocationListSize) {
      dev->wddm_context.pPatchLocationList = args.pNewPatchLocationList;
      dev->wddm_context.PatchLocationListSize = args.NewPatchLocationListSize;
      updated_patch_list = true;
    }
  }

  if (!updated_patch_list) {
    if constexpr (has_member_pPatchLocationList<ArgsT>::value) {
      if (args.pPatchLocationList) {
        dev->wddm_context.pPatchLocationList = args.pPatchLocationList;
      }
    }
    if constexpr (has_member_PatchLocationListSize<ArgsT>::value && has_member_NumPatchLocations<ArgsT>::value) {
      if (args.PatchLocationListSize) {
        dev->wddm_context.PatchLocationListSize = args.PatchLocationListSize;
      }
    }
  }

  // pDmaBufferPrivateData is required by the AeroGPU Win7 KMD (DxgkDdiRender /
  // DxgkDdiPresent expect it to be non-null). The runtime may rotate it along
  // with the command buffer, so treat it as an in/out field.
  if constexpr (has_member_pDmaBufferPrivateData<ArgsT>::value) {
    if (args.pDmaBufferPrivateData) {
      dev->wddm_context.pDmaBufferPrivateData = args.pDmaBufferPrivateData;
    }
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<ArgsT>::value) {
    if (args.DmaBufferPrivateDataSize) {
      dev->wddm_context.DmaBufferPrivateDataSize = args.DmaBufferPrivateDataSize;
    }
  }
}

template <typename CallbackFn>
HRESULT invoke_submit_callback(Device* dev,
                               CallbackFn cb,
                               uint32_t command_length_bytes,
                               bool is_present,
                               uint64_t* out_submission_fence) {
  if (out_submission_fence) {
    *out_submission_fence = 0;
  }

  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;

  Arg args{};
  fill_submit_args(args, dev, command_length_bytes, is_present);
  if constexpr (has_member_NewFenceValue<Arg>::value) {
    args.NewFenceValue = 0;
  }
  uint64_t submission_fence = 0;

  HRESULT hr = E_FAIL;
  if constexpr (has_member_SubmissionFenceId<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.SubmissionFenceId)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      // Some header/interface versions expose SubmissionFenceId as an output
      // pointer rather than an in-struct value. Provide a valid storage location
      // so the runtime can write back the exact per-submission fence ID.
      args.SubmissionFenceId = &fence_storage;
    } else {
      args.SubmissionFenceId = 0;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        } else {
          submission_fence = static_cast<uint64_t>(args.SubmissionFenceId);
        }
      } else {
        if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        } else {
          submission_fence = static_cast<uint64_t>(args.SubmissionFenceId);
        }
      }
    }
  } else if constexpr (has_member_pSubmissionFenceId<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.pSubmissionFenceId)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      args.pSubmissionFenceId = &fence_storage;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        }
      } else if constexpr (std::is_pointer<FenceMemberT>::value) {
        submission_fence = static_cast<uint64_t>(fence_storage);
      }
    }
  } else if constexpr (has_member_pFenceValue<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.pFenceValue)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      args.pFenceValue = &fence_storage;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        }
      } else if constexpr (std::is_pointer<FenceMemberT>::value) {
        submission_fence = static_cast<uint64_t>(fence_storage);
      }
    }
  } else if constexpr (has_member_FenceValue<Arg>::value) {
    args.FenceValue = 0;
    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else {
          submission_fence = static_cast<uint64_t>(args.FenceValue);
        }
      } else {
        submission_fence = static_cast<uint64_t>(args.FenceValue);
      }
    }
  } else {
    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        submission_fence = static_cast<uint64_t>(args.NewFenceValue);
      }
    }
  }

  if (FAILED(hr)) {
    return hr;
  }

  if (out_submission_fence) {
    *out_submission_fence = submission_fence;
  }

  // The runtime may rotate command buffers/lists after a submission. Preserve the
  // updated pointers and reset the book-keeping so the next submission starts
  // from a clean command stream header.
  update_context_from_submit_args(dev, args);
  // Keep the command stream writer bound to the currently active command buffer.
  // The runtime is allowed to return a new DMA buffer pointer/size in the
  // callback out-params; failing to rebind would cause us to write into a stale
  // buffer on the next submission.
  if (dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= sizeof(aerogpu_cmd_stream_header)) {
    dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  }
  dev->wddm_context.reset_submission_buffers();
  return hr;
}
#endif
} // namespace

uint64_t submit(Device* dev, bool is_present) {
  if (!dev) {
    return 0;
  }

  Adapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  if (dev->cmd.empty()) {
    // Even if there's nothing to submit, callers may use submit() as a "split"
    // point when the per-submit allocation list is full. Reset submission-local
    // tracking state so subsequent commands start with a fresh allocation list
    // without issuing an empty DMA buffer to the kernel.
    const uint64_t fence = dev->last_submission_fence;
    dev->cmd.reset();
    dev->alloc_list_tracker.reset();
    dev->wddm_context.reset_submission_buffers();
    return fence;
  }

  dev->cmd.finalize();
  const uint64_t cmd_bytes = static_cast<uint64_t>(dev->cmd.size());

  bool submitted_to_kmd = false;
  uint64_t submission_fence = 0;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  // WDDM submission path: hand the runtime-provided DMA/alloc list buffers back
  // to dxgkrnl via the device callbacks captured at CreateDevice time.
  //
  // The patch-location list is intentionally kept empty; guest-backed memory is
  // referenced via stable `alloc_id` values and resolved by the KMD's per-submit
  // allocation table.
  if (dev->wddm_context.hContext != 0 && dev->wddm_context.pCommandBuffer && dev->wddm_context.CommandBufferSize) {
    if (cmd_bytes <= dev->wddm_context.CommandBufferSize) {
      // CmdStreamWriter can be span-backed and write directly into the runtime
      // DMA buffer. Avoid memcpy on identical ranges (overlap is UB for memcpy).
      if (dev->cmd.data() != dev->wddm_context.pCommandBuffer) {
        std::memcpy(dev->wddm_context.pCommandBuffer, dev->cmd.data(), static_cast<size_t>(cmd_bytes));
      }
      dev->wddm_context.command_buffer_bytes_used = static_cast<uint32_t>(cmd_bytes);
      dev->wddm_context.allocation_list_entries_used = dev->alloc_list_tracker.list_len();
      dev->wddm_context.patch_location_entries_used = 0;

      HRESULT submit_hr = E_NOTIMPL;
      const uint32_t cmd_len = static_cast<uint32_t>(cmd_bytes);
      bool tried_present_cb = false;
      if (is_present) {
        if constexpr (has_pfnPresentCb<WddmDeviceCallbacks>::value) {
          if (dev->wddm_callbacks.pfnPresentCb) {
            tried_present_cb = true;
            submission_fence = 0;
            submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnPresentCb, cmd_len, /*is_present=*/true,
                                               &submission_fence);
          }
        }
      }

      // If no PresentCb is available, route present-like submissions through
      // RenderCb as a fallback.
      if (!is_present || !tried_present_cb) {
        if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
          if (dev->wddm_callbacks.pfnRenderCb) {
            submission_fence = 0;
            submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnRenderCb, cmd_len, /*is_present=*/is_present,
                                               &submission_fence);
          }
        }
      }

      if (SUCCEEDED(submit_hr)) {
        submitted_to_kmd = true;
        dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                       dev->wddm_context.AllocationListSize,
                                       adapter->max_allocation_list_slot_id);
      } else {
        logf("aerogpu-d3d9: submit callbacks failed hr=0x%08x\n", static_cast<unsigned>(submit_hr));
      }
    } else {
      logf("aerogpu-d3d9: submit command buffer too large (cmd=%llu cap=%u)\n",
           static_cast<unsigned long long>(cmd_bytes),
           static_cast<unsigned>(dev->wddm_context.CommandBufferSize));
    }
  }
#endif

  uint64_t fence = 0;
  // Fence value associated with this specific submission (as returned by the
  // runtime callback, or (rarely) the KMD query fallback). Keep this separate
  // from adapter-wide tracking so concurrent submissions cannot cause us to
  // return a "too-new" fence.
  uint64_t per_submission_fence = 0;
  bool updated = false;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  if (submitted_to_kmd) {
    // Critical: capture the exact per-submission fence returned by the runtime
    // callback for *this* submission (SubmissionFenceId/NewFenceValue).
    fence = submission_fence;

    // Some WDK header vintages do not expose the callback fence outputs. In
    // that case, fall back to querying the KMD's fence counters via DxgkDdiEscape
    // (D3DKMTEscape) so we still return a real fence value and never "fake
    // complete" fences in-process.
    uint64_t kmd_submitted = 0;
    uint64_t kmd_completed = 0;
    bool kmd_ok = false;
    if (fence == 0 && adapter->kmd_query_available.load(std::memory_order_acquire)) {
      kmd_ok = adapter->kmd_query.QueryFence(&kmd_submitted, &kmd_completed);
      if (!kmd_ok) {
        adapter->kmd_query_available.store(false, std::memory_order_release);
      } else {
        fence = kmd_submitted;
      }
    }

    per_submission_fence = fence;

    if (kmd_ok) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      const uint64_t prev_completed = adapter->completed_fence;
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, kmd_submitted);
      adapter->completed_fence = std::max(adapter->completed_fence, kmd_completed);
      adapter->next_fence = std::max(adapter->next_fence, adapter->last_submitted_fence + 1);
      adapter->last_kmd_fence_query_ms = monotonic_ms();
      updated = (adapter->last_submitted_fence != prev_submitted) || (adapter->completed_fence != prev_completed);
    }

    if (per_submission_fence) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, per_submission_fence);
      adapter->next_fence = std::max(adapter->next_fence, adapter->last_submitted_fence + 1);
      updated = updated || (adapter->last_submitted_fence != prev_submitted);
    }
  }
#endif

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI))
  if (fence == 0) {
    {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      if (adapter->next_fence <= adapter->last_submitted_fence) {
        adapter->next_fence = adapter->last_submitted_fence + 1;
      }

      const uint64_t stub_fence = adapter->next_fence++;
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      const uint64_t prev_completed = adapter->completed_fence;
      // Never allow the cached fence values to go backwards: they may be advanced
      // by the KMD query path (or, in a real WDDM build, by runtime-provided fence
      // callbacks).
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, stub_fence);
      adapter->completed_fence = std::max(adapter->completed_fence, stub_fence);
      fence = stub_fence;
      updated = updated || (adapter->last_submitted_fence != prev_submitted) || (adapter->completed_fence != prev_completed);
    }
  }
  per_submission_fence = fence;
#endif

  if (per_submission_fence == 0) {
    per_submission_fence = fence;
  }

  if (updated) {
    adapter->fence_cv.notify_all();
  }

  dev->last_submission_fence = per_submission_fence;
  dev->cmd.reset();
  dev->alloc_list_tracker.reset();
  dev->wddm_context.reset_submission_buffers();
  return per_submission_fence;
}

HRESULT flush_locked(Device* dev) {
  // Flushing an empty command buffer should be a no-op. This matters for
  // D3DGETDATA_FLUSH polling loops (e.g. DWM EVENT queries): if we submit an
  // empty buffer every poll we can flood the KMD/emulator with redundant
  // submissions and increase CPU usage.
  if (!dev || dev->cmd.empty()) {
    return S_OK;
  }
  // If we cannot fit an explicit FLUSH marker into the remaining space, just
  // submit the current buffer; the submission boundary is already a flush point.
  const size_t flush_bytes = align_up(sizeof(aerogpu_cmd_flush), 4);
  if (dev->cmd.bytes_remaining() < flush_bytes) {
    submit(dev);
    return S_OK;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_flush>(dev, AEROGPU_CMD_FLUSH);
  if (cmd) {
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  submit(dev);
  return S_OK;
}

HRESULT copy_surface_bytes(const Resource* src, Resource* dst) {
  if (!src || !dst) {
    return E_INVALIDARG;
  }
  if (src->width != dst->width || src->height != dst->height) {
    return E_INVALIDARG;
  }
  if (src->format != dst->format) {
    return E_INVALIDARG;
  }

  const uint32_t bpp = bytes_per_pixel(src->format);
  const uint32_t row_bytes = src->width * bpp;
  if (src->row_pitch < row_bytes || dst->row_pitch < row_bytes) {
    return E_FAIL;
  }
  if (src->storage.size() < static_cast<size_t>(src->row_pitch) * src->height ||
      dst->storage.size() < static_cast<size_t>(dst->row_pitch) * dst->height) {
    return E_FAIL;
  }

  const uint8_t* src_base = src->storage.data();
  uint8_t* dst_base = dst->storage.data();
  for (uint32_t y = 0; y < src->height; y++) {
    std::memcpy(dst_base + static_cast<size_t>(y) * dst->row_pitch,
                src_base + static_cast<size_t>(y) * src->row_pitch,
                row_bytes);
  }
  return S_OK;
}

// -----------------------------------------------------------------------------
// Adapter DDIs
// -----------------------------------------------------------------------------

uint64_t luid_to_u64(const LUID& luid) {
  const uint64_t hi = static_cast<uint64_t>(static_cast<uint32_t>(luid.HighPart));
  const uint64_t lo = static_cast<uint64_t>(luid.LowPart);
  return (hi << 32) | lo;
}

LUID default_luid() {
  LUID luid{};
  luid.LowPart = 0;
  luid.HighPart = 0;
  return luid;
}

std::mutex g_adapter_cache_mutex;
std::unordered_map<uint64_t, Adapter*> g_adapter_cache;

Adapter* acquire_adapter(const LUID& luid,
                         UINT interface_version,
                         UINT umd_version,
                         D3DDDI_ADAPTERCALLBACKS* callbacks,
                         D3DDDI_ADAPTERCALLBACKS2* callbacks2) {
  std::lock_guard<std::mutex> lock(g_adapter_cache_mutex);

  const uint64_t key = luid_to_u64(luid);
  auto it = g_adapter_cache.find(key);
  if (it != g_adapter_cache.end()) {
    Adapter* adapter = it->second;
    adapter->open_count.fetch_add(1);
    adapter->interface_version = interface_version;
    adapter->umd_version = umd_version;
    adapter->adapter_callbacks = callbacks;
    adapter->adapter_callbacks2 = callbacks2;
    adapter->share_token_allocator.set_adapter_luid(luid);
    if (callbacks) {
      adapter->adapter_callbacks_copy = *callbacks;
      adapter->adapter_callbacks_valid = true;
    } else {
      adapter->adapter_callbacks_copy = {};
      adapter->adapter_callbacks_valid = false;
    }
    if (callbacks2) {
      adapter->adapter_callbacks2_copy = *callbacks2;
      adapter->adapter_callbacks2_valid = true;
    } else {
      adapter->adapter_callbacks2_copy = {};
      adapter->adapter_callbacks2_valid = false;
    }
    return adapter;
  }

  auto* adapter = new Adapter();
  adapter->luid = luid;
  adapter->share_token_allocator.set_adapter_luid(luid);
  adapter->open_count.store(1);
  adapter->interface_version = interface_version;
  adapter->umd_version = umd_version;
  adapter->adapter_callbacks = callbacks;
  adapter->adapter_callbacks2 = callbacks2;
  if (callbacks) {
    adapter->adapter_callbacks_copy = *callbacks;
    adapter->adapter_callbacks_valid = true;
  } else {
    adapter->adapter_callbacks_copy = {};
    adapter->adapter_callbacks_valid = false;
  }
  if (callbacks2) {
    adapter->adapter_callbacks2_copy = *callbacks2;
    adapter->adapter_callbacks2_valid = true;
  } else {
    adapter->adapter_callbacks2_copy = {};
    adapter->adapter_callbacks2_valid = false;
  }

#if defined(_WIN32)
  // Initialize a best-effort primary display mode so GetDisplayModeEx returns a
  // stable value even when the runtime opens the adapter via the LUID path (as
  // DWM commonly does).
  const int w = GetSystemMetrics(SM_CXSCREEN);
  const int h = GetSystemMetrics(SM_CYSCREEN);
  if (w > 0) {
    adapter->primary_width = static_cast<uint32_t>(w);
  }
  if (h > 0) {
    adapter->primary_height = static_cast<uint32_t>(h);
  }

  DEVMODEA dm{};
  dm.dmSize = sizeof(dm);
  if (EnumDisplaySettingsA(nullptr, ENUM_CURRENT_SETTINGS, &dm)) {
    if (dm.dmPelsWidth > 0) {
      adapter->primary_width = static_cast<uint32_t>(dm.dmPelsWidth);
    }
    if (dm.dmPelsHeight > 0) {
      adapter->primary_height = static_cast<uint32_t>(dm.dmPelsHeight);
    }
    if (dm.dmDisplayFrequency > 0) {
      adapter->primary_refresh_hz = static_cast<uint32_t>(dm.dmDisplayFrequency);
    }
  }
#endif

  g_adapter_cache.emplace(key, adapter);
  return adapter;
}

void release_adapter(Adapter* adapter) {
  if (!adapter) {
    return;
  }

  std::lock_guard<std::mutex> lock(g_adapter_cache_mutex);
  const uint32_t remaining = adapter->open_count.fetch_sub(1) - 1;
  if (remaining != 0) {
    return;
  }

  g_adapter_cache.erase(luid_to_u64(adapter->luid));

#if defined(_WIN32)
  // Release cross-process alloc_id token allocator state.
  {
    std::lock_guard<std::mutex> share_lock(adapter->share_token_mutex);
    if (adapter->share_token_view) {
      UnmapViewOfFile(adapter->share_token_view);
      adapter->share_token_view = nullptr;
    }
    if (adapter->share_token_mapping) {
      CloseHandle(adapter->share_token_mapping);
      adapter->share_token_mapping = nullptr;
    }
  }
#endif
  delete adapter;
}

HRESULT AEROGPU_D3D9_CALL adapter_close(D3D9DDI_HADAPTER hAdapter) {
  D3d9TraceCall trace(D3d9TraceFunc::AdapterClose, d3d9_trace_arg_ptr(hAdapter.pDrvPrivate), 0, 0, 0);
  release_adapter(as_adapter(hAdapter));
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL adapter_get_caps(
    D3D9DDI_HADAPTER hAdapter,
    const D3D9DDIARG_GETCAPS* pGetCaps) {
  D3d9TraceCall trace(D3d9TraceFunc::AdapterGetCaps,
                      d3d9_trace_arg_ptr(hAdapter.pDrvPrivate),
                      pGetCaps ? static_cast<uint64_t>(pGetCaps->Type) : 0,
                      pGetCaps ? static_cast<uint64_t>(pGetCaps->DataSize) : 0,
                      pGetCaps ? d3d9_trace_arg_ptr(pGetCaps->pData) : 0);
  auto* adapter = as_adapter(hAdapter);
  if (!adapter || !pGetCaps) {
    return trace.ret(E_INVALIDARG);
  }

  AEROGPU_D3D9DDIARG_GETCAPS args{};
  args.type = static_cast<uint32_t>(pGetCaps->Type);
  args.pData = pGetCaps->pData;
  args.data_size = pGetCaps->DataSize;
  return trace.ret(aerogpu::get_caps(adapter, &args));
}

HRESULT AEROGPU_D3D9_CALL adapter_query_adapter_info(
    D3D9DDI_HADAPTER hAdapter,
    const D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo) {
  uint64_t data_ptr = 0;
  uint32_t size = 0;
  if (pQueryAdapterInfo) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
    data_ptr = d3d9_trace_arg_ptr(pQueryAdapterInfo->pPrivateDriverData);
    size = pQueryAdapterInfo->PrivateDriverDataSize;
#else
    data_ptr = d3d9_trace_arg_ptr(pQueryAdapterInfo->pData);
    size = pQueryAdapterInfo->DataSize;
#endif
  }

  D3d9TraceCall trace(D3d9TraceFunc::AdapterQueryAdapterInfo,
                      d3d9_trace_arg_ptr(hAdapter.pDrvPrivate),
                      pQueryAdapterInfo ? static_cast<uint64_t>(pQueryAdapterInfo->Type) : 0,
                      static_cast<uint64_t>(size),
                      data_ptr);

  auto* adapter = as_adapter(hAdapter);
  if (!adapter || !pQueryAdapterInfo) {
    return trace.ret(E_INVALIDARG);
  }

  void* data = nullptr;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  data = pQueryAdapterInfo->pPrivateDriverData;
  size = pQueryAdapterInfo->PrivateDriverDataSize;
#else
  data = pQueryAdapterInfo->pData;
  size = pQueryAdapterInfo->DataSize;
#endif

  AEROGPU_D3D9DDIARG_QUERYADAPTERINFO args{};
  args.type = static_cast<uint32_t>(pQueryAdapterInfo->Type);
  args.pPrivateDriverData = data;
  args.private_driver_data_size = size;
  return trace.ret(aerogpu::query_adapter_info(adapter, &args));
}

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    D3D9DDI_DEVICEFUNCS* pDeviceFuncs);

// -----------------------------------------------------------------------------
// Device DDIs
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL device_destroy(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroy, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(S_OK);
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    // Tear down internal objects that the runtime does not know about.
    if (dev->fvf_vertex_decl) {
      (void)emit_destroy_input_layout_locked(dev, dev->fvf_vertex_decl->handle);
      delete dev->fvf_vertex_decl;
      dev->fvf_vertex_decl = nullptr;
    }
    if (dev->fixedfunc_vs) {
      (void)emit_destroy_shader_locked(dev, dev->fixedfunc_vs->handle);
      delete dev->fixedfunc_vs;
      dev->fixedfunc_vs = nullptr;
    }
    if (dev->fixedfunc_ps) {
      (void)emit_destroy_shader_locked(dev, dev->fixedfunc_ps->handle);
      delete dev->fixedfunc_ps;
      dev->fixedfunc_ps = nullptr;
    }
    if (dev->up_vertex_buffer) {
      (void)emit_destroy_resource_locked(dev, dev->up_vertex_buffer->handle);
      delete dev->up_vertex_buffer;
      dev->up_vertex_buffer = nullptr;
    }
    if (dev->up_index_buffer) {
      (void)emit_destroy_resource_locked(dev, dev->up_index_buffer->handle);
      delete dev->up_index_buffer;
      dev->up_index_buffer = nullptr;
    }

    destroy_blit_objects_locked(dev);
    for (SwapChain* sc : dev->swapchains) {
      if (!sc) {
        continue;
      }
      for (Resource* bb : sc->backbuffers) {
        if (!bb) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, bb->handle);
        delete bb;
      }
      delete sc;
    }
    dev->swapchains.clear();
    dev->current_swapchain = nullptr;
    flush_locked(dev);
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  dev->wddm_context.destroy(dev->wddm_callbacks);
  wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
  dev->wddm_device = 0;
#endif
  delete dev;
  return trace.ret(S_OK);
}

static void consume_wddm_alloc_priv(Resource* res,
                                   const void* priv_data,
                                   uint32_t priv_data_size,
                                   bool is_shared_resource) {
  if (!res || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return;
  }

  aerogpu_wddm_alloc_priv priv{};
  std::memcpy(&priv, priv_data, sizeof(priv));

  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC || priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION) {
    return;
  }

  res->backing_alloc_id = priv.alloc_id;
  res->share_token = priv.share_token;
  if (res->size_bytes == 0 && priv.size_bytes != 0 && priv.size_bytes <= 0xFFFFFFFFull) {
    res->size_bytes = static_cast<uint32_t>(priv.size_bytes);
  }
  if (priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) {
    res->is_shared = true;
  }

  // For compatibility, derive a stable token if share_token is missing.
  if (is_shared_resource && res->share_token == 0 && res->backing_alloc_id != 0) {
    res->share_token = static_cast<uint64_t>(res->backing_alloc_id);
  }
}

HRESULT create_backbuffer_locked(Device* dev, Resource* res, uint32_t format, uint32_t width, uint32_t height) {
  if (!dev || !dev->adapter || !res) {
    return E_INVALIDARG;
  }

  const uint32_t bpp = bytes_per_pixel(format);
  width = std::max(1u, width);
  height = std::max(1u, height);

  res->handle = allocate_global_handle(dev->adapter);
  res->kind = ResourceKind::Surface;
  res->type = 0;
  res->format = format;
  res->width = width;
  res->height = height;
  res->depth = 1;
  res->mip_levels = 1;
  res->usage = kD3DUsageRenderTarget;
  res->pool = kD3DPOOL_DEFAULT;
  res->backing_alloc_id = 0;
  res->share_token = 0;
  res->is_shared = false;
  res->is_shared_alias = false;
  res->wddm_hAllocation = 0;
  res->row_pitch = width * bpp;
  res->slice_pitch = res->row_pitch * height;
  res->locked = false;
  res->locked_offset = 0;
  res->locked_size = 0;
  res->locked_flags = 0;

  uint64_t total = static_cast<uint64_t>(res->slice_pitch);
  if (total > 0x7FFFFFFFu) {
    return E_OUTOFMEMORY;
  }
  res->size_bytes = static_cast<uint32_t>(total);

  try {
    res->storage.resize(res->size_bytes);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_create_resource_locked(dev, res)) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_create_resource(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_CREATERESOURCE* pCreateResource) {
  const uint64_t type_format =
      pCreateResource ? d3d9_trace_pack_u32_u32(pCreateResource->type, pCreateResource->format) : 0;
  const uint64_t wh = pCreateResource ? d3d9_trace_pack_u32_u32(pCreateResource->width, pCreateResource->height) : 0;
  const uint64_t usage_pool =
      pCreateResource ? d3d9_trace_pack_u32_u32(pCreateResource->usage, pCreateResource->pool) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceCreateResource, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), type_format, wh, usage_pool);
  if (!hDevice.pDrvPrivate || !pCreateResource) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool wants_shared = (pCreateResource->pSharedHandle != nullptr);
  const bool open_existing_shared = wants_shared && (*pCreateResource->pSharedHandle != nullptr);
  const uint32_t requested_mip_levels = pCreateResource->mip_levels;
  const uint32_t mip_levels = std::max(1u, requested_mip_levels);
  if (wants_shared && requested_mip_levels != 1) {
    // MVP: shared surfaces must be single-allocation (no mip chains/arrays).
    return trace.ret(D3DERR_INVALIDCALL);
  }

  auto res = std::make_unique<Resource>();
  res->handle = allocate_global_handle(dev->adapter);
  res->type = pCreateResource->type;
  res->format = pCreateResource->format;
  res->width = pCreateResource->width;
  res->height = pCreateResource->height;
  res->depth = std::max(1u, pCreateResource->depth);
  res->mip_levels = mip_levels;
  res->usage = pCreateResource->usage;
  res->pool = pCreateResource->pool;
  res->wddm_hAllocation = static_cast<WddmAllocationHandle>(pCreateResource->wddm_hAllocation);
  res->is_shared = wants_shared;
  res->is_shared_alias = open_existing_shared;

  consume_wddm_alloc_priv(res.get(),
                          pCreateResource->pKmdAllocPrivateData,
                          pCreateResource->KmdAllocPrivateDataSize,
                          wants_shared);

  // Heuristic: if size is provided, treat as buffer; otherwise treat as a 2D image.
  if (pCreateResource->size) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pCreateResource->size;
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else if (res->width && res->height) {
    // Surface/Texture2D share the same storage layout for now.
    res->kind = (res->mip_levels > 1) ? ResourceKind::Texture2D : ResourceKind::Surface;

    const uint32_t bpp = bytes_per_pixel(res->format);
    uint32_t w = std::max(1u, res->width);
    uint32_t h = std::max(1u, res->height);

    res->row_pitch = w * bpp;
    res->slice_pitch = res->row_pitch * h;

    uint64_t total = 0;
    for (uint32_t level = 0; level < res->mip_levels; level++) {
      total += static_cast<uint64_t>(std::max(1u, w)) * static_cast<uint64_t>(std::max(1u, h)) * bpp;
      w = std::max(1u, w / 2);
      h = std::max(1u, h / 2);
    }
    total *= res->depth;
    if (total > 0x7FFFFFFFu) {
      return trace.ret(E_OUTOFMEMORY);
    }
    res->size_bytes = static_cast<uint32_t>(total);
  } else {
    return trace.ret(E_INVALIDARG);
  }

  if (res->pool != kD3DPOOL_SYSTEMMEM && res->kind != ResourceKind::Buffer) {
    if (d3d9_format_to_aerogpu(res->format) == AEROGPU_FORMAT_INVALID) {
      return trace.ret(D3DERR_INVALIDCALL);
    }
  }

  try {
    res->storage.resize(res->size_bytes);
  } catch (...) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // System-memory pool resources are CPU-only: the host does not need a backing
  // GPU object for readback destinations.
  if (res->pool == kD3DPOOL_SYSTEMMEM) {
    if (wants_shared) {
      return trace.ret(D3DERR_INVALIDCALL);
    }
    res->handle = 0;
    pCreateResource->hResource.pDrvPrivate = res.release();
    return trace.ret(S_OK);
  }

  // On the real WDDM path we want GPU resources to be backed by WDDM allocations
  // and referenced in the command stream via a stable per-allocation `alloc_id`
  // (carried in aerogpu_wddm_alloc_priv and resolved via the per-submit allocation
  // table).
  if (!wants_shared && dev->wddm_context.hContext != 0) {
    if (!res->backing_alloc_id) {
      if (!pCreateResource->pKmdAllocPrivateData ||
          pCreateResource->KmdAllocPrivateDataSize < sizeof(aerogpu_wddm_alloc_priv)) {
        logf("aerogpu-d3d9: CreateResource missing private data buffer (have=%u need=%u)\n",
             pCreateResource->KmdAllocPrivateDataSize,
             static_cast<unsigned>(sizeof(aerogpu_wddm_alloc_priv)));
        return trace.ret(D3DERR_INVALIDCALL);
      }

      // Use the same cross-process allocator as shared surfaces so alloc_id values
      // never collide within a submission (DWM can reference shared + non-shared
      // allocations together).
      uint64_t alloc_token = 0;
      uint32_t alloc_id = 0;
      do {
        alloc_token = allocate_shared_alloc_id_token(dev->adapter);
        alloc_id = static_cast<uint32_t>(alloc_token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      } while (alloc_token != 0 && alloc_id == 0);

      if (!alloc_token || !alloc_id) {
        logf("aerogpu-d3d9: Failed to allocate alloc_id for non-shared resource (token=%llu alloc_id=%u)\n",
             static_cast<unsigned long long>(alloc_token),
             static_cast<unsigned>(alloc_id));
        return E_FAIL;
      }

      aerogpu_wddm_alloc_priv priv{};
      priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
      priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
      priv.alloc_id = alloc_id;
      priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE;
      priv.share_token = 0;
      priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
      priv.reserved0 = 0;
      std::memcpy(pCreateResource->pKmdAllocPrivateData, &priv, sizeof(priv));

      res->backing_alloc_id = alloc_id;
      res->backing_offset_bytes = 0;
      res->share_token = 0;
    }
  }

  if (wants_shared && !open_existing_shared) {
    if (!pCreateResource->pKmdAllocPrivateData ||
        pCreateResource->KmdAllocPrivateDataSize < sizeof(aerogpu_wddm_alloc_priv)) {
      logf("aerogpu-d3d9: Create shared resource missing private data buffer (have=%u need=%u)\n",
           pCreateResource->KmdAllocPrivateDataSize,
           static_cast<unsigned>(sizeof(aerogpu_wddm_alloc_priv)));
      return trace.ret(D3DERR_INVALIDCALL);
    }

    // Allocate a stable cross-process alloc_id (31-bit) and a collision-resistant
    // share_token (64-bit) and persist them in allocation private data so they
    // survive OpenResource/OpenAllocation in another process.
    //
    // NOTE: DWM may compose many shared surfaces from *different* processes in a
    // single submission. alloc_id values must therefore avoid collisions across
    // guest processes (not just within one process). share_token must also be
    // collision-resistant across the entire guest because the host maintains a
    // global (share_token -> resource) table.
    uint32_t alloc_id = 0;
    {
      // `allocate_shared_alloc_id_token()` provides a monotonic 64-bit counter shared
      // across guest processes (best effort). Derive a 31-bit alloc_id from it.
      uint64_t alloc_token = 0;
      do {
        alloc_token = allocate_shared_alloc_id_token(dev->adapter);
        alloc_id = static_cast<uint32_t>(alloc_token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      } while (alloc_token != 0 && alloc_id == 0);

      if (!alloc_token || !alloc_id) {
        logf("aerogpu-d3d9: Failed to allocate shared alloc_id (token=%llu alloc_id=%u)\n",
             static_cast<unsigned long long>(alloc_token),
             static_cast<unsigned>(alloc_id));
        return trace.ret(E_FAIL);
      }
    }

    const uint64_t share_token = dev->adapter->share_token_allocator.allocate_share_token();

    aerogpu_wddm_alloc_priv priv{};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv.alloc_id = alloc_id;
    priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
    priv.share_token = share_token;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv.reserved0 = 0;
    std::memcpy(pCreateResource->pKmdAllocPrivateData, &priv, sizeof(priv));

    res->backing_alloc_id = alloc_id;
    res->share_token = share_token;
  }

  if (open_existing_shared) {
    if (!res->share_token) {
      logf("aerogpu-d3d9: Open shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      return trace.ret(E_FAIL);
    }
    // Shared surface open (D3D9Ex): the host already has the original resource,
    // so we only create a new alias handle and IMPORT it.
    if (!emit_import_shared_surface_locked(dev, res.get())) {
      return trace.ret(E_OUTOFMEMORY);
    }
  } else {
    if (!emit_create_resource_locked(dev, res.get())) {
      return trace.ret(E_OUTOFMEMORY);
    }

    if (res->is_shared) {
      if (!res->share_token) {
        logf("aerogpu-d3d9: Create shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      } else {
        // Shared surface create (D3D9Ex): export exactly once so other guest
        // processes can IMPORT using the same stable share_token.
        if (!emit_export_shared_surface_locked(dev, res.get())) {
          return trace.ret(E_OUTOFMEMORY);
        }

        // Shared surfaces must be importable by other processes immediately
        // after CreateResource returns. Since AeroGPU resource creation is
        // expressed in the command stream, force a submission so the host
        // observes the export.
        submit(dev);

        logf("aerogpu-d3d9: export shared_surface res=%u token=%llu\n",
             res->handle,
             static_cast<unsigned long long>(res->share_token));
      }
    }
  }

  pCreateResource->hResource.pDrvPrivate = res.release();
  return trace.ret(S_OK);
}

static HRESULT device_open_resource_impl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  if (!hDevice.pDrvPrivate || !pOpenResource) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  if (!pOpenResource->pPrivateDriverData ||
      pOpenResource->private_driver_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return E_INVALIDARG;
  }

  aerogpu_wddm_alloc_priv priv{};
  std::memcpy(&priv, pOpenResource->pPrivateDriverData, sizeof(priv));
  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC || priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION) {
    return E_INVALIDARG;
  }
  if ((priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) == 0 || priv.share_token == 0 || priv.alloc_id == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto res = std::make_unique<Resource>();
  res->handle = allocate_global_handle(dev->adapter);

  res->is_shared = true;
  res->is_shared_alias = true;
  res->share_token = priv.share_token;
  res->backing_alloc_id = priv.alloc_id;
  res->backing_offset_bytes = 0;

  res->type = pOpenResource->type;
  res->format = pOpenResource->format;
  res->width = pOpenResource->width;
  res->height = pOpenResource->height;
  res->depth = std::max(1u, pOpenResource->depth);
  res->mip_levels = std::max(1u, pOpenResource->mip_levels);
  res->usage = pOpenResource->usage;

  // Prefer a reconstructed size when the runtime provides a description; fall
  // back to the size_bytes persisted in allocation private data.
  if (pOpenResource->size) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pOpenResource->size;
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else if (res->width && res->height) {
    res->kind = (res->mip_levels > 1) ? ResourceKind::Texture2D : ResourceKind::Surface;

    const uint32_t bpp = bytes_per_pixel(res->format);
    uint32_t w = std::max(1u, res->width);
    uint32_t h = std::max(1u, res->height);

    res->row_pitch = w * bpp;
    res->slice_pitch = res->row_pitch * h;

    uint64_t total = 0;
    for (uint32_t level = 0; level < res->mip_levels; level++) {
      total += static_cast<uint64_t>(std::max(1u, w)) * static_cast<uint64_t>(std::max(1u, h)) * bpp;
      w = std::max(1u, w / 2);
      h = std::max(1u, h / 2);
    }
    total *= res->depth;
    if (total > 0x7FFFFFFFu) {
      return E_OUTOFMEMORY;
    }
    res->size_bytes = static_cast<uint32_t>(total);
  } else if (priv.size_bytes != 0 && priv.size_bytes <= 0x7FFFFFFFu) {
    res->kind = ResourceKind::Surface;
    res->size_bytes = static_cast<uint32_t>(priv.size_bytes);
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else {
    return E_INVALIDARG;
  }

  if (res->kind != ResourceKind::Buffer) {
    if (d3d9_format_to_aerogpu(res->format) == AEROGPU_FORMAT_INVALID) {
      return E_INVALIDARG;
    }
  }

  if (!res->size_bytes) {
    return E_INVALIDARG;
  }

  try {
    res->storage.resize(res->size_bytes);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_import_shared_surface_locked(dev, res.get())) {
    return E_OUTOFMEMORY;
  }

  logf("aerogpu-d3d9: import shared_surface out_res=%u token=%llu alloc_id=%u\n",
       res->handle,
       static_cast<unsigned long long>(res->share_token),
       static_cast<unsigned>(res->backing_alloc_id));

  pOpenResource->hResource.pDrvPrivate = res.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_open_resource(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  const uint64_t type_format = pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->type, pOpenResource->format) : 0;
  const uint64_t wh = pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->width, pOpenResource->height) : 0;
  const uint64_t usage_priv =
      pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->usage, pOpenResource->private_driver_data_size) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceOpenResource, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), type_format, wh, usage_priv);
  return trace.ret(device_open_resource_impl(hDevice, pOpenResource));
}

HRESULT AEROGPU_D3D9_CALL device_open_resource2(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  const uint64_t type_format = pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->type, pOpenResource->format) : 0;
  const uint64_t wh = pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->width, pOpenResource->height) : 0;
  const uint64_t usage_priv =
      pOpenResource ? d3d9_trace_pack_u32_u32(pOpenResource->usage, pOpenResource->private_driver_data_size) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceOpenResource2, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), type_format, wh, usage_priv);
  return trace.ret(device_open_resource_impl(hDevice, pOpenResource));
}

HRESULT AEROGPU_D3D9_CALL device_destroy_resource(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hResource) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyResource,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hResource.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* res = as_resource(hResource);
  if (!dev || !res) {
    delete res;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (SwapChain* sc : dev->swapchains) {
    if (!sc) {
      continue;
    }
    auto& bbs = sc->backbuffers;
    bbs.erase(std::remove(bbs.begin(), bbs.end(), res), bbs.end());
  }

  // Defensive: DWM and other D3D9Ex clients can destroy resources while they are
  // still bound. Clear any cached bindings that point at the resource before we
  // delete it so subsequent command emission does not dereference a dangling
  // pointer.
  bool rt_changed = false;
  for (uint32_t i = 0; i < 4; ++i) {
    if (dev->render_targets[i] == res) {
      dev->render_targets[i] = nullptr;
      rt_changed = true;
    }
  }
  if (dev->depth_stencil == res) {
    dev->depth_stencil = nullptr;
    rt_changed = true;
  }

  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (dev->textures[stage] != res) {
      continue;
    }
    dev->textures[stage] = nullptr;
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (dev->streams[stream].vb != res) {
      continue;
    }
    dev->streams[stream] = {};

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = 0;
    binding.stride_bytes = 0;
    binding.offset_bytes = 0;
    binding.reserved0 = 0;

    if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
            dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
      cmd->start_slot = stream;
      cmd->buffer_count = 1;
    }
  }

  if (dev->index_buffer == res) {
    dev->index_buffer = nullptr;
    dev->index_offset_bytes = 0;
    dev->index_format = AEROGPU_D3D9DDI_INDEX_FORMAT_U16;

    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
      cmd->buffer = 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = 0;
      cmd->reserved0 = 0;
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }
  // For shared resources, the host maintains its own refcount across alias
  // handles, so it is safe (and important) to emit DESTROY_RESOURCE on
  // per-process close.
  (void)emit_destroy_resource_locked(dev, res->handle);
  delete res;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_swap_chain(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_CREATESWAPCHAIN* pCreateSwapChain) {
  const uint64_t bb_wh =
      pCreateSwapChain ? d3d9_trace_pack_u32_u32(pCreateSwapChain->present_params.backbuffer_width,
                                                 pCreateSwapChain->present_params.backbuffer_height)
                       : 0;
  const uint64_t fmt_count =
      pCreateSwapChain ? d3d9_trace_pack_u32_u32(pCreateSwapChain->present_params.backbuffer_format,
                                                 pCreateSwapChain->present_params.backbuffer_count)
                       : 0;
  const uint64_t interval_flags =
      pCreateSwapChain ? d3d9_trace_pack_u32_u32(pCreateSwapChain->present_params.presentation_interval,
                                                 pCreateSwapChain->present_params.flags)
                       : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceCreateSwapChain, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  if (!hDevice.pDrvPrivate || !pCreateSwapChain) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  const auto& pp = pCreateSwapChain->present_params;
  if (d3d9_format_to_aerogpu(pp.backbuffer_format) == AEROGPU_FORMAT_INVALID) {
    return trace.ret(E_INVALIDARG);
  }

  const uint32_t width = pp.backbuffer_width ? pp.backbuffer_width : 1u;
  const uint32_t height = pp.backbuffer_height ? pp.backbuffer_height : 1u;
  const uint32_t backbuffer_count = std::max(1u, pp.backbuffer_count);

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto sc = std::make_unique<SwapChain>();
  sc->handle = allocate_global_handle(dev->adapter);
  sc->hwnd = pp.hDeviceWindow;
  sc->width = width;
  sc->height = height;
  sc->format = pp.backbuffer_format;
  sc->sync_interval = pp.presentation_interval;
  sc->swap_effect = pp.swap_effect;
  sc->flags = pp.flags;

  sc->backbuffers.reserve(backbuffer_count);
  for (uint32_t i = 0; i < backbuffer_count; i++) {
    auto bb = std::make_unique<Resource>();
    HRESULT hr = create_backbuffer_locked(dev, bb.get(), sc->format, sc->width, sc->height);
    if (hr < 0) {
      for (Resource* created : sc->backbuffers) {
        if (!created) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, created->handle);
        delete created;
      }
      return trace.ret(hr);
    }
    sc->backbuffers.push_back(bb.release());
  }

  Resource* first_backbuffer = sc->backbuffers.empty() ? nullptr : sc->backbuffers[0];

  // Default D3D9 behavior: the first backbuffer is bound as render target 0.
  if (!dev->render_targets[0] && first_backbuffer) {
    dev->render_targets[0] = first_backbuffer;
    if (!emit_set_render_targets_locked(dev)) {
      // Keep driver state consistent with the host by rolling back the implicit
      // binding and tearing down the partially-created swapchain.
      dev->render_targets[0] = nullptr;
      for (Resource* created : sc->backbuffers) {
        if (!created) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, created->handle);
        delete created;
      }
      return trace.ret(E_OUTOFMEMORY);
    }
  }

  pCreateSwapChain->hBackBuffer.pDrvPrivate = first_backbuffer;
  pCreateSwapChain->hSwapChain.pDrvPrivate = sc.get();

  dev->swapchains.push_back(sc.release());
  if (!dev->current_swapchain) {
    dev->current_swapchain = dev->swapchains.back();
  }

  return trace.ret(S_OK);
}

HRESULT copy_surface_rects(const Resource* src, Resource* dst, const RECT* rects, uint32_t rect_count) {
  if (!rects || rect_count == 0) {
    return copy_surface_bytes(src, dst);
  }
  if (!src || !dst) {
    return E_INVALIDARG;
  }
  if (src->format != dst->format) {
    return E_INVALIDARG;
  }

  const uint32_t bpp = bytes_per_pixel(src->format);

  for (uint32_t i = 0; i < rect_count; ++i) {
    const RECT& r = rects[i];
    if (r.right <= r.left || r.bottom <= r.top) {
      continue;
    }

    const uint32_t left = static_cast<uint32_t>(std::max<long>(0, r.left));
    const uint32_t top = static_cast<uint32_t>(std::max<long>(0, r.top));
    const uint32_t right = static_cast<uint32_t>(std::max<long>(0, r.right));
    const uint32_t bottom = static_cast<uint32_t>(std::max<long>(0, r.bottom));

    const uint32_t clamped_right = std::min<uint32_t>({right, src->width, dst->width});
    const uint32_t clamped_bottom = std::min<uint32_t>({bottom, src->height, dst->height});

    if (left >= clamped_right || top >= clamped_bottom) {
      continue;
    }

    const uint32_t row_bytes = (clamped_right - left) * bpp;
    for (uint32_t y = top; y < clamped_bottom; ++y) {
      const size_t src_off = static_cast<size_t>(y) * src->row_pitch + static_cast<size_t>(left) * bpp;
      const size_t dst_off = static_cast<size_t>(y) * dst->row_pitch + static_cast<size_t>(left) * bpp;
      if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
        return E_INVALIDARG;
      }
      std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
    }
  }

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_swap_chain(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroySwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSwapChain.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* sc = as_swapchain(hSwapChain);
  if (!dev || !sc) {
    delete sc;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
  if (it != dev->swapchains.end()) {
    dev->swapchains.erase(it);
  }
  if (dev->current_swapchain == sc) {
    dev->current_swapchain = dev->swapchains.empty() ? nullptr : dev->swapchains[0];
  }

  bool rt_changed = false;
  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    for (uint32_t i = 0; i < 4; ++i) {
      if (dev->render_targets[i] == bb) {
        dev->render_targets[i] = nullptr;
        rt_changed = true;
      }
    }
    if (dev->depth_stencil == bb) {
      dev->depth_stencil = nullptr;
      rt_changed = true;
    }

    for (uint32_t stage = 0; stage < 16; ++stage) {
      if (dev->textures[stage] != bb) {
        continue;
      }
      dev->textures[stage] = nullptr;
      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
        cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
        cmd->slot = stage;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }

    for (uint32_t stream = 0; stream < 16; ++stream) {
      if (dev->streams[stream].vb != bb) {
        continue;
      }
      dev->streams[stream] = {};

      aerogpu_vertex_buffer_binding binding{};
      binding.buffer = 0;
      binding.stride_bytes = 0;
      binding.offset_bytes = 0;
      binding.reserved0 = 0;

      if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
              dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
        cmd->start_slot = stream;
        cmd->buffer_count = 1;
      }
    }

    if (dev->index_buffer == bb) {
      dev->index_buffer = nullptr;
      dev->index_offset_bytes = 0;
      dev->index_format = AEROGPU_D3D9DDI_INDEX_FORMAT_U16;

      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
        cmd->buffer = 0;
        cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
        cmd->offset_bytes = 0;
        cmd->reserved0 = 0;
      }
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }

  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    (void)emit_destroy_resource_locked(dev, bb->handle);
    delete bb;
  }

  delete sc;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_swap_chain(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t index,
    AEROGPU_D3D9DDI_HSWAPCHAIN* phSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetSwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(index),
                      d3d9_trace_arg_ptr(phSwapChain),
                      0);
  if (!hDevice.pDrvPrivate || !phSwapChain) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (index >= dev->swapchains.size()) {
    phSwapChain->pDrvPrivate = nullptr;
    return trace.ret(E_INVALIDARG);
  }
  phSwapChain->pDrvPrivate = dev->swapchains[index];
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_swap_chain(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetSwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSwapChain.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  auto* sc = as_swapchain(hSwapChain);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (sc) {
    auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
    if (it == dev->swapchains.end()) {
      return trace.ret(E_INVALIDARG);
    }
  }
  dev->current_swapchain = sc;
  return trace.ret(S_OK);
}

HRESULT reset_swap_chain_locked(Device* dev, SwapChain* sc, const AEROGPU_D3D9DDI_PRESENT_PARAMETERS& pp) {
  if (!dev || !dev->adapter || !sc) {
    return E_INVALIDARG;
  }

  if (d3d9_format_to_aerogpu(pp.backbuffer_format) == AEROGPU_FORMAT_INVALID) {
    return E_INVALIDARG;
  }

  const uint32_t new_width = pp.backbuffer_width ? pp.backbuffer_width : sc->width;
  const uint32_t new_height = pp.backbuffer_height ? pp.backbuffer_height : sc->height;
  const uint32_t new_count = std::max(1u, pp.backbuffer_count);

  sc->hwnd = pp.hDeviceWindow ? pp.hDeviceWindow : sc->hwnd;
  sc->width = new_width;
  sc->height = new_height;
  sc->format = pp.backbuffer_format;
  sc->sync_interval = pp.presentation_interval;
  sc->swap_effect = pp.swap_effect;
  sc->flags = pp.flags;

  // Grow/shrink backbuffer array if needed.
  std::vector<Resource*> removed_backbuffers;
  while (sc->backbuffers.size() > new_count) {
    removed_backbuffers.push_back(sc->backbuffers.back());
    sc->backbuffers.pop_back();
  }

  bool rt_changed = false;
  for (Resource* bb : removed_backbuffers) {
    if (!bb) {
      continue;
    }
    for (uint32_t i = 0; i < 4; ++i) {
      if (dev->render_targets[i] == bb) {
        dev->render_targets[i] = nullptr;
        rt_changed = true;
      }
    }
    if (dev->depth_stencil == bb) {
      dev->depth_stencil = nullptr;
      rt_changed = true;
    }

    for (uint32_t stage = 0; stage < 16; ++stage) {
      if (dev->textures[stage] != bb) {
        continue;
      }
      dev->textures[stage] = nullptr;
      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
        cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
        cmd->slot = stage;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }

    for (uint32_t stream = 0; stream < 16; ++stream) {
      if (dev->streams[stream].vb != bb) {
        continue;
      }
      dev->streams[stream] = {};

      aerogpu_vertex_buffer_binding binding{};
      binding.buffer = 0;
      binding.stride_bytes = 0;
      binding.offset_bytes = 0;
      binding.reserved0 = 0;

      if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
              dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
        cmd->start_slot = stream;
        cmd->buffer_count = 1;
      }
    }

    if (dev->index_buffer == bb) {
      dev->index_buffer = nullptr;
      dev->index_offset_bytes = 0;
      dev->index_format = AEROGPU_D3D9DDI_INDEX_FORMAT_U16;

      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
        cmd->buffer = 0;
        cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
        cmd->offset_bytes = 0;
        cmd->reserved0 = 0;
      }
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }

  for (Resource* bb : removed_backbuffers) {
    if (!bb) {
      continue;
    }
    emit_destroy_resource_locked(dev, bb->handle);
    delete bb;
  }
  while (sc->backbuffers.size() < new_count) {
    auto bb = std::make_unique<Resource>();
    HRESULT hr = create_backbuffer_locked(dev, bb.get(), sc->format, sc->width, sc->height);
    if (hr < 0) {
      return hr;
    }
    sc->backbuffers.push_back(bb.release());
  }

  // Recreate backbuffer storage/handles.
  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    (void)emit_destroy_resource_locked(dev, bb->handle);
    HRESULT hr = create_backbuffer_locked(dev, bb, sc->format, sc->width, sc->height);
    if (hr < 0) {
      return hr;
    }
  }

  auto is_backbuffer = [sc](const Resource* res) -> bool {
    if (!sc || !res) {
      return false;
    }
    return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
  };

  // Reset recreates swapchain backbuffer handles. If any of the backbuffers are
  // currently bound via other state (textures / IA bindings), re-emit the bind
  // commands so the host uses the updated handles.
  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (!is_backbuffer(dev->textures[stage])) {
      continue;
    }
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
      cmd->reserved0 = 0;
    }
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (!is_backbuffer(dev->streams[stream].vb)) {
      continue;
    }

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
    binding.stride_bytes = dev->streams[stream].stride_bytes;
    binding.offset_bytes = dev->streams[stream].offset_bytes;
    binding.reserved0 = 0;

    if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
            dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
      cmd->start_slot = stream;
      cmd->buffer_count = 1;
    }
  }

  if (is_backbuffer(dev->index_buffer)) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
      cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = dev->index_offset_bytes;
      cmd->reserved0 = 0;
    }
  }

  if (!dev->render_targets[0] && !sc->backbuffers.empty()) {
    dev->render_targets[0] = sc->backbuffers[0];
  }
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_reset(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_RESET* pReset) {
  const uint64_t bb_wh =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.backbuffer_width, pReset->present_params.backbuffer_height) : 0;
  const uint64_t fmt_count =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.backbuffer_format, pReset->present_params.backbuffer_count) : 0;
  const uint64_t interval_flags =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.presentation_interval, pReset->present_params.flags) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceReset, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  if (!hDevice.pDrvPrivate || !pReset) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  // Reset implies a new frame queue; drop any in-flight present fences so
  // max-frame-latency throttling doesn't block the first presents after a reset.
  dev->inflight_present_fences.clear();
  SwapChain* sc = dev->current_swapchain;
  if (!sc && !dev->swapchains.empty()) {
    sc = dev->swapchains[0];
  }
  if (!sc) {
    return trace.ret(S_OK);
  }

  return trace.ret(reset_swap_chain_locked(dev, sc, pReset->present_params));
}

HRESULT AEROGPU_D3D9_CALL device_reset_ex(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_RESET* pReset) {
  const uint64_t bb_wh =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.backbuffer_width, pReset->present_params.backbuffer_height) : 0;
  const uint64_t fmt_count =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.backbuffer_format, pReset->present_params.backbuffer_count) : 0;
  const uint64_t interval_flags =
      pReset ? d3d9_trace_pack_u32_u32(pReset->present_params.presentation_interval, pReset->present_params.flags) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceResetEx, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  return trace.ret(device_reset(hDevice, pReset));
}

HRESULT AEROGPU_D3D9_CALL device_check_device_state(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    HWND hWnd) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCheckDeviceState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hWnd),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
#if defined(_WIN32)
  if (hWnd) {
    if (IsIconic(hWnd)) {
      return trace.ret(kSPresentOccluded);
    }
    // IsWindowVisible is cheap; treat hidden windows the same as minimized.
    if (!IsWindowVisible(hWnd)) {
      return trace.ret(kSPresentOccluded);
    }
  }
#endif
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_rotate_resource_identities(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE* pResources,
    uint32_t resource_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceRotateResourceIdentities,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(resource_count),
                      d3d9_trace_arg_ptr(pResources),
                      0);
  if (!hDevice.pDrvPrivate || !pResources || resource_count < 2) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  std::vector<Resource*> resources;
  resources.reserve(resource_count);
  for (uint32_t i = 0; i < resource_count; ++i) {
    auto* res = as_resource(pResources[i]);
    if (!res) {
      return trace.ret(E_INVALIDARG);
    }
    resources.push_back(res);
  }
  const aerogpu_handle_t saved = resources[0]->handle;

  for (uint32_t i = 0; i + 1 < resource_count; ++i) {
    resources[i]->handle = resources[i + 1]->handle;
  }

  resources[resource_count - 1]->handle = saved;

  if (!emit_set_render_targets_locked(dev)) {
    // Undo the rotation (rotate right by one).
    const aerogpu_handle_t undo_saved = resources[resource_count - 1]->handle;
    for (uint32_t i = resource_count - 1; i > 0; --i) {
      resources[i]->handle = resources[i - 1]->handle;
    }
    resources[0]->handle = undo_saved;
    return trace.ret(E_OUTOFMEMORY);
  }

  auto is_rotated = [&resources](const Resource* res) -> bool {
    if (!res) {
      return false;
    }
    return std::find(resources.begin(), resources.end(), res) != resources.end();
  };

  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (!is_rotated(dev->textures[stage])) {
      continue;
    }
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
      cmd->reserved0 = 0;
    }
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (!is_rotated(dev->streams[stream].vb)) {
      continue;
    }

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
    binding.stride_bytes = dev->streams[stream].stride_bytes;
    binding.offset_bytes = dev->streams[stream].offset_bytes;
    binding.reserved0 = 0;

    if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
            dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
      cmd->start_slot = stream;
      cmd->buffer_count = 1;
    }
  }

  if (is_rotated(dev->index_buffer)) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
      cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = dev->index_offset_bytes;
      cmd->reserved0 = 0;
    }
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_lock(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_LOCK* pLock,
    AEROGPU_D3D9DDI_LOCKED_BOX* pLockedBox) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceLock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pLock ? d3d9_trace_arg_ptr(pLock->hResource.pDrvPrivate) : 0,
                      pLock ? d3d9_trace_pack_u32_u32(pLock->offset_bytes, pLock->size_bytes) : 0,
                      pLock ? static_cast<uint64_t>(pLock->flags) : 0);
  if (!pLock || !pLockedBox) {
    return trace.ret(E_INVALIDARG);
  }
  auto* res = as_resource(pLock->hResource);
  if (!res) {
    return trace.ret(E_INVALIDARG);
  }
  if (res->locked) {
    return trace.ret(E_FAIL);
  }

  uint32_t offset = pLock->offset_bytes;
  uint32_t size = pLock->size_bytes ? pLock->size_bytes : (res->size_bytes - offset);
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return trace.ret(E_INVALIDARG);
  }

  res->locked = true;
  res->locked_offset = offset;
  res->locked_size = size;
  res->locked_flags = pLock->flags;

  pLockedBox->pData = res->storage.data() + offset;
  pLockedBox->rowPitch = res->row_pitch;
  pLockedBox->slicePitch = res->slice_pitch;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_unlock(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_UNLOCK* pUnlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUnlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pUnlock ? d3d9_trace_arg_ptr(pUnlock->hResource.pDrvPrivate) : 0,
                      pUnlock ? d3d9_trace_pack_u32_u32(pUnlock->offset_bytes, pUnlock->size_bytes) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pUnlock) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* res = as_resource(pUnlock->hResource);
  if (!dev || !res) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!res->locked) {
    return trace.ret(E_FAIL);
  }

  uint32_t offset = pUnlock->offset_bytes ? pUnlock->offset_bytes : res->locked_offset;
  uint32_t size = pUnlock->size_bytes ? pUnlock->size_bytes : res->locked_size;
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return trace.ret(E_INVALIDARG);
  }

  res->locked = false;

  const uint32_t locked_flags = res->locked_flags;
  res->locked_flags = 0;

  // For bring-up we inline resource updates directly into the command stream so
  // the host/emulator does not need to dereference guest allocations.
  //
  // Note: system-memory pool resources (e.g. CreateOffscreenPlainSurface with
  // D3DPOOL_SYSTEMMEM) are CPU-only and must not be uploaded. Similarly, read-only
  // locks do not imply a content update.
  if (res->handle != 0 && (locked_flags & kD3DLOCK_READONLY) == 0 && size) {
    const uint8_t* src = res->storage.data() + offset;
    uint32_t remaining = size;
    uint32_t cur_offset = offset;

    // Split very large uploads across multiple packets so we can fit within a
    // bounded WDDM DMA buffer when the command stream is span-backed.
    while (remaining) {
      // Ensure we can fit at least a minimal upload packet (header + 1 byte).
      const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + 1, 4);
      if (!ensure_cmd_space(dev, min_needed)) {
        return trace.ret(E_OUTOFMEMORY);
      }

      // Uploads write into the resource. Track its backing allocation so the
      // KMD/emulator can resolve the destination memory via the per-submit alloc
      // table even though we keep the patch-location list empty.
      HRESULT track_hr = track_resource_allocation_locked(dev, res, /*write=*/true);
      if (FAILED(track_hr)) {
        return trace.ret(track_hr);
      }

      // Allocation tracking may have split/flushed the submission; ensure we
      // still have room for at least a minimal upload packet before sizing the
      // next chunk.
      if (!ensure_cmd_space(dev, min_needed)) {
        return trace.ret(E_OUTOFMEMORY);
      }

      const size_t avail = dev->cmd.bytes_remaining();
      size_t chunk = 0;
      if (avail > sizeof(aerogpu_cmd_upload_resource)) {
        chunk = std::min<size_t>(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
      }

      // Account for 4-byte alignment padding at the end of the packet.
      while (chunk && align_up(sizeof(aerogpu_cmd_upload_resource) + chunk, 4) > avail) {
        chunk--;
      }
      if (!chunk) {
        // Should only happen if the command buffer is extremely small; try a
        // forced submit and retry.
        submit(dev);
        continue;
      }

      auto* cmd = append_with_payload_locked<aerogpu_cmd_upload_resource>(
          dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }

      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = cur_offset;
      cmd->size_bytes = chunk;

      src += chunk;
      cur_offset += static_cast<uint32_t>(chunk);
      remaining -= static_cast<uint32_t>(chunk);
    }
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_render_target_data(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_GETRENDERTARGETDATA* pGetRenderTargetData) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetRenderTargetData,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pGetRenderTargetData ? d3d9_trace_arg_ptr(pGetRenderTargetData->hSrcResource.pDrvPrivate) : 0,
                      pGetRenderTargetData ? d3d9_trace_arg_ptr(pGetRenderTargetData->hDstResource.pDrvPrivate) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pGetRenderTargetData) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* src = as_resource(pGetRenderTargetData->hSrcResource);
  auto* dst = as_resource(pGetRenderTargetData->hDstResource);
  if (!dev || !src || !dst) {
    return trace.ret(E_INVALIDARG);
  }

  // GetRenderTargetData copies from a GPU render target/backbuffer into a
  // system-memory surface.
  if (dst->pool != kD3DPOOL_SYSTEMMEM) {
    return trace.ret(E_INVALIDARG);
  }
  if (dst->locked) {
    return trace.ret(E_FAIL);
  }

  // Flush prior GPU work and wait for completion so the CPU sees final pixels.
  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence = submit(dev);
  }
  const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  if (wait_res == FenceWaitResult::NotReady) {
    return trace.ret(kD3dErrWasStillDrawing);
  }

  return trace.ret(copy_surface_bytes(src, dst));
}

HRESULT AEROGPU_D3D9_CALL device_copy_rects(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_COPYRECTS* pCopyRects) {
  const uint64_t src_ptr = pCopyRects ? d3d9_trace_arg_ptr(pCopyRects->hSrcResource.pDrvPrivate) : 0;
  const uint64_t dst_ptr = pCopyRects ? d3d9_trace_arg_ptr(pCopyRects->hDstResource.pDrvPrivate) : 0;
  const uint64_t rects =
      pCopyRects ? d3d9_trace_pack_u32_u32(pCopyRects->rect_count, pCopyRects->pSrcRects != nullptr ? 1u : 0u) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCopyRects, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), src_ptr, dst_ptr, rects);
  if (!hDevice.pDrvPrivate || !pCopyRects) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* src = as_resource(pCopyRects->hSrcResource);
  auto* dst = as_resource(pCopyRects->hDstResource);
  if (!dev || !src || !dst) {
    return trace.ret(E_INVALIDARG);
  }

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence = submit(dev);
  }
  const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  if (wait_res == FenceWaitResult::NotReady) {
    return trace.ret(kD3dErrWasStillDrawing);
  }

  return trace.ret(copy_surface_rects(src, dst, pCopyRects->pSrcRects, pCopyRects->rect_count));
}

HRESULT AEROGPU_D3D9_CALL device_set_render_target(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t slot,
    AEROGPU_D3D9DDI_HRESOURCE hSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetRenderTarget,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(slot),
                      d3d9_trace_arg_ptr(hSurface.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (slot >= 4) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  Resource* saved_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};

  if (surf && slot > 0) {
    for (uint32_t i = 0; i < slot; ++i) {
      if (!dev->render_targets[i]) {
        return trace.ret(kD3DErrInvalidCall);
      }
    }
  }

  dev->render_targets[slot] = surf;
  if (!surf) {
    // Maintain contiguity: clearing an earlier slot implicitly clears any later
    // render targets so the host never sees a gapped binding.
    for (uint32_t i = slot + 1; i < 4; ++i) {
      dev->render_targets[i] = nullptr;
    }
  }

  bool changed = false;
  for (uint32_t i = 0; i < 4; ++i) {
    if (dev->render_targets[i] != saved_rts[i]) {
      changed = true;
      break;
    }
  }
  if (!changed) {
    return trace.ret(S_OK);
  }

  if (!emit_set_render_targets_locked(dev)) {
    for (uint32_t i = 0; i < 4; ++i) {
      dev->render_targets[i] = saved_rts[i];
    }
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_depth_stencil(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetDepthStencil,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSurface.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->depth_stencil == surf) {
    return trace.ret(S_OK);
  }
  dev->depth_stencil = surf;
  if (!emit_set_render_targets_locked(dev)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_viewport(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDI_VIEWPORT* pViewport) {
  const uint64_t xy = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->x), f32_bits(pViewport->y)) : 0;
  const uint64_t wh = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->w), f32_bits(pViewport->h)) : 0;
  const uint64_t zz = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->min_z), f32_bits(pViewport->max_z)) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetViewport, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), xy, wh, zz);
  if (!hDevice.pDrvPrivate || !pViewport) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->viewport = *pViewport;

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_viewport>(dev, AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->x_f32 = f32_bits(pViewport->x);
  cmd->y_f32 = f32_bits(pViewport->y);
  cmd->width_f32 = f32_bits(pViewport->w);
  cmd->height_f32 = f32_bits(pViewport->h);
  cmd->min_depth_f32 = f32_bits(pViewport->min_z);
  cmd->max_depth_f32 = f32_bits(pViewport->max_z);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_scissor(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const RECT* pRect,
    BOOL enabled) {
  const uint64_t lt = pRect ? d3d9_trace_pack_u32_u32(static_cast<uint32_t>(pRect->left), static_cast<uint32_t>(pRect->top)) : 0;
  const uint64_t rb =
      pRect ? d3d9_trace_pack_u32_u32(static_cast<uint32_t>(pRect->right), static_cast<uint32_t>(pRect->bottom)) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceSetScissorRect, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), lt, rb, static_cast<uint64_t>(enabled));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (pRect) {
    dev->scissor_rect = *pRect;
  }
  dev->scissor_enabled = enabled;

  int32_t x = 0;
  int32_t y = 0;
  int32_t w = 0x7FFFFFFF;
  int32_t h = 0x7FFFFFFF;
  if (enabled && pRect) {
    x = static_cast<int32_t>(pRect->left);
    y = static_cast<int32_t>(pRect->top);
    w = static_cast<int32_t>(pRect->right - pRect->left);
    h = static_cast<int32_t>(pRect->bottom - pRect->top);
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_scissor>(dev, AEROGPU_CMD_SET_SCISSOR);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->x = x;
  cmd->y = y;
  cmd->width = w;
  cmd->height = h;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_texture(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stage,
    AEROGPU_D3D9DDI_HRESOURCE hTexture) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetTexture,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_arg_ptr(hTexture.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stage >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* tex = as_resource(hTexture);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->textures[stage] == tex) {
    return trace.ret(S_OK);
  }
  dev->textures[stage] = tex;

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->texture = tex ? tex->handle : 0;
  cmd->reserved0 = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_sampler_state(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetSamplerState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      static_cast<uint64_t>(state),
                      static_cast<uint64_t>(value));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stage >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (stage < 16 && state < 16) {
    dev->sampler_states[stage][state] = value;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_sampler_state>(dev, AEROGPU_CMD_SET_SAMPLER_STATE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->state = state;
  cmd->value = value;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_render_state(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t state,
    uint32_t value) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetRenderState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(state),
                      static_cast<uint64_t>(value),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (state < 256) {
    dev->render_states[state] = value;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_state>(dev, AEROGPU_CMD_SET_RENDER_STATE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->state = state;
  cmd->value = value;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const void* pDecl,
    uint32_t decl_size,
    AEROGPU_D3D9DDI_HVERTEXDECL* phDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(decl_size),
                      d3d9_trace_arg_ptr(pDecl),
                      d3d9_trace_arg_ptr(phDecl));
  if (!hDevice.pDrvPrivate || !pDecl || !phDecl || decl_size == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto decl = std::make_unique<VertexDecl>();
  decl->handle = allocate_global_handle(dev->adapter);
  decl->blob.resize(decl_size);
  std::memcpy(decl->blob.data(), pDecl, decl_size);

  if (!emit_create_input_layout_locked(dev, decl.get())) {
    return trace.ret(E_OUTOFMEMORY);
  }

  phDecl->pDrvPrivate = decl.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hDecl.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!emit_set_input_layout_locked(dev, decl)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Some runtimes implement SetFVF by synthesizing a declaration and calling
  // SetVertexDecl. Detect the specific `XYZRHW | DIFFUSE` layout used by the
  // Win7 bring-up test so we can enable the fixed-function fallback path even
  // if `pfnSetFVF` is not invoked.
  bool matches_fvf_xyzrhw_diffuse = false;
  if (decl && decl->blob.size() >= sizeof(D3DVERTEXELEMENT9_COMPAT) * 3) {
    const auto* elems = reinterpret_cast<const D3DVERTEXELEMENT9_COMPAT*>(decl->blob.data());
    const auto& e0 = elems[0];
    const auto& e1 = elems[1];
    const auto& e2 = elems[2];

    const bool e0_ok = (e0.Stream == 0) && (e0.Offset == 0) && (e0.Type == kD3dDeclTypeFloat4) &&
                       (e0.Method == kD3dDeclMethodDefault) &&
                       (e0.Usage == kD3dDeclUsagePositionT || e0.Usage == 0) && (e0.UsageIndex == 0);
    const bool e1_ok = (e1.Stream == 0) && (e1.Offset == 16) && (e1.Type == kD3dDeclTypeD3dColor) &&
                       (e1.Method == kD3dDeclMethodDefault) && (e1.Usage == kD3dDeclUsageColor) && (e1.UsageIndex == 0);
    const bool e2_ok = (e2.Stream == 0xFF) && (e2.Type == kD3dDeclTypeUnused);
    matches_fvf_xyzrhw_diffuse = e0_ok && e1_ok && e2_ok;
  }
  dev->fvf = matches_fvf_xyzrhw_diffuse ? kSupportedFvfXyzrhwDiffuse : 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hDecl.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);
  if (!dev || !decl) {
    delete decl;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->vertex_decl == decl) {
    dev->vertex_decl = nullptr;
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_input_layout>(dev, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
      cmd->input_layout_handle = 0;
      cmd->reserved0 = 0;
    }
  }
  (void)emit_destroy_input_layout_locked(dev, decl->handle);
  delete decl;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_fvf(AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t fvf) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetFVF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(fvf),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (fvf == dev->fvf) {
    return trace.ret(S_OK);
  }

  if (fvf != 0 && fvf != kSupportedFvfXyzrhwDiffuse) {
    return trace.ret(E_NOTIMPL);
  }

  if (fvf == 0) {
    dev->fvf = 0;
    return trace.ret(S_OK);
  }

  if (!dev->fvf_vertex_decl) {
    // Build the declaration for this FVF. For bring-up we only support the
    // `XYZRHW | DIFFUSE` path used by the Win7 d3d9ex_triangle test.
    const D3DVERTEXELEMENT9_COMPAT elems[] = {
        // stream, offset, type, method, usage, usage_index
        {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
        {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
    };

    dev->fvf_vertex_decl = create_internal_vertex_decl_locked(dev, elems, sizeof(elems));
    if (!dev->fvf_vertex_decl) {
      return trace.ret(E_OUTOFMEMORY);
    }
  }

  if (!emit_set_input_layout_locked(dev, dev->fvf_vertex_decl)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  dev->fvf = fvf;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    const void* pBytecode,
    uint32_t bytecode_size,
    AEROGPU_D3D9DDI_HSHADER* phShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      static_cast<uint64_t>(bytecode_size),
                      d3d9_trace_arg_ptr(pBytecode));
  if (!hDevice.pDrvPrivate || !pBytecode || !phShader || bytecode_size == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto sh = std::make_unique<Shader>();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = stage;
  sh->bytecode.resize(bytecode_size);
  std::memcpy(sh->bytecode.data(), pBytecode, bytecode_size);

  if (!emit_create_shader_locked(dev, sh.get())) {
    return trace.ret(E_OUTOFMEMORY);
  }

  phShader->pDrvPrivate = sh.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    AEROGPU_D3D9DDI_HSHADER hShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_arg_ptr(hShader.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);

  std::lock_guard<std::mutex> lock(dev->mutex);

  Shader** user_slot = (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? &dev->user_vs : &dev->user_ps;
  if (*user_slot == sh) {
    return trace.ret(S_OK);
  }

  *user_slot = sh;

  // Bind exactly what the runtime requested. Fixed-function fallbacks are
  // re-bound lazily at draw time when `user_vs/user_ps` are both null.
  dev->vs = dev->user_vs;
  dev->ps = dev->user_ps;

  if (!emit_bind_shaders_locked(dev)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HSHADER hShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hShader.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);
  if (!dev || !sh) {
    delete sh;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  bool bindings_changed = false;

  // The runtime may destroy a shader while it is still bound. Clear both the
  // public "user" bindings and the currently-bound shader slots so subsequent
  // draws can re-bind the fixed-function fallback if needed.
  if (dev->user_vs == sh) {
    dev->user_vs = nullptr;
    bindings_changed = true;
  }
  if (dev->user_ps == sh) {
    dev->user_ps = nullptr;
    bindings_changed = true;
  }
  if (dev->vs == sh) {
    dev->vs = nullptr;
    bindings_changed = true;
  }
  if (dev->ps == sh) {
    dev->ps = nullptr;
    bindings_changed = true;
  }

  if (bindings_changed) {
    (void)emit_bind_shaders_locked(dev);
  }
  (void)emit_destroy_shader_locked(dev, sh->handle);
  delete sh;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_shader_const_f(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    uint32_t start_reg,
    const float* pData,
    uint32_t vec4_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetShaderConstF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_pack_u32_u32(start_reg, vec4_count),
                      d3d9_trace_arg_ptr(pData));
  if (!hDevice.pDrvPrivate || !pData || vec4_count == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  float* dst = (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? dev->vs_consts_f : dev->ps_consts_f;
  if (start_reg < 256) {
    const uint32_t write_regs = std::min(vec4_count, 256u - start_reg);
    std::memcpy(dst + start_reg * 4, pData, static_cast<size_t>(write_regs) * 4 * sizeof(float));
  }

  const size_t payload_size = static_cast<size_t>(vec4_count) * 4 * sizeof(float);
  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_shader_constants_f>(
      dev, AEROGPU_CMD_SET_SHADER_CONSTANTS_F, pData, payload_size);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->stage = d3d9_stage_to_aerogpu_stage(stage);
  cmd->start_register = start_reg;
  cmd->vec4_count = vec4_count;
  cmd->reserved0 = 0;

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_blt(AEROGPU_D3D9DDI_HDEVICE hDevice, const AEROGPU_D3D9DDIARG_BLT* pBlt) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceBlt,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pBlt ? d3d9_trace_arg_ptr(pBlt->hSrc.pDrvPrivate) : 0,
                      pBlt ? d3d9_trace_arg_ptr(pBlt->hDst.pDrvPrivate) : 0,
                      pBlt ? d3d9_trace_pack_u32_u32(pBlt->filter, pBlt->flags) : 0);
  if (!hDevice.pDrvPrivate || !pBlt) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(pBlt->hSrc);
  auto* dst = as_resource(pBlt->hDst);

  std::lock_guard<std::mutex> lock(dev->mutex);
  logf("aerogpu-d3d9: Blt src=%p dst=%p filter=%u\n", src, dst, pBlt->filter);

  return trace.ret(blit_locked(dev, dst, pBlt->pDstRect, src, pBlt->pSrcRect, pBlt->filter));
}

HRESULT AEROGPU_D3D9_CALL device_color_fill(AEROGPU_D3D9DDI_HDEVICE hDevice,
                                             const AEROGPU_D3D9DDIARG_COLORFILL* pColorFill) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceColorFill,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pColorFill ? d3d9_trace_arg_ptr(pColorFill->hDst.pDrvPrivate) : 0,
                      pColorFill ? static_cast<uint64_t>(pColorFill->color_argb) : 0,
                      pColorFill ? static_cast<uint64_t>(pColorFill->pRect != nullptr ? 1u : 0u) : 0);
  if (!hDevice.pDrvPrivate || !pColorFill) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dst = as_resource(pColorFill->hDst);
  std::lock_guard<std::mutex> lock(dev->mutex);
  logf("aerogpu-d3d9: ColorFill dst=%p color=0x%08x\n", dst, pColorFill->color_argb);
  return trace.ret(color_fill_locked(dev, dst, pColorFill->pRect, pColorFill->color_argb));
}

HRESULT AEROGPU_D3D9_CALL device_update_surface(AEROGPU_D3D9DDI_HDEVICE hDevice,
                                                  const AEROGPU_D3D9DDIARG_UPDATESURFACE* pUpdateSurface) {
  const uint64_t rect_flags = pUpdateSurface ? d3d9_trace_pack_u32_u32(pUpdateSurface->pSrcRect != nullptr ? 1u : 0u,
                                                                       pUpdateSurface->pDstRect != nullptr ? 1u : 0u)
                                             : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUpdateSurface,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pUpdateSurface ? d3d9_trace_arg_ptr(pUpdateSurface->hSrc.pDrvPrivate) : 0,
                      pUpdateSurface ? d3d9_trace_arg_ptr(pUpdateSurface->hDst.pDrvPrivate) : 0,
                      rect_flags);
  if (!hDevice.pDrvPrivate || !pUpdateSurface) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(pUpdateSurface->hSrc);
  auto* dst = as_resource(pUpdateSurface->hDst);

  std::lock_guard<std::mutex> lock(dev->mutex);
  logf("aerogpu-d3d9: UpdateSurface src=%p dst=%p\n", src, dst);
  return trace.ret(update_surface_locked(dev, src, pUpdateSurface->pSrcRect, dst, pUpdateSurface->pDstPoint));
}

HRESULT AEROGPU_D3D9_CALL device_update_texture(AEROGPU_D3D9DDI_HDEVICE hDevice,
                                                  const AEROGPU_D3D9DDIARG_UPDATETEXTURE* pUpdateTexture) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUpdateTexture,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pUpdateTexture ? d3d9_trace_arg_ptr(pUpdateTexture->hSrc.pDrvPrivate) : 0,
                      pUpdateTexture ? d3d9_trace_arg_ptr(pUpdateTexture->hDst.pDrvPrivate) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pUpdateTexture) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(pUpdateTexture->hSrc);
  auto* dst = as_resource(pUpdateTexture->hDst);

  std::lock_guard<std::mutex> lock(dev->mutex);
  logf("aerogpu-d3d9: UpdateTexture src=%p dst=%p\n", src, dst);
  return trace.ret(update_texture_locked(dev, src, dst));
}

HRESULT AEROGPU_D3D9_CALL device_set_stream_source(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stream,
    AEROGPU_D3D9DDI_HRESOURCE hVb,
    uint32_t offset_bytes,
    uint32_t stride_bytes) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetStreamSource,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stream),
                      d3d9_trace_arg_ptr(hVb.pDrvPrivate),
                      d3d9_trace_pack_u32_u32(offset_bytes, stride_bytes));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stream >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* vb = as_resource(hVb);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!emit_set_stream_source_locked(dev, stream, vb, offset_bytes, stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_indices(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hIb,
    AEROGPU_D3D9DDI_INDEX_FORMAT fmt,
    uint32_t offset_bytes) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetIndices,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hIb.pDrvPrivate),
                      d3d9_trace_pack_u32_u32(static_cast<uint32_t>(fmt), offset_bytes),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* ib = as_resource(hIb);

  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->index_buffer = ib;
  dev->index_format = fmt;
  dev->index_offset_bytes = offset_bytes;

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->buffer = ib ? ib->handle : 0;
  cmd->format = d3d9_index_format_to_aerogpu(fmt);
  cmd->offset_bytes = offset_bytes;
  cmd->reserved0 = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_begin_scene(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->scene_depth++;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_end_scene(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->scene_depth > 0) {
    dev->scene_depth--;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_clear(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t flags,
    uint32_t color_rgba8,
    float depth,
    uint32_t stencil) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceClear,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(flags),
                      static_cast<uint64_t>(color_rgba8),
                      d3d9_trace_pack_u32_u32(f32_bits(depth), stencil));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_clear), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_render_targets_locked(dev);
  if (hr < 0) {
    return hr;
  }

  const float a = static_cast<float>((color_rgba8 >> 24) & 0xFF) / 255.0f;
  const float r = static_cast<float>((color_rgba8 >> 16) & 0xFF) / 255.0f;
  const float g = static_cast<float>((color_rgba8 >> 8) & 0xFF) / 255.0f;
  const float b = static_cast<float>((color_rgba8 >> 0) & 0xFF) / 255.0f;

  auto* cmd = append_fixed_locked<aerogpu_cmd_clear>(dev, AEROGPU_CMD_CLEAR);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->flags = flags;
  cmd->color_rgba_f32[0] = f32_bits(r);
  cmd->color_rgba_f32[1] = f32_bits(g);
  cmd->color_rgba_f32[2] = f32_bits(b);
  cmd->color_rgba_f32[3] = f32_bits(a);
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRIMITIVE_TYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawPrimitive,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      d3d9_trace_pack_u32_u32(start_vertex, primitive_count),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  // Fixed-function emulation path: for XYZRHW vertices we upload a transformed
  // (clip-space) copy of the referenced vertices into a scratch VB and draw
  // using a built-in shader pair.
  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    DeviceStateStream saved = dev->streams[0];
    DeviceStateStream& ss = dev->streams[0];
    if (!ss.vb || ss.stride_bytes < 20) {
      return E_FAIL;
    }

    const uint32_t vertex_count = vertex_count_from_primitive(type, primitive_count);
    const uint64_t src_offset_u64 =
        static_cast<uint64_t>(ss.offset_bytes) + static_cast<uint64_t>(start_vertex) * ss.stride_bytes;
    const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * ss.stride_bytes;
    if (src_offset_u64 > ss.vb->storage.size() || size_u64 > ss.vb->storage.size() - src_offset_u64) {
      return E_INVALIDARG;
    }

    std::vector<uint8_t> converted;
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev,
        ss.vb->storage.data() + static_cast<size_t>(src_offset_u64),
        ss.stride_bytes,
        vertex_count,
        &converted);
    if (FAILED(hr)) {
      return hr;
    }

    hr = ensure_up_vertex_buffer_locked(dev, static_cast<uint32_t>(converted.size()));
    if (FAILED(hr)) {
      return hr;
    }
    hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, converted.data(), static_cast<uint32_t>(converted.size()));
    if (FAILED(hr)) {
      return hr;
    }

    if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, ss.stride_bytes)) {
      return E_OUTOFMEMORY;
    }

    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }

    const uint32_t topology = d3d9_prim_to_topology(type);
    if (!emit_set_topology_locked(dev, topology)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }

    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }
    hr = track_draw_state_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
    if (!cmd) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }
    cmd->vertex_count = vertex_count;
    cmd->instance_count = 1;
    cmd->first_vertex = 0;
    cmd->first_instance = 0;

    if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
      return E_OUTOFMEMORY;
    }
    return S_OK;
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_draw_state_locked(dev);
  if (hr < 0) {
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->vertex_count = vertex_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive_up(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRIMITIVE_TYPE type,
    uint32_t primitive_count,
    const void* pVertexData,
    uint32_t stride_bytes) {
  const uint64_t packed = d3d9_trace_pack_u32_u32(primitive_count, stride_bytes);
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawPrimitiveUP,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      packed,
                      d3d9_trace_arg_ptr(pVertexData));
  if (!hDevice.pDrvPrivate || !pVertexData || stride_bytes == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t vertex_count = vertex_count_from_primitive(type, primitive_count);
  const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * stride_bytes;
  if (size_u64 == 0 || size_u64 > 0x7FFFFFFFu) {
    return trace.ret(E_INVALIDARG);
  }

  DeviceStateStream saved = dev->streams[0];

  std::vector<uint8_t> converted;
  const void* upload_data = pVertexData;
  uint32_t upload_size = static_cast<uint32_t>(size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(dev, pVertexData, stride_bytes, vertex_count, &converted);
    if (FAILED(hr)) {
      return trace.ret(hr);
    }
    upload_data = converted.data();
    upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, upload_size);
  if (FAILED(hr)) {
    return trace.ret(hr);
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, upload_data, upload_size);
  if (FAILED(hr)) {
    return trace.ret(hr);
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return trace.ret(hr);
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }
  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(hr);
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
  cmd->first_instance = 0;

  if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive2(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_DRAWPRIMITIVE2* pDraw) {
  if (!hDevice.pDrvPrivate || !pDraw) {
    return E_INVALIDARG;
  }
  if (!pDraw->pVertexStreamZeroData || pDraw->vertex_stream_zero_stride == 0) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t vertex_count = vertex_count_from_primitive(pDraw->type, pDraw->primitive_count);
  const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * pDraw->vertex_stream_zero_stride;
  if (size_u64 == 0 || size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }

  DeviceStateStream saved = dev->streams[0];

  std::vector<uint8_t> converted;
  const void* upload_data = pDraw->pVertexStreamZeroData;
  uint32_t upload_size = static_cast<uint32_t>(size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev, pDraw->pVertexStreamZeroData, pDraw->vertex_stream_zero_stride, vertex_count, &converted);
    if (FAILED(hr)) {
      return hr;
    }
    upload_data = converted.data();
    upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, upload_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, upload_data, upload_size);
  if (FAILED(hr)) {
    return hr;
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, pDraw->vertex_stream_zero_stride)) {
    return E_OUTOFMEMORY;
  }

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(pDraw->type);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }

  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
  cmd->first_instance = 0;

  if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive2(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_DRAWINDEXEDPRIMITIVE2* pDraw) {
  if (!hDevice.pDrvPrivate || !pDraw) {
    return E_INVALIDARG;
  }
  if (!pDraw->pVertexStreamZeroData || pDraw->vertex_stream_zero_stride == 0 || !pDraw->pIndexData) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t index_count = index_count_from_primitive(pDraw->type, pDraw->primitive_count);
  const uint32_t index_size = (pDraw->index_format == AEROGPU_D3D9DDI_INDEX_FORMAT_U32) ? 4u : 2u;
  const uint64_t ib_size_u64 = static_cast<uint64_t>(index_count) * index_size;
  if (ib_size_u64 == 0 || ib_size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }
  const uint32_t ib_size = static_cast<uint32_t>(ib_size_u64);

  const uint64_t vertex_count_u64 = static_cast<uint64_t>(pDraw->min_index) + static_cast<uint64_t>(pDraw->num_vertices);
  const uint64_t vb_size_u64 = vertex_count_u64 * static_cast<uint64_t>(pDraw->vertex_stream_zero_stride);
  if (vertex_count_u64 == 0 || vb_size_u64 == 0 || vb_size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }

  DeviceStateStream saved_stream = dev->streams[0];
  Resource* saved_ib = dev->index_buffer;
  const AEROGPU_D3D9DDI_INDEX_FORMAT saved_fmt = dev->index_format;
  const uint32_t saved_offset = dev->index_offset_bytes;

  std::vector<uint8_t> converted;
  const void* vb_upload_data = pDraw->pVertexStreamZeroData;
  uint32_t vb_upload_size = static_cast<uint32_t>(vb_size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev, pDraw->pVertexStreamZeroData, pDraw->vertex_stream_zero_stride, static_cast<uint32_t>(vertex_count_u64), &converted);
    if (FAILED(hr)) {
      return hr;
    }
    vb_upload_data = converted.data();
    vb_upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, vb_upload_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, vb_upload_data, vb_upload_size);
  if (FAILED(hr)) {
    return hr;
  }

  hr = ensure_up_index_buffer_locked(dev, ib_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_index_buffer, pDraw->pIndexData, ib_size);
  if (FAILED(hr)) {
    return hr;
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, pDraw->vertex_stream_zero_stride)) {
    return E_OUTOFMEMORY;
  }

  dev->index_buffer = dev->up_index_buffer;
  dev->index_format = pDraw->index_format;
  dev->index_offset_bytes = 0;

  auto* ib_cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!ib_cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    return E_OUTOFMEMORY;
  }
  ib_cmd->buffer = dev->up_index_buffer ? dev->up_index_buffer->handle : 0;
  ib_cmd->format = d3d9_index_format_to_aerogpu(pDraw->index_format);
  ib_cmd->offset_bytes = 0;
  ib_cmd->reserved0 = 0;

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      // Restore IB state.
      dev->index_buffer = saved_ib;
      dev->index_format = saved_fmt;
      dev->index_offset_bytes = saved_offset;
      auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
      if (restore) {
        restore->buffer = saved_ib ? saved_ib->handle : 0;
        restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
        restore->offset_bytes = saved_offset;
        restore->reserved0 = 0;
      }
      return hr;
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(pDraw->type);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw_indexed), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }

  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw_indexed>(dev, AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = 0;
  cmd->base_vertex = 0;
  cmd->first_instance = 0;

  // Restore stream source 0.
  if (!emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes)) {
    return E_OUTOFMEMORY;
  }

  // Restore index buffer binding.
  dev->index_buffer = saved_ib;
  dev->index_format = saved_fmt;
  dev->index_offset_bytes = saved_offset;
  auto* restore_cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!restore_cmd) {
    return E_OUTOFMEMORY;
  }
  restore_cmd->buffer = saved_ib ? saved_ib->handle : 0;
  restore_cmd->format = d3d9_index_format_to_aerogpu(saved_fmt);
  restore_cmd->offset_bytes = saved_offset;
  restore_cmd->reserved0 = 0;

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRIMITIVE_TYPE type,
    int32_t base_vertex,
    uint32_t /*min_index*/,
    uint32_t /*num_vertices*/,
    uint32_t start_index,
    uint32_t primitive_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawIndexedPrimitive,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      d3d9_trace_pack_u32_u32(static_cast<uint32_t>(base_vertex), start_index),
                      static_cast<uint64_t>(primitive_count));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  // Fixed-function emulation for indexed draws: expand indices into a temporary
  // vertex stream and issue a non-indexed draw. This is intentionally
  // conservative but is sufficient for bring-up.
  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    DeviceStateStream saved_stream = dev->streams[0];
    DeviceStateStream& ss = dev->streams[0];

    if (!ss.vb || ss.stride_bytes < 20) {
      return E_FAIL;
    }
    if (!dev->index_buffer) {
      return E_FAIL;
    }

    const uint32_t index_count = index_count_from_primitive(type, primitive_count);
    const uint32_t index_size = (dev->index_format == AEROGPU_D3D9DDI_INDEX_FORMAT_U32) ? 4u : 2u;
    const uint64_t index_bytes_u64 = static_cast<uint64_t>(index_count) * index_size;
    const uint64_t index_offset_u64 =
        static_cast<uint64_t>(dev->index_offset_bytes) + static_cast<uint64_t>(start_index) * index_size;

    if (index_offset_u64 > dev->index_buffer->storage.size() ||
        index_bytes_u64 > dev->index_buffer->storage.size() - index_offset_u64) {
      return E_INVALIDARG;
    }

    const uint8_t* index_data = dev->index_buffer->storage.data() + static_cast<size_t>(index_offset_u64);

    std::vector<uint8_t> expanded;
    try {
      expanded.resize(static_cast<size_t>(static_cast<uint64_t>(index_count) * ss.stride_bytes));
    } catch (...) {
      return E_OUTOFMEMORY;
    }

    float vp_x = 0.0f;
    float vp_y = 0.0f;
    float vp_w = 1.0f;
    float vp_h = 1.0f;
    get_viewport_dims_locked(dev, &vp_x, &vp_y, &vp_w, &vp_h);

    for (uint32_t i = 0; i < index_count; i++) {
      uint32_t idx = 0;
      if (index_size == 4) {
        std::memcpy(&idx, index_data + i * 4, sizeof(idx));
      } else {
        uint16_t idx16 = 0;
        std::memcpy(&idx16, index_data + i * 2, sizeof(idx16));
        idx = idx16;
      }

      const int64_t vtx = static_cast<int64_t>(base_vertex) + static_cast<int64_t>(idx);
      if (vtx < 0) {
        return E_INVALIDARG;
      }

      const uint64_t src_off_u64 =
          static_cast<uint64_t>(ss.offset_bytes) + static_cast<uint64_t>(vtx) * ss.stride_bytes;
      if (src_off_u64 > ss.vb->storage.size() || ss.stride_bytes > ss.vb->storage.size() - src_off_u64) {
        return E_INVALIDARG;
      }

      const uint8_t* src = ss.vb->storage.data() + static_cast<size_t>(src_off_u64);
      uint8_t* dst = expanded.data() + static_cast<size_t>(i) * ss.stride_bytes;
      std::memcpy(dst, src, ss.stride_bytes);

      const float x = read_f32_unaligned(src + 0);
      const float y = read_f32_unaligned(src + 4);
      const float z = read_f32_unaligned(src + 8);
      const float rhw = read_f32_unaligned(src + 12);

      const float w = (rhw != 0.0f) ? (1.0f / rhw) : 1.0f;
      const float ndc_x = ((x - vp_x) / vp_w) * 2.0f - 1.0f;
      const float ndc_y = 1.0f - ((y - vp_y) / vp_h) * 2.0f;
      const float ndc_z = z;

      write_f32_unaligned(dst + 0, ndc_x * w);
      write_f32_unaligned(dst + 4, ndc_y * w);
      write_f32_unaligned(dst + 8, ndc_z * w);
      write_f32_unaligned(dst + 12, w);
    }

    HRESULT hr = ensure_up_vertex_buffer_locked(dev, static_cast<uint32_t>(expanded.size()));
    if (FAILED(hr)) {
      return hr;
    }
    hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, expanded.data(), static_cast<uint32_t>(expanded.size()));
    if (FAILED(hr)) {
      return hr;
    }

    if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, ss.stride_bytes)) {
      return E_OUTOFMEMORY;
    }

    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return hr;
    }

    const uint32_t topology = d3d9_prim_to_topology(type);
    if (!emit_set_topology_locked(dev, topology)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }

    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }
    hr = track_draw_state_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return hr;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
    if (!cmd) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }
    cmd->vertex_count = index_count;
    cmd->instance_count = 1;
    cmd->first_vertex = 0;
    cmd->first_instance = 0;

    if (!emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes)) {
      return E_OUTOFMEMORY;
    }
    return S_OK;
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw_indexed), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_draw_state_locked(dev);
  if (hr < 0) {
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw_indexed>(dev, AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->index_count = index_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_present_ex(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_PRESENTEX* pPresentEx) {
  const uint64_t wnd = pPresentEx ? d3d9_trace_arg_ptr(pPresentEx->hWnd) : 0;
  const uint64_t sync_flags =
      pPresentEx ? d3d9_trace_pack_u32_u32(pPresentEx->sync_interval, pPresentEx->d3d9_present_flags) : 0;
  const uint64_t src = pPresentEx ? d3d9_trace_arg_ptr(pPresentEx->hSrc.pDrvPrivate) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DevicePresentEx, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), wnd, sync_flags, src);
  if (!hDevice.pDrvPrivate || !pPresentEx) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  uint32_t present_count = 0;
  HRESULT present_hr = S_OK;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);

    bool occluded = false;
#if defined(_WIN32)
    // Returning S_PRESENT_OCCLUDED from PresentEx helps some D3D9Ex clients avoid
    // pathological present loops when their target window is minimized/hidden.
    // Keep the check cheap and never block on it.
    HWND hwnd = pPresentEx->hWnd;
    if (!hwnd) {
      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      hwnd = sc ? sc->hwnd : nullptr;
    }
    if (hwnd) {
      if (IsIconic(hwnd) || !IsWindowVisible(hwnd)) {
        occluded = true;
      }
    }
#endif

    if (occluded) {
      // Even when occluded, Present/PresentEx act as a flush point and must
      // advance D3D9Ex present statistics (GetPresentStats/GetLastPresentCount).
      retire_completed_presents_locked(dev);
      (void)submit(dev, /*is_present=*/false);

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();

      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
      }

      present_hr = kSPresentOccluded;
    } else {
      HRESULT hr = throttle_presents_locked(dev, pPresentEx->d3d9_present_flags);
      if (hr != S_OK) {
        return trace.ret(hr);
      }

      // Submit any pending render work via the Render callback before issuing a
      // Present submission. This ensures the KMD/emulator observes distinct
      // render vs present submissions (DxgkDdiRender vs DxgkDdiPresent).
      (void)submit(dev, /*is_present=*/false);

      auto* cmd = append_fixed_locked<aerogpu_cmd_present_ex>(dev, AEROGPU_CMD_PRESENT_EX);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }
      cmd->scanout_id = 0;
      bool vsync = (pPresentEx->sync_interval != 0) && (pPresentEx->sync_interval != kD3dPresentIntervalImmediate);
      if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
        // Only request vblank-paced presents when the active device reports vblank support.
        vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
      }
      cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
      cmd->d3d9_present_flags = pPresentEx->d3d9_present_flags;
      cmd->reserved0 = 0;

      const uint64_t submit_fence = submit(dev, /*is_present=*/true);
      const uint64_t present_fence = submit_fence;
      if (present_fence) {
        dev->inflight_present_fences.push_back(present_fence);
      }

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();
      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
        sc->last_present_fence = present_fence;
        if (sc->backbuffers.size() > 1 && sc->swap_effect != 0u) {
          auto is_backbuffer = [sc](const Resource* res) -> bool {
            if (!sc || !res) {
              return false;
            }
            return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
          };

          // Present-style backbuffer rotation swaps the host handles attached to the
          // backbuffer Resource objects. If any backbuffers are currently bound via
          // device state (RTs, textures, IA buffers), we must re-emit those binds so
          // the host stops referencing the old handles.
          size_t needed_bytes = align_up(sizeof(aerogpu_cmd_set_render_targets), 4);
          for (uint32_t stage = 0; stage < 16; ++stage) {
            if (is_backbuffer(dev->textures[stage])) {
              needed_bytes += align_up(sizeof(aerogpu_cmd_set_texture), 4);
            }
          }
          for (uint32_t stream = 0; stream < 16; ++stream) {
            if (is_backbuffer(dev->streams[stream].vb)) {
              needed_bytes += align_up(sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding), 4);
            }
          }
          if (is_backbuffer(dev->index_buffer)) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_index_buffer), 4);
          }

          if (ensure_cmd_space(dev, needed_bytes)) {
            const aerogpu_handle_t saved = sc->backbuffers[0]->handle;
            for (size_t i = 0; i + 1 < sc->backbuffers.size(); ++i) {
              sc->backbuffers[i]->handle = sc->backbuffers[i + 1]->handle;
            }
            sc->backbuffers.back()->handle = saved;

            bool ok = emit_set_render_targets_locked(dev);
            for (uint32_t stage = 0; ok && stage < 16; ++stage) {
              if (!is_backbuffer(dev->textures[stage])) {
                continue;
              }
              auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
              if (!cmd) {
                ok = false;
                break;
              }
              cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
              cmd->slot = stage;
              cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
              cmd->reserved0 = 0;
            }

            for (uint32_t stream = 0; ok && stream < 16; ++stream) {
              if (!is_backbuffer(dev->streams[stream].vb)) {
                continue;
              }

              aerogpu_vertex_buffer_binding binding{};
              binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
              binding.stride_bytes = dev->streams[stream].stride_bytes;
              binding.offset_bytes = dev->streams[stream].offset_bytes;
              binding.reserved0 = 0;

              auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
                  dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
              if (!cmd) {
                ok = false;
                break;
              }
              cmd->start_slot = stream;
              cmd->buffer_count = 1;
            }

            if (ok && is_backbuffer(dev->index_buffer)) {
              auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
              if (!cmd) {
                ok = false;
              } else {
                cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
                cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
                cmd->offset_bytes = dev->index_offset_bytes;
                cmd->reserved0 = 0;
              }
            }

            if (!ok) {
              // Preserve device/host state consistency: if we cannot emit the
              // rebinding commands (command buffer too small), undo the rotation so
              // future draws still target the host's current bindings.
              const aerogpu_handle_t undo_saved = sc->backbuffers.back()->handle;
              for (size_t i = sc->backbuffers.size() - 1; i > 0; --i) {
                sc->backbuffers[i]->handle = sc->backbuffers[i - 1]->handle;
              }
              sc->backbuffers[0]->handle = undo_saved;
              dev->cmd.reset();
            }
          }
        }
      }
    }
  }

  d3d9_trace_maybe_dump_on_present(present_count);
  return trace.ret(present_hr);
}

HRESULT AEROGPU_D3D9_CALL device_present(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_PRESENT* pPresent) {
  const uint64_t sc_ptr = pPresent ? d3d9_trace_arg_ptr(pPresent->hSwapChain.pDrvPrivate) : 0;
  const uint64_t src_ptr = pPresent ? d3d9_trace_arg_ptr(pPresent->hSrc.pDrvPrivate) : 0;
  const uint64_t sync_flags = pPresent ? d3d9_trace_pack_u32_u32(pPresent->sync_interval, pPresent->flags) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DevicePresent, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), sc_ptr, src_ptr, sync_flags);
  if (!hDevice.pDrvPrivate || !pPresent) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  uint32_t present_count = 0;
  HRESULT present_hr = S_OK;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);

    bool occluded = false;
#if defined(_WIN32)
    HWND hwnd = pPresent->hWnd;
    if (!hwnd) {
      SwapChain* sc = as_swapchain(pPresent->hSwapChain);
      if (sc) {
        auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
        if (it == dev->swapchains.end()) {
          sc = nullptr;
        }
      }
      if (!sc) {
        sc = dev->current_swapchain;
      }
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      hwnd = sc ? sc->hwnd : nullptr;
    }
    if (hwnd) {
      if (IsIconic(hwnd) || !IsWindowVisible(hwnd)) {
        occluded = true;
      }
    }
#endif

    if (occluded) {
      retire_completed_presents_locked(dev);
      (void)submit(dev, /*is_present=*/false);

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();

      SwapChain* sc = as_swapchain(pPresent->hSwapChain);
      if (sc) {
        auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
        if (it == dev->swapchains.end()) {
          sc = nullptr;
        }
      }
      if (!sc) {
        sc = dev->current_swapchain;
      }
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
      }

      present_hr = kSPresentOccluded;
    } else {
    HRESULT hr = throttle_presents_locked(dev, pPresent->flags);
    if (hr != S_OK) {
      return trace.ret(hr);
    }

    // Submit any pending render work via the Render callback before issuing a
    // Present submission. This ensures the KMD/emulator observes distinct
    // render vs present submissions (DxgkDdiRender vs DxgkDdiPresent).
    (void)submit(dev, /*is_present=*/false);

    auto* cmd = append_fixed_locked<aerogpu_cmd_present_ex>(dev, AEROGPU_CMD_PRESENT_EX);
    if (!cmd) {
      return trace.ret(E_OUTOFMEMORY);
    }
    cmd->scanout_id = 0;
    bool vsync = (pPresent->sync_interval != 0) && (pPresent->sync_interval != kD3dPresentIntervalImmediate);
    if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
      vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
    }
    cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
    cmd->d3d9_present_flags = pPresent->flags;
    cmd->reserved0 = 0;

    const uint64_t submit_fence = submit(dev, /*is_present=*/true);
    const uint64_t present_fence = submit_fence;
    if (present_fence) {
      dev->inflight_present_fences.push_back(present_fence);
    }

    dev->present_count++;
    present_count = dev->present_count;
    dev->present_refresh_count = dev->present_count;
    dev->sync_refresh_count = dev->present_count;
    dev->last_present_qpc = qpc_now();
    SwapChain* sc = as_swapchain(pPresent->hSwapChain);
    if (sc) {
      auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
      if (it == dev->swapchains.end()) {
        sc = nullptr;
      }
    }
    if (!sc) {
      sc = dev->current_swapchain;
    }
    if (!sc && (pPresent->hWnd || pPresent->hSrc.pDrvPrivate)) {
      for (SwapChain* candidate : dev->swapchains) {
        if (!candidate) {
          continue;
        }
        if (pPresent->hWnd && candidate->hwnd == pPresent->hWnd) {
          sc = candidate;
          break;
        }
        if (pPresent->hSrc.pDrvPrivate) {
          auto* src = as_resource(pPresent->hSrc);
          if (src && std::find(candidate->backbuffers.begin(), candidate->backbuffers.end(), src) != candidate->backbuffers.end()) {
            sc = candidate;
            break;
          }
        }
      }
    }
    if (!sc && !dev->swapchains.empty()) {
      sc = dev->swapchains[0];
    }
    if (sc) {
      sc->present_count++;
      sc->last_present_fence = present_fence;
      if (sc->backbuffers.size() > 1 && sc->swap_effect != 0u) {
        auto is_backbuffer = [sc](const Resource* res) -> bool {
          if (!sc || !res) {
            return false;
          }
          return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
        };

        size_t needed_bytes = align_up(sizeof(aerogpu_cmd_set_render_targets), 4);
        for (uint32_t stage = 0; stage < 16; ++stage) {
          if (is_backbuffer(dev->textures[stage])) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_texture), 4);
          }
        }
        for (uint32_t stream = 0; stream < 16; ++stream) {
          if (is_backbuffer(dev->streams[stream].vb)) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding), 4);
          }
        }
        if (is_backbuffer(dev->index_buffer)) {
          needed_bytes += align_up(sizeof(aerogpu_cmd_set_index_buffer), 4);
        }

        if (ensure_cmd_space(dev, needed_bytes)) {
          const aerogpu_handle_t saved = sc->backbuffers[0]->handle;
          for (size_t i = 0; i + 1 < sc->backbuffers.size(); ++i) {
            sc->backbuffers[i]->handle = sc->backbuffers[i + 1]->handle;
          }
          sc->backbuffers.back()->handle = saved;

          bool ok = emit_set_render_targets_locked(dev);
          for (uint32_t stage = 0; ok && stage < 16; ++stage) {
            if (!is_backbuffer(dev->textures[stage])) {
              continue;
            }
            auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
            if (!cmd) {
              ok = false;
              break;
            }
            cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
            cmd->slot = stage;
            cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
            cmd->reserved0 = 0;
          }

          for (uint32_t stream = 0; ok && stream < 16; ++stream) {
            if (!is_backbuffer(dev->streams[stream].vb)) {
              continue;
            }

            aerogpu_vertex_buffer_binding binding{};
            binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
            binding.stride_bytes = dev->streams[stream].stride_bytes;
            binding.offset_bytes = dev->streams[stream].offset_bytes;
            binding.reserved0 = 0;

            auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
                dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
            if (!cmd) {
              ok = false;
              break;
            }
            cmd->start_slot = stream;
            cmd->buffer_count = 1;
          }

          if (ok && is_backbuffer(dev->index_buffer)) {
            auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
            if (!cmd) {
              ok = false;
            } else {
              cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
              cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
              cmd->offset_bytes = dev->index_offset_bytes;
              cmd->reserved0 = 0;
            }
          }

          if (!ok) {
            const aerogpu_handle_t undo_saved = sc->backbuffers.back()->handle;
            for (size_t i = sc->backbuffers.size() - 1; i > 0; --i) {
              sc->backbuffers[i]->handle = sc->backbuffers[i - 1]->handle;
            }
            sc->backbuffers[0]->handle = undo_saved;
            dev->cmd.reset();
          }
        }
      }
    }
    }
  }

  d3d9_trace_maybe_dump_on_present(present_count);
  return trace.ret(present_hr);
}

HRESULT AEROGPU_D3D9_CALL device_set_maximum_frame_latency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t max_frame_latency) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetMaximumFrameLatency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(max_frame_latency),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (max_frame_latency == 0) {
    return trace.ret(E_INVALIDARG);
  }
  dev->max_frame_latency = std::clamp(max_frame_latency, kMaxFrameLatencyMin, kMaxFrameLatencyMax);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_maximum_frame_latency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t* pMaxFrameLatency) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetMaximumFrameLatency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pMaxFrameLatency),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pMaxFrameLatency) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pMaxFrameLatency = dev->max_frame_latency;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_present_stats(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRESENTSTATS* pStats) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetPresentStats,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pStats),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pStats) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  std::memset(pStats, 0, sizeof(*pStats));
  pStats->PresentCount = dev->present_count;
  pStats->PresentRefreshCount = dev->present_refresh_count;
  pStats->SyncRefreshCount = dev->sync_refresh_count;
  pStats->SyncQPCTime = static_cast<int64_t>(dev->last_present_qpc);
  pStats->SyncGPUTime = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_last_present_count(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t* pLastPresentCount) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetLastPresentCount,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pLastPresentCount),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pLastPresentCount) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pLastPresentCount = dev->present_count;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_gpu_thread_priority(AEROGPU_D3D9DDI_HDEVICE hDevice, int32_t priority) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetGPUThreadPriority,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(static_cast<uint32_t>(priority)),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->gpu_thread_priority = std::clamp(priority, kMinGpuThreadPriority, kMaxGpuThreadPriority);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_gpu_thread_priority(AEROGPU_D3D9DDI_HDEVICE hDevice, int32_t* pPriority) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetGPUThreadPriority,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pPriority),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pPriority) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pPriority = dev->gpu_thread_priority;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_query_resource_residency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_QUERYRESOURCERESIDENCY* pArgs) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceQueryResourceResidency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pArgs ? static_cast<uint64_t>(pArgs->resource_count) : 0,
                      pArgs ? d3d9_trace_arg_ptr(pArgs->pResidencyStatus) : 0,
                      d3d9_trace_arg_ptr(pArgs));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  // System-memory-only model: resources are always considered resident.
  AEROGPU_D3D9_STUB_LOG_ONCE();

  if (pArgs && pArgs->pResidencyStatus) {
    for (uint32_t i = 0; i < pArgs->resource_count; i++) {
      pArgs->pResidencyStatus[i] = 1;
    }
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_display_mode_ex(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_GETDISPLAYMODEEX* pGetModeEx) {
  const uint64_t mode_ptr = pGetModeEx ? d3d9_trace_arg_ptr(pGetModeEx->pMode) : 0;
  const uint64_t rotation_ptr = pGetModeEx ? d3d9_trace_arg_ptr(pGetModeEx->pRotation) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetDisplayModeEx,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pGetModeEx),
                      mode_ptr,
                      rotation_ptr);
  if (!hDevice.pDrvPrivate || !pGetModeEx) {
    return trace.ret(E_INVALIDARG);
  }

  AEROGPU_D3D9_STUB_LOG_ONCE();

  auto* dev = as_device(hDevice);
  Adapter* adapter = dev->adapter;
  if (!adapter) {
    return trace.ret(E_FAIL);
  }

  if (pGetModeEx->pMode) {
    AEROGPU_D3D9DDI_DISPLAYMODEEX mode{};
    mode.size = sizeof(AEROGPU_D3D9DDI_DISPLAYMODEEX);
    mode.width = adapter->primary_width;
    mode.height = adapter->primary_height;
    mode.refresh_rate_hz = adapter->primary_refresh_hz;
    mode.format = adapter->primary_format;
    mode.scanline_ordering = AEROGPU_D3D9DDI_SCANLINEORDERING_PROGRESSIVE;
    *pGetModeEx->pMode = mode;
  }

  if (pGetModeEx->pRotation) {
    *pGetModeEx->pRotation = static_cast<AEROGPU_D3D9DDI_DISPLAYROTATION>(adapter->primary_rotation);
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_compose_rects(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_COMPOSERECTS* pComposeRects) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceComposeRects,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pComposeRects),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  // ComposeRects is used by some D3D9Ex clients (including DWM in some modes).
  // Initial bring-up: accept and no-op to keep composition alive.
  AEROGPU_D3D9_STUB_LOG_ONCE();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_flush(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceFlush, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(flush_locked(dev));
}

HRESULT AEROGPU_D3D9_CALL device_wait_for_vblank(AEROGPU_D3D9DDI_HDEVICE hDevice, uint32_t swap_chain_index) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceWaitForVBlank,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(swap_chain_index),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    sleep_ms(16);
    return trace.ret(S_OK);
  }

#if defined(_WIN32)
  uint32_t period_ms = 16;
  if (dev->adapter->primary_refresh_hz != 0) {
    period_ms = std::max<uint32_t>(1, 1000u / dev->adapter->primary_refresh_hz);
  }
  // Some display stacks (particularly remote/virtualised ones) can report bizarre
  // refresh rates (e.g. 1Hz). Clamp the computed period so WaitForVBlank remains
  // bounded and DWM never stalls for seconds.
  period_ms = std::min<uint32_t>(period_ms, 50u);

  // Prefer a real vblank wait when possible (KMD-backed scanline polling),
  // but always keep the wait bounded so DWM cannot hang if vblank delivery is
  // broken.
  const uint32_t timeout_ms = std::min<uint32_t>(40, std::max<uint32_t>(1, period_ms * 2));
  uint32_t vid_pn_source_id = 0;
  if (dev->adapter->vid_pn_source_id_valid) {
    vid_pn_source_id = dev->adapter->vid_pn_source_id;
  }
  if (dev->adapter->kmd_query.WaitForVBlank(vid_pn_source_id, timeout_ms)) {
    return trace.ret(S_OK);
  }
  sleep_ms(std::min<uint32_t>(period_ms, timeout_ms));
#else
  sleep_ms(16);
#endif
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_check_resource_residency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE* pResources,
    uint32_t count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCheckResourceResidency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(count),
                      d3d9_trace_arg_ptr(pResources),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  // System-memory-only model: resources are always considered resident.
  AEROGPU_D3D9_STUB_LOG_ONCE();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_query(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_CREATEQUERY* pCreateQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pCreateQuery ? static_cast<uint64_t>(pCreateQuery->type) : 0,
                      d3d9_trace_arg_ptr(pCreateQuery),
                      0);
  if (!hDevice.pDrvPrivate || !pCreateQuery) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  Adapter* adapter = dev->adapter;
  bool is_event = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (!adapter->event_query_type_known.load(std::memory_order_acquire)) {
      // Accept both the public D3DQUERYTYPE_EVENT (8) encoding and the DDI-style
      // encoding where EVENT is the first enum entry (0). Once observed, lock
      // in the value so we don't accidentally treat other query types as EVENT.
      if (pCreateQuery->type == 0u || pCreateQuery->type == kD3DQueryTypeEvent) {
        adapter->event_query_type.store(pCreateQuery->type, std::memory_order_relaxed);
        adapter->event_query_type_known.store(true, std::memory_order_release);
      }
    }
    const bool known = adapter->event_query_type_known.load(std::memory_order_acquire);
    const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
    is_event = known && (pCreateQuery->type == event_type);
  }

  if (!is_event) {
    pCreateQuery->hQuery.pDrvPrivate = nullptr;
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  auto q = std::make_unique<Query>();
  q->type = pCreateQuery->type;
  pCreateQuery->hQuery.pDrvPrivate = q.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_query(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HQUERY hQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hQuery.pDrvPrivate),
                      0,
                      0);
  delete as_query(hQuery);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_issue_query(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_ISSUEQUERY* pIssueQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceIssueQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pIssueQuery ? d3d9_trace_arg_ptr(pIssueQuery->hQuery.pDrvPrivate) : 0,
                      pIssueQuery ? static_cast<uint64_t>(pIssueQuery->flags) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pIssueQuery) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pIssueQuery->hQuery);
  if (!q) {
    return trace.ret(E_INVALIDARG);
  }
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  Adapter* adapter = dev->adapter;
  const bool event_known = adapter->event_query_type_known.load(std::memory_order_acquire);
  const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
  const bool is_event =
      event_known ? (q->type == event_type) : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  const uint32_t flags = pIssueQuery->flags;
  // Some runtimes appear to pass 0 for END. Be permissive and treat both 0 and
  // D3DISSUE_END (1) as an END marker. D3DISSUE_BEGIN (2) is ignored for EVENT
  // queries.
  const bool end = (flags == 0) || ((flags & kD3DIssueEnd) != 0);
  if (!end) {
    return trace.ret(S_OK);
  }

  // Ensure all prior GPU work is submitted and capture the submission fence.
  const uint64_t submit_fence = submit(dev);

  // Use the per-submission fence returned by submit(). Do not infer fence IDs
  // from a global "last submitted" query, which can be skewed by other
  // devices/processes submitting concurrently (e.g. DWM + apps).
  const uint64_t fence_value = submit_fence;

  q->fence_value.store(fence_value, std::memory_order_release);
  q->issued.store(true, std::memory_order_release);
  q->completion_logged.store(false, std::memory_order_relaxed);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_query_data(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_GETQUERYDATA* pGetQueryData) {
  const uint64_t data_flags = pGetQueryData ? d3d9_trace_pack_u32_u32(pGetQueryData->data_size, pGetQueryData->flags) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetQueryData,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pGetQueryData ? d3d9_trace_arg_ptr(pGetQueryData->hQuery.pDrvPrivate) : 0,
                      data_flags,
                      pGetQueryData ? d3d9_trace_arg_ptr(pGetQueryData->pData) : 0);
  if (!hDevice.pDrvPrivate || !pGetQueryData) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pGetQueryData->hQuery);
  if (!q) {
    return trace.ret(E_INVALIDARG);
  }

  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }
  Adapter* adapter = dev->adapter;

  const bool event_known = adapter->event_query_type_known.load(std::memory_order_acquire);
  const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
  const bool is_event =
      event_known ? (q->type == event_type) : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  const bool has_data_ptr = (pGetQueryData->pData != nullptr);
  const bool has_data_size = (pGetQueryData->data_size != 0);
  // Mirror IDirect3DQuery9::GetData validation: pData must be NULL iff data_size
  // is 0. Treat mismatched pointer/size as D3DERR_INVALIDCALL.
  if (has_data_ptr != has_data_size) {
    return trace.ret(D3DERR_INVALIDCALL);
  }

  // EVENT queries return a BOOL-like DWORD; validate the output buffer size even
  // when the query is not yet ready so callers observe D3DERR_INVALIDCALL.
  if (has_data_ptr && pGetQueryData->data_size < sizeof(uint32_t)) {
    return trace.ret(D3DERR_INVALIDCALL);
  }

  // If no output buffer provided, just report readiness via HRESULT.
  const bool need_data = has_data_ptr;

  if (!q->issued.load(std::memory_order_acquire)) {
    // D3D9 clients can call GetData before Issue(END). Treat it as "not ready"
    // rather than a hard error to keep polling code (DWM) robust.
    if (need_data && pGetQueryData->data_size >= sizeof(uint32_t)) {
      *reinterpret_cast<uint32_t*>(pGetQueryData->pData) = FALSE;
    }
    return trace.ret(S_FALSE);
  }

  const uint64_t fence_value = q->fence_value.load(std::memory_order_acquire);

  FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  if (wait_res == FenceWaitResult::NotReady && (pGetQueryData->flags & kD3DGetDataFlush)) {
    // Non-blocking GetData(FLUSH): attempt a single flush then re-check. Never
    // wait here (DWM can call into GetData while holding global locks). Also
    // avoid blocking on the device mutex: if another thread is inside the UMD
    // we skip the flush attempt and fall back to polling.
    std::unique_lock<std::mutex> dev_lock(dev->mutex, std::try_to_lock);
    if (dev_lock.owns_lock()) {
      (void)flush_locked(dev);
    }
    wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  }

  if (wait_res == FenceWaitResult::Complete) {
    if (need_data) {
      // D3DQUERYTYPE_EVENT expects a BOOL-like result.
      if (pGetQueryData->data_size < sizeof(uint32_t)) {
        return trace.ret(D3DERR_INVALIDCALL);
      }
      *reinterpret_cast<uint32_t*>(pGetQueryData->pData) = TRUE;
    }
    if (!q->completion_logged.exchange(true, std::memory_order_relaxed)) {
      uint64_t completed = 0;
      {
        std::lock_guard<std::mutex> lock(adapter->fence_mutex);
        completed = adapter->completed_fence;
      }
      logf("aerogpu-d3d9: event_query ready fence=%llu completed=%llu\n",
           static_cast<unsigned long long>(fence_value),
           static_cast<unsigned long long>(completed));
    }
    return trace.ret(S_OK);
  }
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  return trace.ret(S_FALSE);
}

HRESULT AEROGPU_D3D9_CALL device_wait_for_idle(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceWaitForIdle, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  uint64_t fence_value = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence_value = submit(dev);
  }
  if (fence_value == 0) {
    return trace.ret(S_OK);
  }

  // Never block indefinitely in a DDI call. Waiting for idle should be best-effort:
  // if the GPU stops making forward progress we return a non-fatal "still drawing"
  // code so callers can decide how to proceed.
  const uint64_t deadline = monotonic_ms() + 2000;
  while (monotonic_ms() < deadline) {
    const uint64_t now = monotonic_ms();
    const uint64_t remaining = (deadline > now) ? (deadline - now) : 0;
    const uint64_t slice = std::min<uint64_t>(remaining, 250);

    const FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/slice);
    if (wait_res == FenceWaitResult::Complete) {
      return trace.ret(S_OK);
    }
    if (wait_res == FenceWaitResult::Failed) {
      return trace.ret(E_FAIL);
    }
  }

  const FenceWaitResult final_check = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  if (final_check == FenceWaitResult::Complete) {
    return trace.ret(S_OK);
  }
  if (final_check == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  return trace.ret(kD3dErrWasStillDrawing);
}

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    D3D9DDI_DEVICEFUNCS* pDeviceFuncs) {
  const uint64_t adapter_ptr = pCreateDevice ? d3d9_trace_arg_ptr(pCreateDevice->hAdapter.pDrvPrivate) : 0;
  const uint64_t flags = pCreateDevice ? static_cast<uint64_t>(pCreateDevice->Flags) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::AdapterCreateDevice, adapter_ptr, flags, d3d9_trace_arg_ptr(pDeviceFuncs), d3d9_trace_arg_ptr(pCreateDevice));
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  if (!pCreateDevice || !pDeviceFuncs) {
    return trace.ret(E_INVALIDARG);
  }

  auto* adapter = as_adapter(pCreateDevice->hAdapter);
  if (!adapter) {
    return trace.ret(E_INVALIDARG);
  }

  std::unique_ptr<Device> dev;
  try {
    dev = std::make_unique<Device>(adapter);
  } catch (...) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Publish the device handle early so the runtime has a valid cookie for any
  // follow-up DDIs (including error paths).
  pCreateDevice->hDevice.pDrvPrivate = dev.get();

  if (!pCreateDevice->pCallbacks) {
    aerogpu::logf("aerogpu-d3d9: CreateDevice missing device callbacks\n");
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(E_INVALIDARG);
  }

  dev->wddm_callbacks = *pCreateDevice->pCallbacks;

  HRESULT hr = wddm_create_device(dev->wddm_callbacks, adapter, &dev->wddm_device);
  if (FAILED(hr)) {
    aerogpu::logf("aerogpu-d3d9: CreateDeviceCb failed hr=0x%08x\n", static_cast<unsigned>(hr));
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(hr);
  }

  hr = wddm_create_context(dev->wddm_callbacks, dev->wddm_device, &dev->wddm_context);
  if (FAILED(hr)) {
    aerogpu::logf("aerogpu-d3d9: CreateContextCb failed hr=0x%08x\n", static_cast<unsigned>(hr));
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(hr);
  }

  // If the adapter wasn't opened through a path that initialized our KMD query
  // helper (e.g. missing HDC at OpenAdapter time), opportunistically initialize
  // it here. This enables fence polling when hSyncObject is absent/zero.
  if (!adapter->kmd_query_available.load(std::memory_order_acquire)) {
    bool kmd_ok = false;
    if (adapter->luid.LowPart != 0 || adapter->luid.HighPart != 0) {
      kmd_ok = adapter->kmd_query.InitFromLuid(adapter->luid);
    }
    if (!kmd_ok) {
      HDC hdc = GetDC(nullptr);
      if (hdc) {
        kmd_ok = adapter->kmd_query.InitFromHdc(hdc);
        ReleaseDC(nullptr, hdc);
      }
    }
    adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
  }

  // Validate the runtime-provided submission buffers. These must be present for
  // DMA buffer construction.
  const uint32_t min_cmd_buffer_size = static_cast<uint32_t>(
      sizeof(aerogpu_cmd_stream_header) + align_up(sizeof(aerogpu_cmd_set_render_targets), 4));
  if (!dev->wddm_context.pCommandBuffer ||
      dev->wddm_context.CommandBufferSize < min_cmd_buffer_size ||
      !dev->wddm_context.pAllocationList || dev->wddm_context.AllocationListSize == 0 ||
      !dev->wddm_context.pDmaBufferPrivateData ||
      dev->wddm_context.DmaBufferPrivateDataSize < AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
    aerogpu::logf("aerogpu-d3d9: WDDM CreateContext returned invalid buffers "
                  "cmd=%p size=%u alloc=%p size=%u patch=%p size=%u dma_priv=%p bytes=%u (need>=%u) sync=0x%08x\n",
                  dev->wddm_context.pCommandBuffer,
                  static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                  dev->wddm_context.pAllocationList,
                  static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                  dev->wddm_context.pPatchLocationList,
                  static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                  dev->wddm_context.pDmaBufferPrivateData,
                  static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize),
                  static_cast<unsigned>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES),
                  static_cast<unsigned>(dev->wddm_context.hSyncObject));

    dev->wddm_context.destroy(dev->wddm_callbacks);
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(E_FAIL);
  }

  {
    static std::once_flag wddm_diag_once;
    const bool patch_list_present =
        dev->wddm_context.pPatchLocationList && dev->wddm_context.PatchLocationListSize != 0;

    const bool has_sync_object = (dev->wddm_context.hSyncObject != 0);
    const bool kmd_query_available = adapter->kmd_query_available.load(std::memory_order_acquire);
    const bool has_wait_fn = (load_d3dkmt_wait_for_sync_object() != nullptr);

    const bool sync_object_wait_available = has_sync_object && has_wait_fn;

    // `wait_for_fence()` uses different mechanisms depending on whether the caller
    // is doing a bounded wait (PresentEx throttling) or a non-blocking poll (EVENT
    // queries / GetData). Log both to make bring-up debugging on Win7 clearer.
    const char* bounded_wait_mode = "polling";
    if (sync_object_wait_available) {
      bounded_wait_mode = "sync_object";
    } else if (kmd_query_available) {
      bounded_wait_mode = "kmd_query";
    }

    const char* poll_wait_mode = "polling";
    if (kmd_query_available) {
      poll_wait_mode = "kmd_query";
    } else if (sync_object_wait_available) {
      poll_wait_mode = "sync_object";
    }

    std::call_once(wddm_diag_once,
                   [patch_list_present, bounded_wait_mode, poll_wait_mode, has_sync_object, kmd_query_available] {
      aerogpu::logf("aerogpu-d3d9: WDDM patch_list=%s (AeroGPU submits with NumPatchLocations=0)\n",
                    patch_list_present ? "present" : "absent");
      aerogpu::logf("aerogpu-d3d9: fence_wait bounded=%s poll=%s (hSyncObject=%s kmd_query=%s)\n",
                    bounded_wait_mode,
                    poll_wait_mode,
                    has_sync_object ? "present" : "absent",
                    kmd_query_available ? "available" : "unavailable");
    });
  }

  aerogpu::logf("aerogpu-d3d9: CreateDevice wddm_device=0x%08x hContext=0x%08x hSyncObject=0x%08x "
                "cmd=%p bytes=%u alloc_list=%p entries=%u patch_list=%p entries=%u dma_priv=%p bytes=%u\n",
                static_cast<unsigned>(dev->wddm_device),
                static_cast<unsigned>(dev->wddm_context.hContext),
                static_cast<unsigned>(dev->wddm_context.hSyncObject),
                dev->wddm_context.pCommandBuffer,
                static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                dev->wddm_context.pAllocationList,
                static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                dev->wddm_context.pPatchLocationList,
                static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                dev->wddm_context.pDmaBufferPrivateData,
                static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize));

  // Wire the command stream builder to the runtime-provided DMA buffer so all
  // command emission paths write directly into `pCommandBuffer` (no per-submit
  // std::vector allocations). This is a prerequisite for real Win7 D3D9UMDDI
  // submission plumbing.
  if (dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= sizeof(aerogpu_cmd_stream_header)) {
    dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  }

  // Bind the per-submit allocation list tracker to the runtime-provided buffers
  // so allocation tracking works immediately (e.g. shared surface CreateResource
  // can reference its backing allocation before the first submit()).
  dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                 dev->wddm_context.AllocationListSize,
                                 adapter->max_allocation_list_slot_id);

  std::memset(pDeviceFuncs, 0, sizeof(*pDeviceFuncs));

  // The translation layer uses a reduced set of DDI argument structs (prefixed
  // with AEROGPU_D3D9DDIARG_*). In WDK builds, cast the entrypoints to the
  // runtime's expected function pointer types.
#define AEROGPU_SET_D3D9DDI_FN(member, fn)                                                            \
  do {                                                                                                \
    pDeviceFuncs->member = reinterpret_cast<decltype(pDeviceFuncs->member)>(fn);                      \
  } while (0)

  AEROGPU_SET_D3D9DDI_FN(pfnDestroyDevice, device_destroy);
  AEROGPU_SET_D3D9DDI_FN(pfnCreateResource, device_create_resource);
  if constexpr (aerogpu_has_member_pfnOpenResource<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnOpenResource, device_open_resource);
  }
  if constexpr (aerogpu_has_member_pfnOpenResource2<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnOpenResource2, device_open_resource2);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyResource, device_destroy_resource);
  AEROGPU_SET_D3D9DDI_FN(pfnLock, device_lock);
  AEROGPU_SET_D3D9DDI_FN(pfnUnlock, device_unlock);

  AEROGPU_SET_D3D9DDI_FN(pfnSetRenderTarget, device_set_render_target);
  AEROGPU_SET_D3D9DDI_FN(pfnSetDepthStencil, device_set_depth_stencil);
  AEROGPU_SET_D3D9DDI_FN(pfnSetViewport, device_set_viewport);
  AEROGPU_SET_D3D9DDI_FN(pfnSetScissorRect, device_set_scissor);
  AEROGPU_SET_D3D9DDI_FN(pfnSetTexture, device_set_texture);
  if constexpr (aerogpu_has_member_pfnSetTextureStageState<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetTextureStageState,
        aerogpu_d3d9_stub_pfnSetTextureStageState<decltype(pDeviceFuncs->pfnSetTextureStageState)>::pfnSetTextureStageState);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnSetSamplerState, device_set_sampler_state);
  AEROGPU_SET_D3D9DDI_FN(pfnSetRenderState, device_set_render_state);
  if constexpr (aerogpu_has_member_pfnSetTransform<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetTransform,
        aerogpu_d3d9_stub_pfnSetTransform<decltype(pDeviceFuncs->pfnSetTransform)>::pfnSetTransform);
  }
  if constexpr (aerogpu_has_member_pfnMultiplyTransform<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnMultiplyTransform,
        aerogpu_d3d9_stub_pfnMultiplyTransform<decltype(pDeviceFuncs->pfnMultiplyTransform)>::pfnMultiplyTransform);
  }
  if constexpr (aerogpu_has_member_pfnSetClipPlane<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetClipPlane,
        aerogpu_d3d9_stub_pfnSetClipPlane<decltype(pDeviceFuncs->pfnSetClipPlane)>::pfnSetClipPlane);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnCreateVertexDecl, device_create_vertex_decl);
  AEROGPU_SET_D3D9DDI_FN(pfnSetVertexDecl, device_set_vertex_decl);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyVertexDecl, device_destroy_vertex_decl);
  if constexpr (aerogpu_has_member_pfnSetFVF<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetFVF, device_set_fvf);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnCreateShader, device_create_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnSetShader, device_set_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyShader, device_destroy_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnSetShaderConstF, device_set_shader_const_f);
  if constexpr (aerogpu_has_member_pfnSetShaderConstI<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetShaderConstI,
        aerogpu_d3d9_stub_pfnSetShaderConstI<decltype(pDeviceFuncs->pfnSetShaderConstI)>::pfnSetShaderConstI);
  }
  if constexpr (aerogpu_has_member_pfnSetShaderConstB<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetShaderConstB,
        aerogpu_d3d9_stub_pfnSetShaderConstB<decltype(pDeviceFuncs->pfnSetShaderConstB)>::pfnSetShaderConstB);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnSetStreamSource, device_set_stream_source);
  AEROGPU_SET_D3D9DDI_FN(pfnSetIndices, device_set_indices);
  if constexpr (aerogpu_has_member_pfnBeginScene<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnBeginScene, device_begin_scene);
  }
  if constexpr (aerogpu_has_member_pfnEndScene<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnEndScene, device_end_scene);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnClear, device_clear);
  AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitive, device_draw_primitive);
  if constexpr (aerogpu_has_member_pfnDrawPrimitiveUP<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitiveUP, device_draw_primitive_up);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnDrawIndexedPrimitive, device_draw_indexed_primitive);
  if constexpr (aerogpu_has_member_pfnDrawPrimitive2<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitive2, device_draw_primitive2);
  }
  if constexpr (aerogpu_has_member_pfnDrawIndexedPrimitive2<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawIndexedPrimitive2, device_draw_indexed_primitive2);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnCreateSwapChain, device_create_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroySwapChain, device_destroy_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnGetSwapChain, device_get_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnSetSwapChain, device_set_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnReset, device_reset);
  AEROGPU_SET_D3D9DDI_FN(pfnResetEx, device_reset_ex);
  AEROGPU_SET_D3D9DDI_FN(pfnCheckDeviceState, device_check_device_state);

  if constexpr (aerogpu_has_member_pfnWaitForVBlank<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnWaitForVBlank, device_wait_for_vblank);
  }
  if constexpr (aerogpu_has_member_pfnSetGPUThreadPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetGPUThreadPriority, device_set_gpu_thread_priority);
  }
  if constexpr (aerogpu_has_member_pfnGetGPUThreadPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetGPUThreadPriority, device_get_gpu_thread_priority);
  }
  if constexpr (aerogpu_has_member_pfnCheckResourceResidency<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnCheckResourceResidency, device_check_resource_residency);
  }
  if constexpr (aerogpu_has_member_pfnQueryResourceResidency<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnQueryResourceResidency, device_query_resource_residency);
  }
  if constexpr (aerogpu_has_member_pfnGetDisplayModeEx<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetDisplayModeEx, device_get_display_mode_ex);
  }
  if constexpr (aerogpu_has_member_pfnComposeRects<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnComposeRects, device_compose_rects);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnRotateResourceIdentities, device_rotate_resource_identities);
  AEROGPU_SET_D3D9DDI_FN(pfnPresent, device_present);
  AEROGPU_SET_D3D9DDI_FN(pfnPresentEx, device_present_ex);
  AEROGPU_SET_D3D9DDI_FN(pfnFlush, device_flush);
  AEROGPU_SET_D3D9DDI_FN(pfnSetMaximumFrameLatency, device_set_maximum_frame_latency);
  AEROGPU_SET_D3D9DDI_FN(pfnGetMaximumFrameLatency, device_get_maximum_frame_latency);
  AEROGPU_SET_D3D9DDI_FN(pfnGetPresentStats, device_get_present_stats);
  AEROGPU_SET_D3D9DDI_FN(pfnGetLastPresentCount, device_get_last_present_count);

  AEROGPU_SET_D3D9DDI_FN(pfnCreateQuery, device_create_query);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyQuery, device_destroy_query);
  AEROGPU_SET_D3D9DDI_FN(pfnIssueQuery, device_issue_query);
  AEROGPU_SET_D3D9DDI_FN(pfnGetQueryData, device_get_query_data);
  AEROGPU_SET_D3D9DDI_FN(pfnGetRenderTargetData, device_get_render_target_data);
  AEROGPU_SET_D3D9DDI_FN(pfnCopyRects, device_copy_rects);
  AEROGPU_SET_D3D9DDI_FN(pfnWaitForIdle, device_wait_for_idle);

  AEROGPU_SET_D3D9DDI_FN(pfnBlt, device_blt);
  AEROGPU_SET_D3D9DDI_FN(pfnColorFill, device_color_fill);
  AEROGPU_SET_D3D9DDI_FN(pfnUpdateSurface, device_update_surface);
  AEROGPU_SET_D3D9DDI_FN(pfnUpdateTexture, device_update_texture);

#undef AEROGPU_SET_D3D9DDI_FN

  dev.release();
  return trace.ret(S_OK);
#else
  if (!pCreateDevice || !pDeviceFuncs) {
    return trace.ret(E_INVALIDARG);
  }
  auto* adapter = as_adapter(pCreateDevice->hAdapter);
  if (!adapter) {
    return trace.ret(E_INVALIDARG);
  }

  auto dev = std::make_unique<Device>(adapter);
  pCreateDevice->hDevice.pDrvPrivate = dev.get();

  std::memset(pDeviceFuncs, 0, sizeof(*pDeviceFuncs));
  pDeviceFuncs->pfnDestroyDevice = device_destroy;
  pDeviceFuncs->pfnCreateResource = device_create_resource;
  pDeviceFuncs->pfnOpenResource = device_open_resource;
  pDeviceFuncs->pfnOpenResource2 = device_open_resource2;
  pDeviceFuncs->pfnDestroyResource = device_destroy_resource;
  pDeviceFuncs->pfnLock = device_lock;
  pDeviceFuncs->pfnUnlock = device_unlock;

  pDeviceFuncs->pfnSetRenderTarget = device_set_render_target;
  pDeviceFuncs->pfnSetDepthStencil = device_set_depth_stencil;
  pDeviceFuncs->pfnSetViewport = device_set_viewport;
  pDeviceFuncs->pfnSetScissorRect = device_set_scissor;
  pDeviceFuncs->pfnSetTexture = device_set_texture;
  pDeviceFuncs->pfnSetSamplerState = device_set_sampler_state;
  pDeviceFuncs->pfnSetRenderState = device_set_render_state;

  pDeviceFuncs->pfnCreateVertexDecl = device_create_vertex_decl;
  pDeviceFuncs->pfnSetVertexDecl = device_set_vertex_decl;
  pDeviceFuncs->pfnDestroyVertexDecl = device_destroy_vertex_decl;
  pDeviceFuncs->pfnSetFVF = device_set_fvf;

  pDeviceFuncs->pfnCreateShader = device_create_shader;
  pDeviceFuncs->pfnSetShader = device_set_shader;
  pDeviceFuncs->pfnDestroyShader = device_destroy_shader;
  pDeviceFuncs->pfnSetShaderConstF = device_set_shader_const_f;

  pDeviceFuncs->pfnSetStreamSource = device_set_stream_source;
  pDeviceFuncs->pfnSetIndices = device_set_indices;
  pDeviceFuncs->pfnBeginScene = device_begin_scene;
  pDeviceFuncs->pfnEndScene = device_end_scene;

  pDeviceFuncs->pfnClear = device_clear;
  pDeviceFuncs->pfnDrawPrimitive = device_draw_primitive;
  pDeviceFuncs->pfnDrawPrimitiveUP = device_draw_primitive_up;
  pDeviceFuncs->pfnDrawIndexedPrimitive = device_draw_indexed_primitive;
  pDeviceFuncs->pfnDrawPrimitive2 = device_draw_primitive2;
  pDeviceFuncs->pfnDrawIndexedPrimitive2 = device_draw_indexed_primitive2;
  pDeviceFuncs->pfnCreateSwapChain = device_create_swap_chain;
  pDeviceFuncs->pfnDestroySwapChain = device_destroy_swap_chain;
  pDeviceFuncs->pfnGetSwapChain = device_get_swap_chain;
  pDeviceFuncs->pfnSetSwapChain = device_set_swap_chain;
  pDeviceFuncs->pfnReset = device_reset;
  pDeviceFuncs->pfnResetEx = device_reset_ex;
  pDeviceFuncs->pfnCheckDeviceState = device_check_device_state;
  pDeviceFuncs->pfnWaitForVBlank = device_wait_for_vblank;
  pDeviceFuncs->pfnSetGPUThreadPriority = device_set_gpu_thread_priority;
  pDeviceFuncs->pfnGetGPUThreadPriority = device_get_gpu_thread_priority;
  pDeviceFuncs->pfnCheckResourceResidency = device_check_resource_residency;
  pDeviceFuncs->pfnQueryResourceResidency = device_query_resource_residency;
  pDeviceFuncs->pfnGetDisplayModeEx = device_get_display_mode_ex;
  pDeviceFuncs->pfnComposeRects = device_compose_rects;
  pDeviceFuncs->pfnRotateResourceIdentities = device_rotate_resource_identities;
  pDeviceFuncs->pfnPresent = device_present;
  pDeviceFuncs->pfnPresentEx = device_present_ex;
  pDeviceFuncs->pfnFlush = device_flush;
  pDeviceFuncs->pfnSetMaximumFrameLatency = device_set_maximum_frame_latency;
  pDeviceFuncs->pfnGetMaximumFrameLatency = device_get_maximum_frame_latency;
  pDeviceFuncs->pfnGetPresentStats = device_get_present_stats;
  pDeviceFuncs->pfnGetLastPresentCount = device_get_last_present_count;

  pDeviceFuncs->pfnCreateQuery = device_create_query;
  pDeviceFuncs->pfnDestroyQuery = device_destroy_query;
  pDeviceFuncs->pfnIssueQuery = device_issue_query;
  pDeviceFuncs->pfnGetQueryData = device_get_query_data;
  pDeviceFuncs->pfnGetRenderTargetData = device_get_render_target_data;
  pDeviceFuncs->pfnCopyRects = device_copy_rects;
  pDeviceFuncs->pfnWaitForIdle = device_wait_for_idle;

  pDeviceFuncs->pfnBlt = device_blt;
  pDeviceFuncs->pfnColorFill = device_color_fill;
  pDeviceFuncs->pfnUpdateSurface = device_update_surface;
  pDeviceFuncs->pfnUpdateTexture = device_update_texture;

  dev.release();
  return trace.ret(S_OK);
#endif
}

HRESULT OpenAdapterCommon(const char* entrypoint,
                          UINT interface_version,
                          UINT umd_version,
                          D3DDDI_ADAPTERCALLBACKS* callbacks,
                          D3DDDI_ADAPTERCALLBACKS2* callbacks2,
                          const LUID& luid,
                          D3D9DDI_HADAPTER* phAdapter,
                          D3D9DDI_ADAPTERFUNCS* pAdapterFuncs) {
  if (!entrypoint || !phAdapter || !pAdapterFuncs) {
    return E_INVALIDARG;
  }

#if defined(_WIN32)
  // Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
  // correct UMD bitness was loaded (System32 vs SysWOW64).
  static bool logged_module_path = false;
  if (!logged_module_path) {
    logged_module_path = true;

    HMODULE module = NULL;
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                               GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCSTR>(&OpenAdapterCommon),
                           &module)) {
      char path[MAX_PATH] = {};
      if (GetModuleFileNameA(module, path, static_cast<DWORD>(sizeof(path))) != 0) {
        aerogpu::logf("aerogpu-d3d9: module_path=%s\n", path);
      }
    }
  }
#endif

  if (interface_version == 0 || umd_version == 0) {
    aerogpu::logf("aerogpu-d3d9: %s invalid interface/version (%u/%u)\n",
                  entrypoint,
                  static_cast<unsigned>(interface_version),
                  static_cast<unsigned>(umd_version));
    return E_INVALIDARG;
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  // The D3D runtime passes a D3D_UMD_INTERFACE_VERSION in the OpenAdapter args.
  // Be defensive: if the runtime asks for a newer interface than the headers we
  // are compiled against, fail cleanly rather than returning a vtable that does
  // not match what the runtime expects.
  if (interface_version > D3D_UMD_INTERFACE_VERSION) {
    aerogpu::logf("aerogpu-d3d9: %s unsupported interface_version=%u (compiled=%u)\n",
                  entrypoint,
                  static_cast<unsigned>(interface_version),
                  static_cast<unsigned>(D3D_UMD_INTERFACE_VERSION));
    return E_INVALIDARG;
  }
#endif

  Adapter* adapter = acquire_adapter(luid, interface_version, umd_version, callbacks, callbacks2);
  if (!adapter) {
    return E_OUTOFMEMORY;
  }

  phAdapter->pDrvPrivate = adapter;

  std::memset(pAdapterFuncs, 0, sizeof(*pAdapterFuncs));
  pAdapterFuncs->pfnCloseAdapter = adapter_close;
  pAdapterFuncs->pfnGetCaps = adapter_get_caps;
  pAdapterFuncs->pfnCreateDevice = adapter_create_device;
  pAdapterFuncs->pfnQueryAdapterInfo = adapter_query_adapter_info;

  aerogpu::logf("aerogpu-d3d9: %s Interface=%u Version=%u LUID=%08x:%08x\n",
                entrypoint,
                static_cast<unsigned>(interface_version),
                static_cast<unsigned>(umd_version),
                static_cast<unsigned>(luid.HighPart),
                static_cast<unsigned>(luid.LowPart));
  return S_OK;
}

} // namespace

uint64_t submit_locked(Device* dev, bool is_present) {
  return submit(dev, is_present);
}

aerogpu_handle_t allocate_global_handle(Adapter* adapter) {
  if (!adapter) {
    return 0;
  }

#if defined(_WIN32)
  // Protocol object handles must be stable and avoid collisions across guest
  // processes. Prefer deriving them from the same cross-process counter used
  // for shared alloc_id generation.
  for (;;) {
    const uint64_t token = allocate_shared_alloc_id_token(adapter);
    const aerogpu_handle_t handle = static_cast<aerogpu_handle_t>(token & 0xFFFFFFFFu);
    if (handle != 0) {
      return handle;
    }
  }
#else
  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == 0) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
#endif
}

} // namespace aerogpu

// -----------------------------------------------------------------------------
// Public entrypoints
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL OpenAdapter(
    D3DDDIARG_OPENADAPTER* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapter,
                               iface_version,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0,
                               0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  // Some runtimes may call OpenAdapter/OpenAdapter2 without providing an HDC or
  // explicit LUID. Resolve a stable LUID from the primary display so the adapter
  // cache and KMD query helpers can be shared with OpenAdapterFromHdc/Luid.
  HDC hdc = GetDC(nullptr);
  if (hdc) {
    if (!aerogpu::get_luid_from_hdc(hdc, &luid)) {
      aerogpu::logf("aerogpu-d3d9: OpenAdapter failed to resolve adapter LUID from primary HDC\n");
    }
  }
#endif
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
#if defined(_WIN32)
    if (hdc) {
      ReleaseDC(nullptr, hdc);
    }
#endif
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapter",
                                                 get_interface_version(pOpenAdapter),
                                                 pOpenAdapter->Version,
                                                 pOpenAdapter->pAdapterCallbacks,
                                                 get_adapter_callbacks2(pOpenAdapter),
                                                 luid,
                                                 &pOpenAdapter->hAdapter,
                                                 adapter_funcs);
#if defined(_WIN32)
  if (SUCCEEDED(hr) && hdc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(hdc, HORZRES);
      const int h = GetDeviceCaps(hdc, VERTRES);
      const int refresh = GetDeviceCaps(hdc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }

    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(hdc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
  if (hdc) {
    ReleaseDC(nullptr, hdc);
  }
#endif
  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapter2(
    D3DDDIARG_OPENADAPTER2* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapter2,
                               iface_version,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0,
                               0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  HDC hdc = GetDC(nullptr);
  if (hdc) {
    if (!aerogpu::get_luid_from_hdc(hdc, &luid)) {
      aerogpu::logf("aerogpu-d3d9: OpenAdapter2 failed to resolve adapter LUID from primary HDC\n");
    }
  }
#endif
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
#if defined(_WIN32)
    if (hdc) {
      ReleaseDC(nullptr, hdc);
    }
#endif
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapter2",
                                                 get_interface_version(pOpenAdapter),
                                                 pOpenAdapter->Version,
                                                 pOpenAdapter->pAdapterCallbacks,
                                                 get_adapter_callbacks2(pOpenAdapter),
                                                 luid,
                                                 &pOpenAdapter->hAdapter,
                                                 adapter_funcs);
#if defined(_WIN32)
  if (SUCCEEDED(hr) && hdc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(hdc, HORZRES);
      const int h = GetDeviceCaps(hdc, VERTRES);
      const int refresh = GetDeviceCaps(hdc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }

    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(hdc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
  if (hdc) {
    ReleaseDC(nullptr, hdc);
  }
#endif
  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapterFromHdc(
    D3DDDIARG_OPENADAPTERFROMHDC* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapterFromHdc,
                               iface_version,
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->hDc) : 0,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  if (pOpenAdapter->hDc && !aerogpu::get_luid_from_hdc(pOpenAdapter->hDc, &luid)) {
    aerogpu::logf("aerogpu-d3d9: OpenAdapterFromHdc failed to resolve adapter LUID from HDC\n");
  }
#endif
  pOpenAdapter->AdapterLuid = luid;

  aerogpu::logf("aerogpu-d3d9: OpenAdapterFromHdc hdc=%p LUID=%08x:%08x\n",
                pOpenAdapter->hDc,
                static_cast<unsigned>(luid.HighPart),
                static_cast<unsigned>(luid.LowPart));
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapterFromHdc",
                                                get_interface_version(pOpenAdapter),
                                                pOpenAdapter->Version,
                                                pOpenAdapter->pAdapterCallbacks,
                                                get_adapter_callbacks2(pOpenAdapter),
                                                luid,
                                                &pOpenAdapter->hAdapter,
                                                adapter_funcs);

#if defined(_WIN32)
  if (SUCCEEDED(hr) && pOpenAdapter->hDc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(pOpenAdapter->hDc, HORZRES);
      const int h = GetDeviceCaps(pOpenAdapter->hDc, VERTRES);
      const int refresh = GetDeviceCaps(pOpenAdapter->hDc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }
    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(pOpenAdapter->hDc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
#endif

  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapterFromLuid(
    D3DDDIARG_OPENADAPTERFROMLUID* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  const uint64_t luid_packed = pOpenAdapter
                                  ? aerogpu::d3d9_trace_pack_u32_u32(pOpenAdapter->AdapterLuid.LowPart,
                                                                     static_cast<uint32_t>(pOpenAdapter->AdapterLuid.HighPart))
                                  : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapterFromLuid,
                               iface_version,
                               luid_packed,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  const LUID luid = pOpenAdapter->AdapterLuid;
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapterFromLuid",
                                                get_interface_version(pOpenAdapter),
                                                pOpenAdapter->Version,
                                                pOpenAdapter->pAdapterCallbacks,
                                                get_adapter_callbacks2(pOpenAdapter),
                                                luid,
                                                &pOpenAdapter->hAdapter,
                                                adapter_funcs);

#if defined(_WIN32)
  if (SUCCEEDED(hr)) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    const bool kmd_ok = adapter && adapter->kmd_query.InitFromLuid(luid);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
#endif

  return trace.ret(hr);
}
