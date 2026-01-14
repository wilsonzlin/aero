#include "aerogpu_kmd_query.h"

#include <cstddef>
#include <cstring>
#include <type_traits>
#include <utility>

#if defined(_WIN32)
  #include "aerogpu_dbgctl_escape.h"
#if defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  #include <d3dkmthk.h>
#endif
#endif

namespace aerogpu {

#if defined(_WIN32)

namespace {

constexpr bool NtSuccess(long st) {
  return st >= 0;
}

bool ReadU32At(const void* data, size_t data_size, size_t offset, uint32_t* out) {
  if (!out) {
    return false;
  }
  if (!data || data_size < offset + sizeof(uint32_t)) {
    return false;
  }
  std::memcpy(out, static_cast<const uint8_t*>(data) + offset, sizeof(uint32_t));
  return true;
}

bool ReadU64At(const void* data, size_t data_size, size_t offset, uint64_t* out) {
  if (!out) {
    return false;
  }
  if (!data || data_size < offset + sizeof(uint64_t)) {
    return false;
  }
  std::memcpy(out, static_cast<const uint8_t*>(data) + offset, sizeof(uint64_t));
  return true;
}

enum D3DKMT_ESCAPETYPE {
  D3DKMT_ESCAPE_DRIVERPRIVATE = 0,
};

struct D3DKMT_ESCAPEFLAGS {
  union {
    struct {
      unsigned int HardwareAccess : 1;
      unsigned int Reserved : 31;
    };
    unsigned int Value;
  };
};

static_assert(sizeof(aerogpu_escape_query_fence_out) == 48, "aerogpu_escape_query_fence_out ABI mismatch");

// Minimal portable definition for the Win7 `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT`
// ABI.
//
// When building against the real WDK headers, `AerogpuD3DKMTWaitForSynchronizationObject`
// is validated against `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT` via static assertions.
// Repository builds (no WDK headers) use this struct directly when calling the
// gdi32.dll thunk.
// D3DKMT wait structs are defined with 8-byte packing. Be explicit so WOW64/x86
// builds don't accidentally inherit a different packing configuration from
// downstream toolchains.
#pragma pack(push, 8)
struct AerogpuD3DKMTWaitForSynchronizationObject {
  uint32_t ObjectCount;
  union {
    const uint32_t* ObjectHandleArray;
    uint32_t hSyncObjects; // single-handle alias used by some header revisions
  };
  union {
    const uint64_t* FenceValueArray;
    uint64_t FenceValue; // single-fence alias in some headers
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

#if defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
template <typename T, typename = void>
struct has_member_ObjectHandleArray : std::false_type {};
template <typename T>
struct has_member_ObjectHandleArray<T, std::void_t<decltype(std::declval<T&>().ObjectHandleArray)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hSyncObjects : std::false_type {};
template <typename T>
struct has_member_hSyncObjects<T, std::void_t<decltype(std::declval<T&>().hSyncObjects)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValueArray : std::false_type {};
template <typename T>
struct has_member_FenceValueArray<T, std::void_t<decltype(std::declval<T&>().FenceValueArray)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValue : std::false_type {};
template <typename T>
struct has_member_FenceValue<T, std::void_t<decltype(std::declval<T&>().FenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Timeout : std::false_type {};
template <typename T>
struct has_member_Timeout<T, std::void_t<decltype(std::declval<T&>().Timeout)>> : std::true_type {};

template <typename WdkT,
          bool HasObjectHandleArray = has_member_ObjectHandleArray<WdkT>::value,
          bool HasHSyncObjects = has_member_hSyncObjects<WdkT>::value>
struct AerogpuWaitSyncObjObjectHandleOffsetAssert {};

template <typename WdkT>
struct AerogpuWaitSyncObjObjectHandleOffsetAssert<WdkT, true, false> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == offsetof(WdkT, ObjectHandleArray),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (ObjectHandleArray offset)");
};

template <typename WdkT>
struct AerogpuWaitSyncObjObjectHandleOffsetAssert<WdkT, false, true> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == offsetof(WdkT, hSyncObjects),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (hSyncObjects offset)");
};

template <typename WdkT>
struct AerogpuWaitSyncObjObjectHandleOffsetAssert<WdkT, true, true> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == offsetof(WdkT, ObjectHandleArray),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (ObjectHandleArray offset)");
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectHandleArray) == offsetof(WdkT, hSyncObjects),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (hSyncObjects offset)");
};

template <typename WdkT,
          bool HasFenceValueArray = has_member_FenceValueArray<WdkT>::value,
          bool HasFenceValue = has_member_FenceValue<WdkT>::value>
struct AerogpuWaitSyncObjFenceValueOffsetAssert {};

template <typename WdkT>
struct AerogpuWaitSyncObjFenceValueOffsetAssert<WdkT, true, false> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == offsetof(WdkT, FenceValueArray),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (FenceValueArray offset)");
};

template <typename WdkT>
struct AerogpuWaitSyncObjFenceValueOffsetAssert<WdkT, false, true> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == offsetof(WdkT, FenceValue),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (FenceValue offset)");
};

template <typename WdkT>
struct AerogpuWaitSyncObjFenceValueOffsetAssert<WdkT, true, true> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == offsetof(WdkT, FenceValueArray),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (FenceValueArray offset)");
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, FenceValueArray) == offsetof(WdkT, FenceValue),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (FenceValue offset)");
};

template <typename WdkT, bool HasTimeout = has_member_Timeout<WdkT>::value>
struct AerogpuWaitSyncObjTimeoutOffsetAssert {};

template <typename WdkT>
struct AerogpuWaitSyncObjTimeoutOffsetAssert<WdkT, true> {
  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, Timeout) == offsetof(WdkT, Timeout),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (Timeout offset)");
};

template <typename WdkT>
struct AerogpuWaitForSyncObjectAbiAsserts
    : AerogpuWaitSyncObjObjectHandleOffsetAssert<WdkT>,
      AerogpuWaitSyncObjFenceValueOffsetAssert<WdkT>,
      AerogpuWaitSyncObjTimeoutOffsetAssert<WdkT> {
  static_assert(has_member_ObjectHandleArray<WdkT>::value || has_member_hSyncObjects<WdkT>::value,
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT missing sync-object handle member");
  static_assert(has_member_FenceValueArray<WdkT>::value || has_member_FenceValue<WdkT>::value,
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT missing fence value member");
  static_assert(has_member_Timeout<WdkT>::value, "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT missing Timeout member");
  static_assert(sizeof(AerogpuD3DKMTWaitForSynchronizationObject) == sizeof(WdkT),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (size)");

  static_assert(offsetof(AerogpuD3DKMTWaitForSynchronizationObject, ObjectCount) == offsetof(WdkT, ObjectCount),
                "D3DKMT_WAITFORSYNCHRONIZATIONOBJECT ABI mismatch (ObjectCount offset)");
};

// Instantiate ABI checks for the WDK struct when available.
template struct AerogpuWaitForSyncObjectAbiAsserts<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>;
#endif

FARPROC LoadD3dkmtWaitForSyncObjectProc() {
  static FARPROC proc = []() -> FARPROC {
    HMODULE gdi32 = GetModuleHandleW(L"gdi32.dll");
    if (!gdi32) {
      gdi32 = LoadLibraryW(L"gdi32.dll");
    }
    if (!gdi32) {
      return nullptr;
    }
    return GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject");
  }();
  return proc;
}

} // namespace

// Minimal D3DKMT ABI declarations for Win7 user-mode calls. These must match
// the gdi32.dll exported function ABI.
struct AerogpuKmdQuery::D3DKMT_OPENADAPTERFROMLUID {
  LUID AdapterLuid;       // in
  D3DKMT_HANDLE hAdapter; // out
};

struct AerogpuKmdQuery::D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;                // in
  D3DKMT_HANDLE hAdapter; // out
  LUID AdapterLuid;       // out
  unsigned int VidPnSourceId; // out
};

struct AerogpuKmdQuery::D3DKMT_CLOSEADAPTER {
  D3DKMT_HANDLE hAdapter; // in
};

struct AerogpuKmdQuery::D3DKMT_QUERYADAPTERINFO {
  D3DKMT_HANDLE hAdapter;
  unsigned int Type;
  void* pPrivateDriverData;
  unsigned int PrivateDriverDataSize;
};

struct AerogpuKmdQuery::D3DKMT_ESCAPE {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  D3DKMT_HANDLE hContext;
  D3DKMT_ESCAPETYPE Type;
  D3DKMT_ESCAPEFLAGS Flags;
  void* pPrivateDriverData;
  unsigned int PrivateDriverDataSize;
};

struct AerogpuKmdQuery::D3DKMT_GETSCANLINE {
  D3DKMT_HANDLE hAdapter;
  unsigned int VidPnSourceId;
  BOOL InVerticalBlank;
  unsigned int ScanLine;
};

AerogpuKmdQuery::AerogpuKmdQuery() = default;

AerogpuKmdQuery::~AerogpuKmdQuery() noexcept {
  // Destructors are implicitly `noexcept`; be defensive so failures in the
  // best-effort shutdown path (e.g. mutex lock errors) cannot trigger
  // `std::terminate` during adapter teardown.
  try {
    Shutdown();
  } catch (...) {
  }
}

bool AerogpuKmdQuery::InitFromLuid(LUID adapter_luid) {
  std::lock_guard<std::mutex> lock(mutex_);
  ShutdownLocked();

  gdi32_ = LoadLibraryW(L"gdi32.dll");
  if (!gdi32_) {
    return false;
  }

  open_adapter_from_luid_ =
      reinterpret_cast<PFND3DKMTOpenAdapterFromLuid>(GetProcAddress(gdi32_, "D3DKMTOpenAdapterFromLuid"));
  open_adapter_from_hdc_ =
      reinterpret_cast<PFND3DKMTOpenAdapterFromHdc>(GetProcAddress(gdi32_, "D3DKMTOpenAdapterFromHdc"));
  close_adapter_ =
      reinterpret_cast<PFND3DKMTCloseAdapter>(GetProcAddress(gdi32_, "D3DKMTCloseAdapter"));
  query_adapter_info_ =
      reinterpret_cast<PFND3DKMTQueryAdapterInfo>(GetProcAddress(gdi32_, "D3DKMTQueryAdapterInfo"));
  escape_ = reinterpret_cast<PFND3DKMTEscape>(GetProcAddress(gdi32_, "D3DKMTEscape"));
  get_scanline_ = reinterpret_cast<PFND3DKMTGetScanLine>(GetProcAddress(gdi32_, "D3DKMTGetScanLine"));
  wait_for_sync_object_ = GetProcAddress(gdi32_, "D3DKMTWaitForSynchronizationObject");

  if (!close_adapter_ || !escape_) {
    ShutdownLocked();
    return false;
  }
  if (!open_adapter_from_luid_ && !open_adapter_from_hdc_) {
    ShutdownLocked();
    return false;
  }

  // Preferred path: open directly from LUID.
  if (open_adapter_from_luid_) {
    D3DKMT_OPENADAPTERFROMLUID data{};
    data.AdapterLuid = adapter_luid;
    data.hAdapter = 0;
    const NTSTATUS st = open_adapter_from_luid_(&data);
    if (NtSuccess(st) && data.hAdapter != 0) {
      adapter_ = data.hAdapter;
      adapter_luid_ = adapter_luid;
      vid_pn_source_id_ = 0;
      vid_pn_source_id_valid_ = false;
      if (query_adapter_info_) {
        ProbeUmdPrivateTypeLocked();
      }

      // Best-effort: resolve the VidPnSourceId by enumerating display HDCs. The
      // LUID open path does not provide it, but having a valid source ID enables
      // a more accurate vblank wait via D3DKMTGetScanLine.
      if (open_adapter_from_hdc_) {
        DISPLAY_DEVICEW dd;
        std::memset(&dd, 0, sizeof(dd));
        dd.cb = sizeof(dd);

        for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
          const bool active = (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0;
          if (!active) {
            std::memset(&dd, 0, sizeof(dd));
            dd.cb = sizeof(dd);
            continue;
          }

          HDC hdc = CreateDCW(L"DISPLAY", dd.DeviceName, nullptr, nullptr);
          if (!hdc) {
            std::memset(&dd, 0, sizeof(dd));
            dd.cb = sizeof(dd);
            continue;
          }

          D3DKMT_OPENADAPTERFROMHDC open_hdc{};
          open_hdc.hDc = hdc;
          open_hdc.hAdapter = 0;
          std::memset(&open_hdc.AdapterLuid, 0, sizeof(open_hdc.AdapterLuid));
          open_hdc.VidPnSourceId = 0;

          const NTSTATUS st_hdc = open_adapter_from_hdc_(&open_hdc);
          DeleteDC(hdc);

          if (!NtSuccess(st_hdc) || open_hdc.hAdapter == 0) {
            std::memset(&dd, 0, sizeof(dd));
            dd.cb = sizeof(dd);
            continue;
          }

          const bool luid_match = (open_hdc.AdapterLuid.LowPart == adapter_luid.LowPart) &&
                                  (open_hdc.AdapterLuid.HighPart == adapter_luid.HighPart);

          // Close the temporary handle regardless of match; we keep the handle
          // returned by D3DKMTOpenAdapterFromLuid.
          D3DKMT_CLOSEADAPTER close{};
          close.hAdapter = open_hdc.hAdapter;
          close_adapter_(&close);

          if (luid_match) {
            vid_pn_source_id_ = open_hdc.VidPnSourceId;
            vid_pn_source_id_valid_ = true;
            break;
          }

          std::memset(&dd, 0, sizeof(dd));
          dd.cb = sizeof(dd);
        }
      }

      return true;
    }
  }

  // Fallback path: match the LUID by enumerating display HDCs.
  if (!open_adapter_from_hdc_) {
    ShutdownLocked();
    return false;
  }

  DISPLAY_DEVICEW dd;
  std::memset(&dd, 0, sizeof(dd));
  dd.cb = sizeof(dd);

  bool opened = false;
  for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
    const bool active = (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0;
    if (!active) {
      std::memset(&dd, 0, sizeof(dd));
      dd.cb = sizeof(dd);
      continue;
    }

    HDC hdc = CreateDCW(L"DISPLAY", dd.DeviceName, nullptr, nullptr);
    if (!hdc) {
      std::memset(&dd, 0, sizeof(dd));
      dd.cb = sizeof(dd);
      continue;
    }

    D3DKMT_OPENADAPTERFROMHDC open_hdc{};
    open_hdc.hDc = hdc;
    open_hdc.hAdapter = 0;
    std::memset(&open_hdc.AdapterLuid, 0, sizeof(open_hdc.AdapterLuid));
    open_hdc.VidPnSourceId = 0;

    const NTSTATUS st = open_adapter_from_hdc_(&open_hdc);
    DeleteDC(hdc);

    if (!NtSuccess(st) || open_hdc.hAdapter == 0) {
      std::memset(&dd, 0, sizeof(dd));
      dd.cb = sizeof(dd);
      continue;
    }

    const bool luid_match = (open_hdc.AdapterLuid.LowPart == adapter_luid.LowPart) &&
                            (open_hdc.AdapterLuid.HighPart == adapter_luid.HighPart);
    if (!luid_match) {
      D3DKMT_CLOSEADAPTER close{};
      close.hAdapter = open_hdc.hAdapter;
      close_adapter_(&close);

      std::memset(&dd, 0, sizeof(dd));
      dd.cb = sizeof(dd);
      continue;
    }

    adapter_ = open_hdc.hAdapter;
    adapter_luid_ = open_hdc.AdapterLuid;
    vid_pn_source_id_ = open_hdc.VidPnSourceId;
    vid_pn_source_id_valid_ = true;
    if (query_adapter_info_) {
      ProbeUmdPrivateTypeLocked();
    }
    opened = true;
    break;
  }

  if (!opened) {
    ShutdownLocked();
  }

  return opened;
}

bool AerogpuKmdQuery::InitFromHdc(HDC hdc) {
  if (!hdc) {
    return false;
  }

  std::lock_guard<std::mutex> lock(mutex_);
  ShutdownLocked();

  gdi32_ = LoadLibraryW(L"gdi32.dll");
  if (!gdi32_) {
    return false;
  }

  open_adapter_from_hdc_ =
      reinterpret_cast<PFND3DKMTOpenAdapterFromHdc>(GetProcAddress(gdi32_, "D3DKMTOpenAdapterFromHdc"));
  close_adapter_ =
      reinterpret_cast<PFND3DKMTCloseAdapter>(GetProcAddress(gdi32_, "D3DKMTCloseAdapter"));
  query_adapter_info_ =
      reinterpret_cast<PFND3DKMTQueryAdapterInfo>(GetProcAddress(gdi32_, "D3DKMTQueryAdapterInfo"));
  escape_ = reinterpret_cast<PFND3DKMTEscape>(GetProcAddress(gdi32_, "D3DKMTEscape"));
  get_scanline_ = reinterpret_cast<PFND3DKMTGetScanLine>(GetProcAddress(gdi32_, "D3DKMTGetScanLine"));
  wait_for_sync_object_ = GetProcAddress(gdi32_, "D3DKMTWaitForSynchronizationObject");

  if (!open_adapter_from_hdc_ || !close_adapter_ || !escape_) {
    ShutdownLocked();
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC data{};
  data.hDc = hdc;
  data.hAdapter = 0;
  std::memset(&data.AdapterLuid, 0, sizeof(data.AdapterLuid));
  data.VidPnSourceId = 0;

  const NTSTATUS st = open_adapter_from_hdc_(&data);
  if (!NtSuccess(st) || data.hAdapter == 0) {
    ShutdownLocked();
    return false;
  }

  adapter_ = data.hAdapter;
  adapter_luid_ = data.AdapterLuid;
  vid_pn_source_id_ = data.VidPnSourceId;
  vid_pn_source_id_valid_ = true;
  if (query_adapter_info_) {
    ProbeUmdPrivateTypeLocked();
  }
  return true;
}

bool AerogpuKmdQuery::GetVidPnSourceId(uint32_t* out_vid_pn_source_id) {
  if (!out_vid_pn_source_id) {
    return false;
  }

  std::lock_guard<std::mutex> lock(mutex_);
  if (!vid_pn_source_id_valid_) {
    return false;
  }
  *out_vid_pn_source_id = vid_pn_source_id_;
  return true;
}

void AerogpuKmdQuery::Shutdown() {
  std::lock_guard<std::mutex> lock(mutex_);
  ShutdownLocked();
}

void AerogpuKmdQuery::ShutdownLocked() {
  if (adapter_ && close_adapter_) {
    D3DKMT_CLOSEADAPTER close{};
    close.hAdapter = adapter_;
    close_adapter_(&close);
  }

  adapter_ = 0;
  std::memset(&adapter_luid_, 0, sizeof(adapter_luid_));
  vid_pn_source_id_ = 0;
  vid_pn_source_id_valid_ = false;

  open_adapter_from_luid_ = nullptr;
  open_adapter_from_hdc_ = nullptr;
  close_adapter_ = nullptr;
  query_adapter_info_ = nullptr;
  escape_ = nullptr;
  get_scanline_ = nullptr;
  wait_for_sync_object_ = nullptr;

  umdriverprivate_type_known_ = false;
  umdriverprivate_type_ = 0;

  drivercaps_type_known_ = false;
  drivercaps_type_ = 0;
  drivercaps_wddmversion_padding_bytes_ = 4;

  if (gdi32_) {
    FreeLibrary(gdi32_);
    gdi32_ = nullptr;
  }
}

bool AerogpuKmdQuery::ProbeUmdPrivateTypeLocked() {
  umdriverprivate_type_known_ = false;
  umdriverprivate_type_ = 0;

  if (!adapter_ || !query_adapter_info_) {
    return false;
  }

  aerogpu_umd_private_v1 blob;
  std::memset(&blob, 0, sizeof(blob));

  D3DKMT_QUERYADAPTERINFO q;
  std::memset(&q, 0, sizeof(q));
  q.hAdapter = adapter_;
  q.pPrivateDriverData = &blob;
  q.PrivateDriverDataSize = static_cast<unsigned int>(sizeof(blob));

  // Avoid relying on the WDK's numeric KMTQAITYPE_UMDRIVERPRIVATE constant by probing a
  // small range of values and looking for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  for (unsigned int type = 0; type < 256; ++type) {
    std::memset(&blob, 0, sizeof(blob));
    q.Type = type;

    const NTSTATUS st = query_adapter_info_(&q);
    if (!NtSuccess(st)) {
      continue;
    }

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    umdriverprivate_type_known_ = true;
    umdriverprivate_type_ = type;
    return true;
  }

  return false;
}

bool AerogpuKmdQuery::ProbeDriverCapsTypeLocked() {
  drivercaps_type_known_ = false;
  drivercaps_type_ = 0;
  drivercaps_wddmversion_padding_bytes_ = 4;

  if (!adapter_ || !query_adapter_info_) {
    return false;
  }

  // We only need the prefix (up to MaxAllocationListSlotId), but some KMDs check
  // for the full DXGK_DRIVERCAPS size. Provide a generously sized buffer to
  // avoid STATUS_BUFFER_TOO_SMALL across WDK variants.
  alignas(8) uint8_t buf[512];
  std::memset(buf, 0, sizeof(buf));

  D3DKMT_QUERYADAPTERINFO q;
  std::memset(&q, 0, sizeof(q));
  q.hAdapter = adapter_;
  q.pPrivateDriverData = buf;
  q.PrivateDriverDataSize = static_cast<unsigned int>(sizeof(buf));

  // Avoid hard-coding the WDK's numeric KMTQAITYPE_DRIVERCAPS constant by
  // probing a small range of values and looking for a plausible DRIVERCAPS
  // layout.
  //
  // We treat a very large `HighestAcceptableAddress` as a strong signal: AeroGPU's
  // Win7 KMD sets it to all-ones. Keep this heuristic permissive so it continues
  // to work if the driver ever changes it to something less than ~0ULL.
  constexpr uint64_t kMinHighestAcceptableAddress = 0xFFFFFFFFull;

  for (unsigned int type = 0; type < 256; ++type) {
    std::memset(buf, 0, sizeof(buf));
    q.Type = type;

    const NTSTATUS st = query_adapter_info_(&q);
    if (!NtSuccess(st)) {
      continue;
    }

    // The WDK-defined DXGK_DRIVERCAPS uses MSVC packing rules (8-byte aligned
    // LARGE_INTEGER), but some non-MSVC toolchains can disagree. Probe both
    // candidate layouts:
    //   - pad=4 => HighestAcceptableAddress at offset 8 (expected on Win7).
    //   - pad=0 => HighestAcceptableAddress at offset 4.
    for (const unsigned int pad : {4u, 0u}) {
      const size_t highest_off = 4u + pad;
      const size_t dma_priv_off = 20u + pad;

      uint64_t highest = 0;
      uint32_t dma_priv = 0;
      if (!ReadU64At(buf, sizeof(buf), highest_off, &highest) ||
          !ReadU32At(buf, sizeof(buf), dma_priv_off, &dma_priv)) {
        continue;
      }

      if (highest < kMinHighestAcceptableAddress) {
        continue;
      }

      // Sanity check: DMA private data is typically small (tens of bytes). Avoid
      // mis-identifying other query types that happen to contain ~0ULL.
      if (dma_priv == 0 || dma_priv > 4096) {
        continue;
      }

      drivercaps_type_known_ = true;
      drivercaps_type_ = type;
      drivercaps_wddmversion_padding_bytes_ = pad;
      return true;
    }
  }

  return false;
}

bool AerogpuKmdQuery::QueryFence(uint64_t* last_submitted, uint64_t* last_completed) {
  if (!last_submitted && !last_completed) {
    return false;
  }

  std::lock_guard<std::mutex> lock(mutex_);
  if (!adapter_ || !escape_) {
    return false;
  }

  aerogpu_escape_query_fence_out out;
  std::memset(&out, 0, sizeof(out));
  out.hdr.version = AEROGPU_ESCAPE_VERSION;
  out.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  out.hdr.size = sizeof(out);
  out.hdr.reserved0 = 0;

  D3DKMT_ESCAPE esc{};
  std::memset(&esc, 0, sizeof(esc));
  esc.hAdapter = adapter_;
  esc.hDevice = 0;
  esc.hContext = 0;
  esc.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  esc.Flags.Value = 0;
  esc.pPrivateDriverData = &out;
  esc.PrivateDriverDataSize = static_cast<unsigned int>(sizeof(out));

  const NTSTATUS st = escape_(&esc);
  if (!NtSuccess(st)) {
    return false;
  }

  if (last_submitted) {
    *last_submitted = static_cast<uint64_t>(out.last_submitted_fence);
  }
  if (last_completed) {
    *last_completed = static_cast<uint64_t>(out.last_completed_fence);
  }
  return true;
}

bool AerogpuKmdQuery::SendEscape(void* data, uint32_t size) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!adapter_ || !escape_ || !data || size == 0) {
    return false;
  }

  D3DKMT_ESCAPE esc{};
  std::memset(&esc, 0, sizeof(esc));
  esc.hAdapter = adapter_;
  esc.hDevice = 0;
  esc.hContext = 0;
  esc.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  esc.Flags.Value = 0;
  esc.pPrivateDriverData = data;
  esc.PrivateDriverDataSize = size;

  const NTSTATUS st = escape_(&esc);
  return NtSuccess(st);
}

uint32_t AerogpuKmdQuery::GetKmtAdapterHandle() {
  std::lock_guard<std::mutex> lock(mutex_);
  return adapter_;
}

long AerogpuKmdQuery::WaitForSyncObject(uint32_t sync_object, uint64_t fence_value, uint32_t timeout_ms) {
  constexpr NTSTATUS kStatusNotSupported = static_cast<NTSTATUS>(0xC00000BBL); // STATUS_NOT_SUPPORTED
  constexpr NTSTATUS kStatusSuccess = static_cast<NTSTATUS>(0x00000000L);      // STATUS_SUCCESS

  if (fence_value == 0) {
    return kStatusSuccess;
  }
  if (!sync_object) {
    return kStatusNotSupported;
  }

  FARPROC wait_proc = nullptr;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    wait_proc = wait_for_sync_object_;
  }
  if (!wait_proc) {
    wait_proc = LoadD3dkmtWaitForSyncObjectProc();
  }
  if (!wait_proc) {
    return kStatusNotSupported;
  }

  const uint64_t timeout_kmt = (timeout_ms == INFINITE) ? ~0ull : static_cast<uint64_t>(timeout_ms);

#if defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  using WaitFn = decltype(&D3DKMTWaitForSynchronizationObject);
  auto* wait_fn = reinterpret_cast<WaitFn>(wait_proc);

  const D3DKMT_HANDLE handles[1] = {static_cast<D3DKMT_HANDLE>(sync_object)};
  const UINT64 fences[1] = {static_cast<UINT64>(fence_value)};

  D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
  args.ObjectCount = 1;
  if constexpr (has_member_ObjectHandleArray<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>::value) {
    args.ObjectHandleArray = handles;
  } else if constexpr (has_member_hSyncObjects<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>::value) {
    using FieldT = std::remove_reference_t<decltype(args.hSyncObjects)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      args.hSyncObjects = handles;
    } else {
      args.hSyncObjects = handles[0];
    }
  }
  if constexpr (has_member_FenceValueArray<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>::value) {
    args.FenceValueArray = fences;
  } else if constexpr (has_member_FenceValue<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>::value) {
    using FieldT = std::remove_reference_t<decltype(args.FenceValue)>;
    if constexpr (std::is_pointer_v<FieldT>) {
      args.FenceValue = fences;
    } else {
      args.FenceValue = fences[0];
    }
  }
  if constexpr (has_member_Timeout<D3DKMT_WAITFORSYNCHRONIZATIONOBJECT>::value) {
    args.Timeout = static_cast<decltype(args.Timeout)>(timeout_kmt);
  }

  return wait_fn(&args);
#else
  using WaitFn = NTSTATUS(WINAPI*)(AerogpuD3DKMTWaitForSynchronizationObject* pData);
  auto* wait_fn = reinterpret_cast<WaitFn>(wait_proc);

  const uint32_t handles[1] = {sync_object};
  const uint64_t fences[1] = {fence_value};

  AerogpuD3DKMTWaitForSynchronizationObject args{};
  args.ObjectCount = 1;
  args.ObjectHandleArray = handles;
  args.FenceValueArray = fences;
  args.Timeout = timeout_kmt;
  return wait_fn(&args);
#endif
}

bool AerogpuKmdQuery::QueryUmdPrivate(aerogpu_umd_private_v1* out) {
  if (!out) {
    return false;
  }

  std::lock_guard<std::mutex> lock(mutex_);
  if (!adapter_ || !query_adapter_info_) {
    return false;
  }

  if (!umdriverprivate_type_known_ && !ProbeUmdPrivateTypeLocked()) {
    return false;
  }

  std::memset(out, 0, sizeof(*out));

  D3DKMT_QUERYADAPTERINFO q;
  std::memset(&q, 0, sizeof(q));
  q.hAdapter = adapter_;
  q.Type = umdriverprivate_type_;
  q.pPrivateDriverData = out;
  q.PrivateDriverDataSize = static_cast<unsigned int>(sizeof(*out));

  const NTSTATUS st = query_adapter_info_(&q);
  if (!NtSuccess(st)) {
    return false;
  }

  if (out->size_bytes < sizeof(*out) || out->struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
    return false;
  }
  return true;
}

bool AerogpuKmdQuery::QueryMaxAllocationListSlotId(uint32_t* out_max_slot_id) {
  if (!out_max_slot_id) {
    return false;
  }

  std::lock_guard<std::mutex> lock(mutex_);
  if (!adapter_ || !query_adapter_info_) {
    return false;
  }

  if (!drivercaps_type_known_ && !ProbeDriverCapsTypeLocked()) {
    return false;
  }

  alignas(8) uint8_t buf[512];
  std::memset(buf, 0, sizeof(buf));

  D3DKMT_QUERYADAPTERINFO q;
  std::memset(&q, 0, sizeof(q));
  q.hAdapter = adapter_;
  q.Type = drivercaps_type_;
  q.pPrivateDriverData = buf;
  q.PrivateDriverDataSize = static_cast<unsigned int>(sizeof(buf));

  const NTSTATUS st = query_adapter_info_(&q);
  if (!NtSuccess(st)) {
    return false;
  }

  const size_t max_alloc_off = 12u + drivercaps_wddmversion_padding_bytes_;
  uint32_t max_alloc = 0;
  if (!ReadU32At(buf, sizeof(buf), max_alloc_off, &max_alloc)) {
    return false;
  }

  *out_max_slot_id = max_alloc;
  return true;
}

bool AerogpuKmdQuery::WaitForVBlank(uint32_t vid_pn_source_id, uint32_t timeout_ms) {
  D3DKMT_HANDLE adapter = 0;
  PFND3DKMTGetScanLine get_scanline = nullptr;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    adapter = adapter_;
    get_scanline = get_scanline_;
  }
  if (!adapter || !get_scanline) {
    return false;
  }

  D3DKMT_GETSCANLINE scan{};
  scan.hAdapter = adapter;
  scan.VidPnSourceId = vid_pn_source_id;

  const NTSTATUS st0 = get_scanline(&scan);
  if (!NtSuccess(st0)) {
    return false;
  }

  const bool started_in_vblank = (scan.InVerticalBlank != FALSE);
  bool need_exit_vblank = started_in_vblank;

  const DWORD start = GetTickCount();
  uint32_t iteration = 0;
  for (;;) {
    if (need_exit_vblank) {
      if (scan.InVerticalBlank == FALSE) {
        need_exit_vblank = false;
      }
    } else {
      if (scan.InVerticalBlank != FALSE) {
        return true;
      }
    }

    const DWORD elapsed = GetTickCount() - start;
    if (elapsed >= timeout_ms) {
      // We already waited up to the requested bound; treat as best-effort success.
      return true;
    }

    // Yield early (Sleep(0)) then back off to 1ms sleeps.
    Sleep((iteration < 4) ? 0 : 1);
    iteration++;

    std::memset(&scan, 0, sizeof(scan));
    scan.hAdapter = adapter;
    scan.VidPnSourceId = vid_pn_source_id;
    const NTSTATUS st = get_scanline(&scan);
    if (!NtSuccess(st)) {
      return false;
    }
  }
}

bool AerogpuKmdQuery::GetScanLine(uint32_t vid_pn_source_id, bool* out_in_vblank, uint32_t* out_scan_line) {
  if (out_in_vblank) {
    *out_in_vblank = false;
  }
  if (out_scan_line) {
    *out_scan_line = 0;
  }
  D3DKMT_HANDLE adapter = 0;
  PFND3DKMTGetScanLine get_scanline = nullptr;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    adapter = adapter_;
    get_scanline = get_scanline_;
  }
  if (!adapter || !get_scanline) {
    return false;
  }

  D3DKMT_GETSCANLINE scan{};
  scan.hAdapter = adapter;
  scan.VidPnSourceId = vid_pn_source_id;

  const NTSTATUS st = get_scanline(&scan);
  if (!NtSuccess(st)) {
    return false;
  }

  if (out_in_vblank) {
    *out_in_vblank = (scan.InVerticalBlank != FALSE);
  }
  if (out_scan_line) {
    *out_scan_line = scan.ScanLine;
  }
  return true;
}

bool AerogpuKmdQuery::WaitForFence(uint64_t fence, uint32_t timeout_ms) {
  const DWORD start = GetTickCount();

  uint32_t iteration = 0;
  for (;;) {
    uint64_t completed = 0;
    if (!QueryFence(nullptr, &completed)) {
      return false;
    }
    if (completed >= fence) {
      return true;
    }

    const DWORD elapsed = GetTickCount() - start;
    if (elapsed >= timeout_ms) {
      return false;
    }

    // Yield early (Sleep(0)) then back off to 1ms sleeps.
    Sleep((iteration < 4) ? 0 : 1);
    iteration++;
  }
}

#else

AerogpuKmdQuery::AerogpuKmdQuery() = default;
AerogpuKmdQuery::~AerogpuKmdQuery() = default;

bool AerogpuKmdQuery::InitFromLuid(LUID) {
  return false;
}

void AerogpuKmdQuery::Shutdown() {}

bool AerogpuKmdQuery::QueryFence(uint64_t*, uint64_t*) {
  return false;
}

bool AerogpuKmdQuery::SendEscape(void*, uint32_t) {
  return false;
}

uint32_t AerogpuKmdQuery::GetKmtAdapterHandle() {
  return 0;
}

bool AerogpuKmdQuery::GetVidPnSourceId(uint32_t*) {
  return false;
}

bool AerogpuKmdQuery::QueryUmdPrivate(aerogpu_umd_private_v1*) {
  return false;
}

bool AerogpuKmdQuery::QueryMaxAllocationListSlotId(uint32_t*) {
  return false;
}

bool AerogpuKmdQuery::WaitForVBlank(uint32_t, uint32_t) {
  return false;
}

bool AerogpuKmdQuery::GetScanLine(uint32_t, bool* out_in_vblank, uint32_t* out_scan_line) {
  if (out_in_vblank) {
    *out_in_vblank = false;
  }
  if (out_scan_line) {
    *out_scan_line = 0;
  }
  return false;
}

bool AerogpuKmdQuery::WaitForFence(uint64_t, uint32_t) {
  return false;
}

void AerogpuKmdQuery::ShutdownLocked() {}

#endif

} // namespace aerogpu
