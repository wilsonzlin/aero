#pragma once

#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

namespace d3d9ex_shared_surface_wow64 {

static const int kWidth = 64;
static const int kHeight = 64;

static const DWORD kClearColor = D3DCOLOR_ARGB(0xFF, 0x11, 0x22, 0x33);  // 0xFF112233.
static const uint32_t kExpectedPixel = 0xFF112233u;                     // BGRA = (0x33,0x22,0x11,0xFF).

static inline std::string FormatU64Hex(uint64_t v) {
  char buf[32];
  _snprintf(buf, sizeof(buf), "0x%016I64X", (unsigned __int64)v);
  buf[sizeof(buf) - 1] = 0;
  return std::string(buf);
}

static inline std::string FormatHandleHex(HANDLE h) {
  return FormatU64Hex((uint64_t)(uintptr_t)h);
}

struct AdapterRequirements {
  AdapterRequirements()
      : allow_microsoft(false),
        allow_non_aerogpu(false),
        require_umd(false),
        has_require_vid(false),
        has_require_did(false),
        require_vid(0),
        require_did(0) {}

  bool allow_microsoft;
  bool allow_non_aerogpu;
  bool require_umd;

  bool has_require_vid;
  bool has_require_did;
  uint32_t require_vid;
  uint32_t require_did;

  // Preserve the original strings for forwarding to the consumer.
  std::string require_vid_str;
  std::string require_did_str;
};

static inline int ParseAdapterRequirements(int argc,
                                          char** argv,
                                          const char* test_name,
                                          AdapterRequirements* out,
                                          aerogpu_test::TestReporter* reporter) {
  if (!out) {
    if (reporter) {
      return reporter->Fail("internal: ParseAdapterRequirements out == NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: ParseAdapterRequirements out == NULL");
  }

  out->allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  out->allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  out->require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");

  out->has_require_vid = false;
  out->has_require_did = false;
  out->require_vid = 0;
  out->require_did = 0;
  out->require_vid_str.clear();
  out->require_did_str.clear();

  std::string vid_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(vid_str, &out->require_vid, &err)) {
      if (reporter) {
        return reporter->Fail("invalid --require-vid: %s", err.c_str());
      }
      return aerogpu_test::Fail(test_name, "invalid --require-vid: %s", err.c_str());
    }
    out->has_require_vid = true;
    out->require_vid_str = vid_str;
  }

  std::string did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(did_str, &out->require_did, &err)) {
      if (reporter) {
        return reporter->Fail("invalid --require-did: %s", err.c_str());
      }
      return aerogpu_test::Fail(test_name, "invalid --require-did: %s", err.c_str());
    }
    out->has_require_did = true;
    out->require_did_str = did_str;
  }

  return 0;
}

static inline int ValidateAdapter(const char* test_name,
                                  IDirect3D9Ex* d3d,
                                  const AdapterRequirements& req,
                                  aerogpu_test::TestReporter* reporter) {
  if (!d3d) {
    if (reporter) {
      return reporter->Fail("ValidateAdapter: d3d == NULL");
    }
    return aerogpu_test::Fail(test_name, "ValidateAdapter: d3d == NULL");
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (FAILED(hr)) {
    if (req.has_require_vid || req.has_require_did) {
      if (reporter) {
        return reporter->FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
      }
      return aerogpu_test::FailHresult(test_name,
                                       "GetAdapterIdentifier (required for --require-vid/--require-did)",
                                       hr);
    }
    return 0;
  }

  if (reporter) {
    reporter->SetAdapterInfoA(ident.Description, (uint32_t)ident.VendorId, (uint32_t)ident.DeviceId);
  }
  aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                             test_name,
                             ident.Description,
                             (unsigned)ident.VendorId,
                             (unsigned)ident.DeviceId);

  if (!req.allow_microsoft && ident.VendorId == 0x1414) {
    if (reporter) {
      return reporter->Fail("refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                            "Install AeroGPU driver or pass --allow-microsoft.",
                            (unsigned)ident.VendorId,
                            (unsigned)ident.DeviceId);
    }
    return aerogpu_test::Fail(test_name,
                              "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                              "Install AeroGPU driver or pass --allow-microsoft.",
                              (unsigned)ident.VendorId,
                              (unsigned)ident.DeviceId);
  }
  if (req.has_require_vid && ident.VendorId != req.require_vid) {
    if (reporter) {
      return reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                            (unsigned)ident.VendorId,
                            (unsigned)req.require_vid);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter VID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.VendorId,
                              (unsigned)req.require_vid);
  }
  if (req.has_require_did && ident.DeviceId != req.require_did) {
    if (reporter) {
      return reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                            (unsigned)ident.DeviceId,
                            (unsigned)req.require_did);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter DID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.DeviceId,
                              (unsigned)req.require_did);
  }
  if (!req.allow_non_aerogpu && !req.has_require_vid && !req.has_require_did &&
      !(ident.VendorId == 0x1414 && req.allow_microsoft) &&
      !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
    if (reporter) {
      return reporter->Fail("adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                            "or use --require-vid/--require-did)",
                            ident.Description);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                              "or use --require-vid/--require-did)",
                              ident.Description);
  }
  return 0;
}

static inline int CreateD3D9ExDevice(const char* test_name,
                                     HWND hwnd,
                                     ComPtr<IDirect3D9Ex>* out_d3d,
                                     ComPtr<IDirect3DDevice9Ex>* out_dev,
                                     aerogpu_test::TestReporter* reporter) {
  if (!out_d3d || !out_dev) {
    if (reporter) {
      return reporter->Fail("internal: CreateD3D9ExDevice out params are NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: CreateD3D9ExDevice out params are NULL");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("Direct3DCreate9Ex", hr);
    }
    return aerogpu_test::FailHresult(test_name, "Direct3DCreate9Ex", hr);
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = kWidth;
  pp.BackBufferHeight = kHeight;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = d3d->CreateDeviceEx(
      D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(
        D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  }
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3D9Ex::CreateDeviceEx", hr);
  }

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return 0;
}

static inline DWORD RemainingTimeoutMs(DWORD start_ticks, DWORD timeout_ms) {
  const DWORD now = GetTickCount();
  const DWORD elapsed = now - start_ticks;
  if (elapsed >= timeout_ms) {
    return 0;
  }
  return timeout_ms - elapsed;
}

static const uint32_t kIpcMagic = 0x36575741u;  // 'AWW6' (arbitrary non-zero marker).
static const uint32_t kIpcVersion = 1;

struct Wow64Ipc {
  uint32_t magic;
  uint32_t version;
  uint64_t producer_handle_value;
  uint64_t shared_handle_value;  // HANDLE value in the consumer process.
  volatile LONG ready;
  volatile LONG done;
  volatile LONG consumer_exit_code;
  uint32_t reserved;
};

typedef char Wow64IpcSizeCheck[(sizeof(Wow64Ipc) == 40) ? 1 : -1];

}  // namespace d3d9ex_shared_surface_wow64
