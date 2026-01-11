#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static int RunD3D9RasterStatusPacing(int argc, char** argv) {
  const char* kTestName = "d3d9_raster_status_pacing";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--allow-remote]",
        kTestName);
    return 0;
  }

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
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  uint32_t max_samples = 200000;
  aerogpu_test::GetArgUint32(argc, argv, "--samples", &max_samples);
  // `run_all.cmd` forwards `--samples` to multiple tests, many of which default to ~120 samples.
  // `GetRasterStatus` can be very fast, so enforce a larger minimum to make it likely we observe
  // scanline progression + vblank transitions even on fast hosts.
  if (max_samples < 50000) {
    max_samples = 50000;
  }

  const int kWidth = 64;
  const int kHeight = 64;

  // Some remote display paths do not deliver vblank semantics in a meaningful way.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      aerogpu_test::PrintfStdout("PASS: %s", kTestName);
      return 0;
    }
    return aerogpu_test::Fail(
        kTestName,
        "running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9RasterStatusPacing",
                                              L"AeroGPU D3D9 Raster Status Pacing",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Direct3DCreate9Ex", hr);
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
    return aerogpu_test::FailHresult(kTestName, "IDirect3D9Ex::CreateDeviceEx", hr);
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
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return aerogpu_test::Fail(kTestName,
                                "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                "Install AeroGPU driver or pass --allow-microsoft.",
                                (unsigned)ident.VendorId,
                                (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return aerogpu_test::Fail(kTestName,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return aerogpu_test::Fail(kTestName,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return aerogpu_test::Fail(kTestName,
                                "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                                "or use --require-vid/--require-did)",
                                ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(kTestName,
                                     "GetAdapterIdentifier (required for --require-vid/--require-did)",
                                     hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  LARGE_INTEGER qpc_freq;
  QueryPerformanceFrequency(&qpc_freq);

  uint32_t in_vblank_samples = 0;
  uint32_t scanline_changes = 0;
  uint32_t wraps = 0;
  UINT min_scan = 0xFFFFFFFFu;
  UINT max_scan = 0;

  bool prev_in_vblank = false;
  UINT prev_scan = 0;
  std::vector<LONGLONG> vblank_edges_qpc;
  const size_t kTargetEdges = 8;
  uint32_t iterations = 0;

  for (uint32_t i = 0; i < max_samples; ++i) {
    iterations = i + 1;
    D3DRASTER_STATUS rs;
    ZeroMemory(&rs, sizeof(rs));
    hr = dev->GetRasterStatus(0, &rs);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetRasterStatus", hr);
    }

    if (rs.InVBlank) {
      in_vblank_samples++;
    }

    if (i != 0) {
      if (rs.ScanLine != prev_scan) {
        scanline_changes++;
      }
      if (rs.ScanLine < prev_scan) {
        wraps++;
      }
      if (!prev_in_vblank && rs.InVBlank) {
        LARGE_INTEGER t;
        QueryPerformanceCounter(&t);
        vblank_edges_qpc.push_back(t.QuadPart);
      }
    }

    if (rs.ScanLine < min_scan) {
      min_scan = rs.ScanLine;
    }
    if (rs.ScanLine > max_scan) {
      max_scan = rs.ScanLine;
    }

    prev_scan = rs.ScanLine;
    prev_in_vblank = rs.InVBlank ? true : false;

    if ((i & 0x3FFu) == 0) {
      SwitchToThread();
    }

    if (vblank_edges_qpc.size() >= kTargetEdges && wraps > 0 && scanline_changes > 0) {
      break;
    }
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: samples=%u in_vblank_samples=%u scanline_changes=%u wraps=%u scan_range=[%u,%u] "
      "vblank_edges=%u",
      kTestName,
      (unsigned)iterations,
      (unsigned)in_vblank_samples,
      (unsigned)scanline_changes,
      (unsigned)wraps,
      (unsigned)min_scan,
      (unsigned)max_scan,
      (unsigned)vblank_edges_qpc.size());

  if (scanline_changes == 0) {
    return aerogpu_test::Fail(kTestName, "ScanLine did not change (stuck?)");
  }
  if (wraps == 0) {
    return aerogpu_test::Fail(kTestName, "ScanLine never wrapped/reset (stuck?)");
  }
  if (in_vblank_samples < 3) {
    return aerogpu_test::Fail(kTestName, "InVBlank was true only %u time(s) (expected >= 3)", (unsigned)in_vblank_samples);
  }

  if (vblank_edges_qpc.size() >= 2 && qpc_freq.QuadPart != 0) {
    double sum_ms = 0.0;
    double min_ms = 1e30;
    double max_ms = 0.0;
    size_t intervals = 0;
    for (size_t i = 1; i < vblank_edges_qpc.size(); ++i) {
      const LONGLONG dt = vblank_edges_qpc[i] - vblank_edges_qpc[i - 1];
      const double ms = (double)dt * 1000.0 / (double)qpc_freq.QuadPart;
      sum_ms += ms;
      if (ms < min_ms) min_ms = ms;
      if (ms > max_ms) max_ms = ms;
      intervals++;
    }
    const double avg_ms = sum_ms / (double)intervals;
    const double hz = (avg_ms > 0.0) ? (1000.0 / avg_ms) : 0.0;
    aerogpu_test::PrintfStdout(
        "INFO: %s: estimated vblank interval: avg=%.3f ms min=%.3f ms max=%.3f ms (%.2f Hz) "
        "from %u interval(s)",
        kTestName,
        avg_ms,
        min_ms,
        max_ms,
        hz,
        (unsigned)intervals);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: insufficient vblank edge samples to estimate interval", kTestName);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9RasterStatusPacing(argc, argv);
}
