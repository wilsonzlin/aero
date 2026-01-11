#include "aerogpu_kmd_query.h"

#include <cstring>

#if defined(_WIN32)
  #include "aerogpu_dbgctl_escape.h"
#endif

namespace aerogpu {

#if defined(_WIN32)

namespace {

constexpr bool NtSuccess(long st) {
  return st >= 0;
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

static_assert(sizeof(aerogpu_escape_query_fence_out) == 32, "aerogpu_escape_query_fence_out ABI mismatch");

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

AerogpuKmdQuery::~AerogpuKmdQuery() {
  Shutdown();
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
      if (query_adapter_info_) {
        ProbeUmdPrivateTypeLocked();
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
  if (query_adapter_info_) {
    ProbeUmdPrivateTypeLocked();
  }
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

  open_adapter_from_luid_ = nullptr;
  open_adapter_from_hdc_ = nullptr;
  close_adapter_ = nullptr;
  query_adapter_info_ = nullptr;
  escape_ = nullptr;
  get_scanline_ = nullptr;

  umdriverprivate_type_known_ = false;
  umdriverprivate_type_ = 0;

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

    if (blob.size_bytes != sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
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

uint32_t AerogpuKmdQuery::GetKmtAdapterHandle() {
  std::lock_guard<std::mutex> lock(mutex_);
  return adapter_;
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

  if (out->size_bytes != sizeof(*out) || out->struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
    return false;
  }
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

uint32_t AerogpuKmdQuery::GetKmtAdapterHandle() {
  return 0;
}

bool AerogpuKmdQuery::QueryUmdPrivate(aerogpu_umd_private_v1*) {
  return false;
}

bool AerogpuKmdQuery::WaitForVBlank(uint32_t, uint32_t) {
  return false;
}

bool AerogpuKmdQuery::WaitForFence(uint64_t, uint32_t) {
  return false;
}

void AerogpuKmdQuery::ShutdownLocked() {}

#endif

} // namespace aerogpu
