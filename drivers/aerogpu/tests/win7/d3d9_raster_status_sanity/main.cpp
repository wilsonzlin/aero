#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

#include <set>

using aerogpu_test::ComPtr;

static int RunD3D9RasterStatusSanity(int argc, char** argv) {
  const char* kTestName = "d3d9_raster_status_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--allow-remote]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return reporter.Fail("invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return reporter.Fail("invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  uint32_t min_samples = 2000;
  aerogpu_test::GetArgUint32(argc, argv, "--samples", &min_samples);

  // Some remote display paths do not deliver vblank semantics in a meaningful way.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail(
        "running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9RasterStatusSanity",
                                              L"AeroGPU D3D9 Raster Status Sanity",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("Direct3DCreate9Ex", hr);
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
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                           D3DDEVTYPE_HAL,
                           hwnd,
                           create_flags,
                           &pp,
                           NULL,
                           dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             &pp,
                             NULL,
                             dev.put());
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               kTestName,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    reporter.SetAdapterInfoA(ident.Description, ident.VendorId, ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return reporter.Fail(
          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
          (unsigned)ident.VendorId,
          (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return reporter.Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.VendorId,
                           (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return reporter.Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.DeviceId,
                           (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return reporter.Fail(
          "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu or use --require-vid/--require-did)",
          ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return reporter.FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  LARGE_INTEGER qpc_freq;
  QueryPerformanceFrequency(&qpc_freq);
  LARGE_INTEGER qpc_start;
  QueryPerformanceCounter(&qpc_start);

  const uint32_t kMinDistinctScanlines = 16;
  const double kMaxDurationMs = 1000.0;

  uint32_t in_vblank_samples = 0;
  uint32_t out_vblank_samples = 0;
  UINT min_scan = 0xFFFFFFFFu;
  UINT max_scan = 0;
  std::set<UINT> distinct_scanlines_not_vblank;

  uint32_t iterations = 0;
  double elapsed_ms = 0.0;

  for (;;) {
    D3DRASTER_STATUS rs;
    ZeroMemory(&rs, sizeof(rs));
    hr = dev->GetRasterStatus(0, &rs);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::GetRasterStatus", hr);
    }
    iterations++;

    if (rs.InVBlank) {
      in_vblank_samples++;
    } else {
      out_vblank_samples++;
      distinct_scanlines_not_vblank.insert(rs.ScanLine);
    }

    if (rs.ScanLine < min_scan) {
      min_scan = rs.ScanLine;
    }
    if (rs.ScanLine > max_scan) {
      max_scan = rs.ScanLine;
    }

    if (qpc_freq.QuadPart != 0) {
      LARGE_INTEGER qpc_now;
      QueryPerformanceCounter(&qpc_now);
      const LONGLONG dt = qpc_now.QuadPart - qpc_start.QuadPart;
      elapsed_ms = (double)dt * 1000.0 / (double)qpc_freq.QuadPart;
    }

    const bool criteria_met =
        (in_vblank_samples > 0) && (out_vblank_samples > 0) &&
        (distinct_scanlines_not_vblank.size() >= (size_t)kMinDistinctScanlines);

    if (elapsed_ms >= kMaxDurationMs) {
      break;
    }
    if (iterations >= min_samples && criteria_met) {
      break;
    }

    if ((iterations & 0xFFu) == 0) {
      SwitchToThread();
    }
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: elapsed_ms=%.1f samples=%u in_vblank=%u out_vblank=%u scan_range=[%u,%u] "
      "distinct_scanlines_not_vblank=%u",
      kTestName,
      elapsed_ms,
      (unsigned)iterations,
      (unsigned)in_vblank_samples,
      (unsigned)out_vblank_samples,
      (unsigned)min_scan,
      (unsigned)max_scan,
      (unsigned)distinct_scanlines_not_vblank.size());

  if (in_vblank_samples == 0) {
    return reporter.Fail("InVBlank was never true (scanline/vblank stuck?)");
  }
  if (out_vblank_samples == 0) {
    return reporter.Fail("InVBlank was never false (scanline/vblank stuck?)");
  }
  if (distinct_scanlines_not_vblank.size() < (size_t)kMinDistinctScanlines) {
    return reporter.Fail(
        "distinct ScanLine values (not in vblank) was %u (expected >= %u; ScanLine stuck?)",
        (unsigned)distinct_scanlines_not_vblank.size(),
        (unsigned)kMinDistinctScanlines);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9RasterStatusSanity(argc, argv);
}
