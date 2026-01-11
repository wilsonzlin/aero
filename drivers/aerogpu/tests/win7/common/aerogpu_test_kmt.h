#pragma once

#include "aerogpu_test_common.h"

// Win7 guest-side tests avoid taking a dependency on WDK headers. Instead, we define the minimal
// D3DKMT structures needed for driver-private escapes (D3DKMTEscape) and adapter opening
// (D3DKMTOpenAdapterFromHdc).

#include "..\\..\\..\\protocol\\aerogpu_dbgctl_escape.h"

namespace aerogpu_test {
namespace kmt {

typedef LONG NTSTATUS;

static inline bool NtSuccess(NTSTATUS st) { return st >= 0; }

static const NTSTATUS kStatusNotSupported = (NTSTATUS)0xC00000BBL;
static const NTSTATUS kStatusInvalidParameter = (NTSTATUS)0xC000000DL;

typedef UINT D3DKMT_HANDLE;

typedef struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  D3DKMT_HANDLE hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
} D3DKMT_OPENADAPTERFROMHDC;

typedef struct D3DKMT_CLOSEADAPTER {
  D3DKMT_HANDLE hAdapter;
} D3DKMT_CLOSEADAPTER;

typedef enum D3DKMT_ESCAPETYPE {
  D3DKMT_ESCAPE_DRIVERPRIVATE = 0,
} D3DKMT_ESCAPETYPE;

typedef struct D3DKMT_ESCAPEFLAGS {
  union {
    struct {
      UINT HardwareAccess : 1;
      UINT Reserved : 31;
    };
    UINT Value;
  };
} D3DKMT_ESCAPEFLAGS;

typedef struct D3DKMT_ESCAPE {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  D3DKMT_HANDLE hContext;
  D3DKMT_ESCAPETYPE Type;
  D3DKMT_ESCAPEFLAGS Flags;
  VOID* pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_ESCAPE;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTEscape)(D3DKMT_ESCAPE* pData);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
} D3DKMT_FUNCS;

static inline bool LoadD3DKMT(D3DKMT_FUNCS* out, std::string* err) {
  if (err) {
    err->clear();
  }
  if (!out) {
    if (err) {
      *err = "LoadD3DKMT: out == NULL";
    }
    return false;
  }
  ZeroMemory(out, sizeof(*out));

  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    if (err) {
      *err = "LoadLibraryW(gdi32.dll) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->Escape = (PFND3DKMTEscape)GetProcAddress(out->gdi32, "D3DKMTEscape");

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->Escape) {
    if (err) {
      *err = "Required D3DKMT* exports not found in gdi32.dll. This test requires Windows Vista+ (WDDM).";
    }
    FreeLibrary(out->gdi32);
    ZeroMemory(out, sizeof(*out));
    return false;
  }

  return true;
}

static inline void UnloadD3DKMT(D3DKMT_FUNCS* f) {
  if (!f) {
    return;
  }
  if (f->gdi32) {
    FreeLibrary(f->gdi32);
  }
  ZeroMemory(f, sizeof(*f));
}

static inline bool OpenPrimaryAdapter(const D3DKMT_FUNCS* f, D3DKMT_HANDLE* out_adapter, std::string* err) {
  if (err) {
    err->clear();
  }
  if (!f || !out_adapter || !f->OpenAdapterFromHdc) {
    if (err) {
      *err = "OpenPrimaryAdapter: invalid args";
    }
    return false;
  }
  *out_adapter = 0;

  HDC hdc = GetDC(NULL);
  if (!hdc) {
    if (err) {
      *err = "GetDC(NULL) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f->OpenAdapterFromHdc(&open);
  ReleaseDC(NULL, hdc);

  if (!NtSuccess(st) || open.hAdapter == 0) {
    if (err) {
      char buf[128];
      _snprintf(buf, sizeof(buf), "D3DKMTOpenAdapterFromHdc failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      buf[sizeof(buf) - 1] = 0;
      *err = buf;
    }
    return false;
  }

  *out_adapter = open.hAdapter;
  return true;
}

static inline void CloseAdapter(const D3DKMT_FUNCS* f, D3DKMT_HANDLE adapter) {
  if (!f || !adapter || !f->CloseAdapter) {
    return;
  }
  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = adapter;
  (void)f->CloseAdapter(&close);
}

static inline bool AerogpuEscape(const D3DKMT_FUNCS* f,
                                 D3DKMT_HANDLE adapter,
                                 void* buf,
                                 UINT buf_size,
                                 NTSTATUS* out_status) {
  if (out_status) {
    *out_status = 0;
  }
  if (!f || !adapter || !f->Escape || !buf || buf_size == 0) {
    if (out_status) {
      *out_status = kStatusInvalidParameter;
    }
    return false;
  }

  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = adapter;
  e.hDevice = 0;
  e.hContext = 0;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = buf;
  e.PrivateDriverDataSize = buf_size;

  const NTSTATUS st = f->Escape(&e);
  if (out_status) {
    *out_status = st;
  }
  return NtSuccess(st);
}

static inline bool AerogpuQueryFence(const D3DKMT_FUNCS* f,
                                     D3DKMT_HANDLE adapter,
                                     unsigned long long* out_submitted,
                                     unsigned long long* out_completed,
                                     NTSTATUS* out_status) {
  if (out_submitted) {
    *out_submitted = 0;
  }
  if (out_completed) {
    *out_completed = 0;
  }

  aerogpu_escape_query_fence_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  if (!AerogpuEscape(f, adapter, &q, sizeof(q), out_status)) {
    return false;
  }

  if (out_submitted) {
    *out_submitted = (unsigned long long)q.last_submitted_fence;
  }
  if (out_completed) {
    *out_completed = (unsigned long long)q.last_completed_fence;
  }
  return true;
}

static inline bool AerogpuDumpRingV2(const D3DKMT_FUNCS* f,
                                    D3DKMT_HANDLE adapter,
                                    uint32_t ring_id,
                                    aerogpu_escape_dump_ring_v2_inout* out_dump,
                                    NTSTATUS* out_status) {
  if (out_dump) {
    ZeroMemory(out_dump, sizeof(*out_dump));
  }
  if (!out_dump) {
    if (out_status) {
      *out_status = kStatusInvalidParameter;
    }
    return false;
  }

  out_dump->hdr.version = AEROGPU_ESCAPE_VERSION;
  out_dump->hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  out_dump->hdr.size = sizeof(*out_dump);
  out_dump->hdr.reserved0 = 0;
  out_dump->ring_id = ring_id;
  out_dump->desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  return AerogpuEscape(f, adapter, out_dump, sizeof(*out_dump), out_status);
}

static inline bool FindRingDescByFence(const aerogpu_escape_dump_ring_v2_inout& dump,
                                       unsigned long long fence,
                                       aerogpu_dbgctl_ring_desc_v2* out_desc,
                                       uint32_t* out_index) {
  if (out_desc) {
    ZeroMemory(out_desc, sizeof(*out_desc));
  }
  if (out_index) {
    *out_index = 0;
  }
  if (dump.desc_count == 0) {
    return false;
  }
  uint32_t count = dump.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }
  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_ring_desc_v2& d = dump.desc[i];
    if ((unsigned long long)d.fence == fence) {
      if (out_desc) {
        *out_desc = d;
      }
      if (out_index) {
        *out_index = i;
      }
      return true;
    }
  }
  return false;
}

static inline bool GetLastWrittenRingDesc(const aerogpu_escape_dump_ring_v2_inout& dump,
                                         aerogpu_dbgctl_ring_desc_v2* out_desc,
                                         uint32_t* out_index) {
  if (out_desc) {
    ZeroMemory(out_desc, sizeof(*out_desc));
  }
  if (out_index) {
    *out_index = 0;
  }
  if (dump.desc_count == 0) {
    return false;
  }

  uint32_t count = dump.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }

  // The KMD returns descriptors in submission order starting at `head` (oldest pending).
  // The most recently written descriptor is therefore the one at `tail - 1`, which is
  // at offset `(tail - head - 1)` from `head` when the dump isn't truncated.
  uint32_t idx = count - 1;
  if (dump.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
    const uint32_t pending = dump.tail - dump.head;  // head/tail are monotonically increasing (not masked).
    if (pending != 0 && pending <= count) {
      idx = pending - 1;
    }
  }

  if (out_desc) {
    *out_desc = dump.desc[idx];
  }
  if (out_index) {
    *out_index = idx;
  }
  return true;
}

}  // namespace kmt
}  // namespace aerogpu_test

