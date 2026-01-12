#pragma once

#include "aerogpu_test_common.h"

// Win7 guest-side tests avoid taking a dependency on WDK headers. Instead, we define the minimal
// D3DKMT structures needed for:
//   - driver-private escapes (D3DKMTEscape)
//   - adapter opening (D3DKMTOpenAdapterFromHdc)
//   - adapter info queries used by UMD discovery (D3DKMTQueryAdapterInfo)

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

typedef struct D3DKMT_QUERYADAPTERINFO {
  D3DKMT_HANDLE hAdapter;
  UINT Type; /* KMTQUERYADAPTERINFOTYPE */
  VOID* pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_QUERYADAPTERINFO;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTEscape)(D3DKMT_ESCAPE* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTQueryAdapterInfo)(D3DKMT_QUERYADAPTERINFO* pData);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
  PFND3DKMTQueryAdapterInfo QueryAdapterInfo;
} D3DKMT_FUNCS;

// If an escape/query call times out, the worker thread may still be blocked inside a kernel thunk.
// In that scenario, calling D3DKMTCloseAdapter can deadlock (the kernel may be holding locks
// needed by close). Mirror the win7_dbgctl safety behavior and skip adapter close when any
// timed call has hit a timeout.
static volatile LONG g_skip_close_adapter = 0;

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
  out->QueryAdapterInfo = (PFND3DKMTQueryAdapterInfo)GetProcAddress(out->gdi32, "D3DKMTQueryAdapterInfo");

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
  // If an escape call timed out, a worker thread may still be executing inside gdi32's
  // D3DKMTEscape thunk. FreeLibrary'ing gdi32 in that scenario is unsafe (could unload code
  // while it is still in use). Skip unloading and rely on process termination instead.
  if (f->gdi32 && InterlockedCompareExchange(&g_skip_close_adapter, 0, 0) == 0) {
    FreeLibrary(f->gdi32);
  }
  ZeroMemory(f, sizeof(*f));
}

static inline bool OpenAdapterFromHdc(const D3DKMT_FUNCS* f,
                                      HDC hdc,
                                      D3DKMT_HANDLE* out_adapter,
                                      std::string* err) {
  if (err) {
    err->clear();
  }
  if (!f || !out_adapter || !f->OpenAdapterFromHdc || !hdc) {
    if (err) {
      *err = "OpenAdapterFromHdc: invalid args";
    }
    return false;
  }
  *out_adapter = 0;

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f->OpenAdapterFromHdc(&open);
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

static inline bool OpenAdapterFromHwnd(const D3DKMT_FUNCS* f,
                                       HWND hwnd,
                                       D3DKMT_HANDLE* out_adapter,
                                       std::string* err) {
  if (err) {
    err->clear();
  }
  if (!hwnd) {
    if (err) {
      *err = "OpenAdapterFromHwnd: hwnd == NULL";
    }
    return false;
  }

  HDC hdc = GetDC(hwnd);
  if (!hdc) {
    if (err) {
      *err = "GetDC(hwnd) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  const bool ok = OpenAdapterFromHdc(f, hdc, out_adapter, err);
  ReleaseDC(hwnd, hdc);
  return ok;
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

  const bool ok = OpenAdapterFromHdc(f, hdc, out_adapter, err);
  ReleaseDC(NULL, hdc);
  return ok;
}

static inline void CloseAdapter(const D3DKMT_FUNCS* f, D3DKMT_HANDLE adapter) {
  if (!f || !adapter || !f->CloseAdapter) {
    return;
  }
  if (InterlockedCompareExchange(&g_skip_close_adapter, 0, 0) != 0) {
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

struct AerogpuTimedEscapeCtx {
  const D3DKMT_FUNCS* f;
  D3DKMT_HANDLE adapter;
  std::vector<unsigned char> buf;
  NTSTATUS status;
};

static DWORD WINAPI AerogpuTimedEscapeThreadProc(LPVOID param) {
  AerogpuTimedEscapeCtx* ctx = (AerogpuTimedEscapeCtx*)param;
  if (!ctx || !ctx->f || !ctx->f->Escape || !ctx->adapter || ctx->buf.empty()) {
    if (ctx) {
      ctx->status = kStatusInvalidParameter;
    }
    return 0;
  }

  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = ctx->adapter;
  e.hDevice = 0;
  e.hContext = 0;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = &ctx->buf[0];
  e.PrivateDriverDataSize = (UINT)ctx->buf.size();
  ctx->status = ctx->f->Escape(&e);
  return 0;
}

static inline bool AerogpuEscapeWithTimeout(const D3DKMT_FUNCS* f,
                                            D3DKMT_HANDLE adapter,
                                            void* buf,
                                            UINT buf_size,
                                            DWORD timeout_ms,
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

  // Run D3DKMTEscape on a worker thread so a buggy kernel driver cannot hang the test process
  // indefinitely. If the call times out, we intentionally leak the context (the worker thread may
  // still be running) and rely on process termination to clean up.
  AerogpuTimedEscapeCtx* ctx = new AerogpuTimedEscapeCtx();
  ctx->f = f;
  ctx->adapter = adapter;
  ctx->status = 0;
  ctx->buf.assign((const unsigned char*)buf, (const unsigned char*)buf + buf_size);

  HANDLE thread = CreateThread(NULL, 0, AerogpuTimedEscapeThreadProc, ctx, 0, NULL);
  if (!thread) {
    if (out_status) {
      *out_status = (NTSTATUS)GetLastError();
    }
    delete ctx;
    return false;
  }

  DWORD w = WaitForSingleObject(thread, timeout_ms);
  if (w == WAIT_OBJECT_0) {
    CloseHandle(thread);
    if (out_status) {
      *out_status = ctx->status;
    }
    if (NtSuccess(ctx->status)) {
      memcpy(buf, &ctx->buf[0], buf_size);
      delete ctx;
      return true;
    }
    delete ctx;
    return false;
  }

  // Timeout or wait failure. Close the handle but do not free ctx (thread may still access it).
  CloseHandle(thread);
  if (out_status) {
    *out_status = (w == WAIT_TIMEOUT) ? (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */ : (NTSTATUS)GetLastError();
  }
  // If we failed to observe the worker thread exit, it may still be blocked inside the kernel
  // thunk. Avoid deadlock-prone teardown paths (CloseAdapter/FreeLibrary) in this case.
  InterlockedExchange(&g_skip_close_adapter, 1);
  return false;
}

struct D3DKMTQueryAdapterInfoCtx {
  const D3DKMT_FUNCS* f;
  D3DKMT_HANDLE adapter;
  UINT type;
  std::vector<unsigned char> buf;
  NTSTATUS status;
};

static DWORD WINAPI D3DKMTQueryAdapterInfoThreadProc(LPVOID param) {
  D3DKMTQueryAdapterInfoCtx* ctx = (D3DKMTQueryAdapterInfoCtx*)param;
  if (!ctx || !ctx->f || !ctx->f->QueryAdapterInfo || !ctx->adapter || ctx->buf.empty()) {
    if (ctx) {
      ctx->status = kStatusInvalidParameter;
    }
    return 0;
  }

  D3DKMT_QUERYADAPTERINFO q;
  ZeroMemory(&q, sizeof(q));
  q.hAdapter = ctx->adapter;
  q.Type = ctx->type;
  q.pPrivateDriverData = &ctx->buf[0];
  q.PrivateDriverDataSize = (UINT)ctx->buf.size();

  ctx->status = ctx->f->QueryAdapterInfo(&q);
  return 0;
}

static inline bool D3DKMTQueryAdapterInfoWithTimeout(const D3DKMT_FUNCS* f,
                                                     D3DKMT_HANDLE adapter,
                                                     UINT type,
                                                     void* buf,
                                                     UINT buf_size,
                                                     DWORD timeout_ms,
                                                     NTSTATUS* out_status) {
  if (out_status) {
    *out_status = 0;
  }
  if (!f || !adapter || !f->QueryAdapterInfo || !buf || buf_size == 0) {
    if (out_status) {
      *out_status = kStatusInvalidParameter;
    }
    return false;
  }

  // Run D3DKMTQueryAdapterInfo on a worker thread so a buggy kernel driver cannot hang the test
  // process indefinitely. If the call times out, we intentionally leak the context (the worker
  // thread may still be running) and rely on process termination to clean up.
  D3DKMTQueryAdapterInfoCtx* ctx = new D3DKMTQueryAdapterInfoCtx();
  ctx->f = f;
  ctx->adapter = adapter;
  ctx->type = type;
  ctx->status = 0;
  ctx->buf.assign((const unsigned char*)buf, (const unsigned char*)buf + buf_size);

  HANDLE thread = CreateThread(NULL, 0, D3DKMTQueryAdapterInfoThreadProc, ctx, 0, NULL);
  if (!thread) {
    if (out_status) {
      *out_status = (NTSTATUS)GetLastError();
    }
    delete ctx;
    return false;
  }

  DWORD w = WaitForSingleObject(thread, timeout_ms);
  if (w == WAIT_OBJECT_0) {
    CloseHandle(thread);
    if (out_status) {
      *out_status = ctx->status;
    }
    if (NtSuccess(ctx->status)) {
      memcpy(buf, &ctx->buf[0], buf_size);
      delete ctx;
      return true;
    }
    delete ctx;
    return false;
  }

  CloseHandle(thread);
  if (out_status) {
    *out_status = (w == WAIT_TIMEOUT) ? (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */ : (NTSTATUS)GetLastError();
  }
  // Avoid deadlock-prone teardown paths (CloseAdapter/FreeLibrary) in this case.
  InterlockedExchange(&g_skip_close_adapter, 1);
  return false;
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

  if (!AerogpuEscapeWithTimeout(f, adapter, &q, sizeof(q), 2000, out_status)) {
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

static inline bool AerogpuQueryVblank(const D3DKMT_FUNCS* f,
                                      D3DKMT_HANDLE adapter,
                                      uint32_t vidpn_source_id,
                                      aerogpu_escape_query_vblank_out* out_vblank,
                                      NTSTATUS* out_status) {
  if (out_vblank) {
    ZeroMemory(out_vblank, sizeof(*out_vblank));
  }
  if (!out_vblank) {
    if (out_status) {
      *out_status = kStatusInvalidParameter;
    }
    return false;
  }

  out_vblank->hdr.version = AEROGPU_ESCAPE_VERSION;
  out_vblank->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
  out_vblank->hdr.size = sizeof(*out_vblank);
  out_vblank->hdr.reserved0 = 0;
  out_vblank->vidpn_source_id = vidpn_source_id;

  return AerogpuEscapeWithTimeout(f, adapter, out_vblank, sizeof(*out_vblank), 2000, out_status);
}

static inline bool AerogpuQueryScanout(const D3DKMT_FUNCS* f,
                                       D3DKMT_HANDLE adapter,
                                       uint32_t vidpn_source_id,
                                       aerogpu_escape_query_scanout_out* out_scanout,
                                       NTSTATUS* out_status) {
  if (out_scanout) {
    ZeroMemory(out_scanout, sizeof(*out_scanout));
  }
  if (!out_scanout) {
    if (out_status) {
      *out_status = kStatusInvalidParameter;
    }
    return false;
  }

  out_scanout->hdr.version = AEROGPU_ESCAPE_VERSION;
  out_scanout->hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  out_scanout->hdr.size = sizeof(*out_scanout);
  out_scanout->hdr.reserved0 = 0;
  out_scanout->vidpn_source_id = vidpn_source_id;
  out_scanout->reserved0 = 0;

  return AerogpuEscapeWithTimeout(f, adapter, out_scanout, sizeof(*out_scanout), 2000, out_status);
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

  return AerogpuEscapeWithTimeout(f, adapter, out_dump, sizeof(*out_dump), 2000, out_status);
}

static inline bool AerogpuDumpCreateAllocationTrace(const D3DKMT_FUNCS* f,
                                                    D3DKMT_HANDLE adapter,
                                                    aerogpu_escape_dump_createallocation_inout* out_dump,
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
  out_dump->hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
  out_dump->hdr.size = sizeof(*out_dump);
  out_dump->hdr.reserved0 = 0;

  out_dump->write_index = 0;
  out_dump->entry_count = 0;
  out_dump->entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
  out_dump->reserved0 = 0;

  return AerogpuEscapeWithTimeout(f, adapter, out_dump, sizeof(*out_dump), 2000, out_status);
}

static inline bool AerogpuMapSharedHandleDebugToken(const D3DKMT_FUNCS* f,
                                                    D3DKMT_HANDLE adapter,
                                                    unsigned long long shared_handle,
                                                    uint32_t* out_token,
                                                    NTSTATUS* out_status) {
  if (out_token) {
    *out_token = 0;
  }

  aerogpu_escape_map_shared_handle_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.shared_handle = (uint64_t)shared_handle;
  q.debug_token = 0;
  q.reserved0 = 0;

  if (!AerogpuEscapeWithTimeout(f, adapter, &q, sizeof(q), 2000, out_status)) {
    return false;
  }

  if (out_token) {
    *out_token = q.debug_token;
  }
  return q.debug_token != 0;
}

// Convenience wrapper: open the primary adapter, issue MAP_SHARED_HANDLE, then close/unload.
//
// This is intended for tests that do not have an HWND handy (for example: offscreen D3D10/D3D11
// shared-resource IPC tests). It returns the 32-bit debug token when supported.
//
// NOTE: This debug token is distinct from the protocol `u64 share_token` used by
// `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE` (it exists only for bring-up tooling).
static inline bool MapSharedHandleDebugTokenPrimary(HANDLE shared_handle, uint32_t* out_token, std::string* err) {
  if (out_token) {
    *out_token = 0;
  }
  if (err) {
    err->clear();
  }
  if (!shared_handle) {
    if (err) {
      *err = "MapSharedHandleDebugTokenPrimary: shared_handle is NULL";
    }
    return false;
  }

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!LoadD3DKMT(&kmt, &kmt_err)) {
    if (err) {
      *err = kmt_err;
    }
    return false;
  }

  D3DKMT_HANDLE adapter = 0;
  if (!OpenPrimaryAdapter(&kmt, &adapter, &kmt_err)) {
    UnloadD3DKMT(&kmt);
    if (err) {
      *err = kmt_err;
    }
    return false;
  }

  uint32_t token = 0;
  NTSTATUS st = 0;
  const bool ok = AerogpuMapSharedHandleDebugToken(&kmt,
                                                   adapter,
                                                   (unsigned long long)(uintptr_t)shared_handle,
                                                   &token,
                                                   &st);

  CloseAdapter(&kmt, adapter);
  UnloadD3DKMT(&kmt);

  if (!ok) {
    if (err) {
      if (st == 0) {
        *err = "MAP_SHARED_HANDLE returned debug_token=0";
      } else {
        char buf[96];
        _snprintf(buf,
                  sizeof(buf),
                  "D3DKMTEscape(map-shared-handle) failed (NTSTATUS=0x%08lX)",
                  (unsigned long)st);
        buf[sizeof(buf) - 1] = 0;
        *err = buf;
      }
    }
    return false;
  }

  if (out_token) {
    *out_token = token;
  }
  return token != 0;
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

  // Newest descriptor is expected to be last in the returned array (tail window for AGPU).
  uint32_t idx = count - 1;

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
