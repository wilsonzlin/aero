#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d10_1.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

#ifndef DXGI_ERROR_WAS_STILL_DRAWING
  #define DXGI_ERROR_WAS_STILL_DRAWING ((HRESULT)0x887A000AL)
#endif

static int FailD3D10WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                      const char* test_name,
                                      const char* what,
                                      HRESULT hr,
                                      ID3D10Device* device) {
  if (device) {
    HRESULT reason = device->GetDeviceRemovedReason();
    if (FAILED(reason)) {
      aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                                 test_name,
                                 aerogpu_test::HresultToString(reason).c_str());
    }
  }
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static void PrintDeviceRemovedReasonIfAny(const char* test_name, ID3D10Device* device) {
  if (!device) {
    return;
  }
  HRESULT reason = device->GetDeviceRemovedReason();
  if (reason != S_OK) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: device removed reason: %s", test_name, aerogpu_test::HresultToString(reason).c_str());
  }
}

struct MapThreadArgs {
  ID3D10Texture2D* tex;
  UINT map_flags;
  HRESULT hr;
  UINT row_pitch;
  uint32_t pixel;
  bool has_pixel;
};

static DWORD WINAPI MapThreadProc(LPVOID param) {
  MapThreadArgs* args = (MapThreadArgs*)param;
  args->hr = E_FAIL;
  args->row_pitch = 0;
  args->pixel = 0;
  args->has_pixel = false;

  D3D10_MAPPED_TEXTURE2D mapped;
  ZeroMemory(&mapped, sizeof(mapped));
  args->hr = args->tex->Map(0, D3D10_MAP_READ, args->map_flags, &mapped);
  if (SUCCEEDED(args->hr) && mapped.pData) {
    args->row_pitch = mapped.RowPitch;
    args->pixel = aerogpu_test::ReadPixelBGRA(mapped.pData, (int)mapped.RowPitch, 0, 0);
    args->has_pixel = true;
    args->tex->Unmap(0);
  }

  args->tex->Release();
  args->tex = NULL;
  return 0;
}

static int RunMapDoNotWait(int argc, char** argv) {
  const char* kTestName = "d3d10_1_map_do_not_wait";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
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

  ComPtr<ID3D10Device1> device;
  const UINT flags = D3D10_CREATE_DEVICE_BGRA_SUPPORT;
  D3D10_FEATURE_LEVEL1 feature_levels[] = {D3D10_FEATURE_LEVEL1_10_1, D3D10_FEATURE_LEVEL1_10_0};
  D3D10_FEATURE_LEVEL1 chosen_level = (D3D10_FEATURE_LEVEL1)0;
  HRESULT hr = E_FAIL;
  for (size_t i = 0; i < ARRAYSIZE(feature_levels); ++i) {
    chosen_level = feature_levels[i];
    hr = D3D10CreateDevice1(NULL,
                            D3D10_DRIVER_TYPE_HARDWARE,
                            NULL,
                            flags,
                            chosen_level,
                            D3D10_SDK_VERSION,
                            device.put());
    if (SUCCEEDED(hr)) {
      break;
    }
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D10CreateDevice1(HARDWARE)", hr);
  }

  // This test is specifically intended to exercise the D3D10.1 runtime path (d3d10_1.dll).
  if (!GetModuleHandleW(L"d3d10_1.dll")) {
    return reporter.Fail("d3d10_1.dll is not loaded");
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  const D3D10_FEATURE_LEVEL1 actual_level = device->GetFeatureLevel();
  if (actual_level != chosen_level) {
    return reporter.Fail("ID3D10Device1::GetFeatureLevel returned 0x%04X (expected 0x%04X)",
                         (unsigned)actual_level,
                         (unsigned)chosen_level);
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return reporter.FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
        }
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                                   kTestName,
                                   ad.Description,
                                   (unsigned)ad.VendorId,
                                   (unsigned)ad.DeviceId);
        reporter.SetAdapterInfoW(ad.Description, ad.VendorId, ad.DeviceId);
        if (!allow_microsoft && ad.VendorId == 0x1414) {
          return reporter.Fail(
              "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
              (unsigned)ad.VendorId,
              (unsigned)ad.DeviceId);
        }
        if (has_require_vid && ad.VendorId != require_vid) {
          return reporter.Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                               (unsigned)ad.VendorId,
                               (unsigned)require_vid);
        }
        if (has_require_did && ad.DeviceId != require_did) {
          return reporter.Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                               (unsigned)ad.DeviceId,
                               (unsigned)require_did);
        }
        if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
            !(ad.VendorId == 0x1414 && allow_microsoft) &&
            !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
          return reporter.Fail(
              "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu or use --require-vid/--require-did)",
              ad.Description);
        }
      }
    }
  } else if (has_require_vid || has_require_did) {
    return reporter.FailHresult("QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }

    // This test is explicitly intended to cover the D3D10.1 UMD entrypoint path (`OpenAdapter10_2`).
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_10_2 = GetProcAddress(umd, "OpenAdapter10_2");
    if (!open_adapter_10_2) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_10_2 = GetProcAddress(umd, "_OpenAdapter10_2@4");
    }
    if (!open_adapter_10_2) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter10_2 (D3D10.1 entrypoint)");
    }
  }

  const int kWidth = 1024;
  const int kHeight = 1024;

  D3D10_TEXTURE2D_DESC rt_desc;
  ZeroMemory(&rt_desc, sizeof(rt_desc));
  rt_desc.Width = kWidth;
  rt_desc.Height = kHeight;
  rt_desc.MipLevels = 1;
  rt_desc.ArraySize = 1;
  rt_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  rt_desc.SampleDesc.Count = 1;
  rt_desc.SampleDesc.Quality = 0;
  rt_desc.Usage = D3D10_USAGE_DEFAULT;
  rt_desc.BindFlags = D3D10_BIND_RENDER_TARGET;
  rt_desc.CPUAccessFlags = 0;
  rt_desc.MiscFlags = 0;

  ComPtr<ID3D10Texture2D> rt_tex;
  hr = device->CreateTexture2D(&rt_desc, NULL, rt_tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(render target)", hr);
  }

  ComPtr<ID3D10RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(rt_tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView", hr);
  }

  ID3D10RenderTargetView* rtvs[] = {rtv.get()};
  device->OMSetRenderTargets(1, rtvs, NULL);

  const FLOAT clear_rgba[4] = {0.0f, 1.0f, 0.0f, 1.0f};
  device->ClearRenderTargetView(rtv.get(), clear_rgba);
  device->OMSetRenderTargets(0, NULL, NULL);

  D3D10_TEXTURE2D_DESC st_desc = rt_desc;
  st_desc.Usage = D3D10_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;

  ComPtr<ID3D10Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  // Issue enough work to make it likely that the GPU hasn't completed by the time we try to map.
  const int kCopyCount = 64;
  for (int i = 0; i < kCopyCount; ++i) {
    device->CopyResource(staging.get(), rt_tex.get());
  }
  device->Flush();

  // Map with DO_NOT_WAIT should never block. On typical async drivers it should
  // return DXGI_ERROR_WAS_STILL_DRAWING; if it succeeds immediately that's fine,
  // but it still must return promptly.
  {
    MapThreadArgs args;
    ZeroMemory(&args, sizeof(args));
    args.tex = staging.get();
    args.map_flags = D3D10_MAP_FLAG_DO_NOT_WAIT;
    args.hr = E_FAIL;
    staging->AddRef();

    HANDLE thread = CreateThread(NULL, 0, &MapThreadProc, &args, 0, NULL);
    if (!thread) {
      staging->Release();
      return reporter.Fail("CreateThread(Map DO_NOT_WAIT) failed");
    }

    const DWORD wait = WaitForSingleObject(thread, 250);
    CloseHandle(thread);
    if (wait == WAIT_TIMEOUT) {
      return reporter.Fail("Map(READ, DO_NOT_WAIT) appears to have blocked (>250ms)");
    }

    if (args.hr == DXGI_ERROR_WAS_STILL_DRAWING) {
      aerogpu_test::PrintfStdout("INFO: %s: Map(DO_NOT_WAIT) => DXGI_ERROR_WAS_STILL_DRAWING", kTestName);
    } else if (SUCCEEDED(args.hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: Map(DO_NOT_WAIT) succeeded immediately", kTestName);
      if (!args.has_pixel) {
        return reporter.Fail("Map(DO_NOT_WAIT) returned NULL pData");
      }
      const UINT min_row_pitch = (UINT)(kWidth * 4);
      if (args.row_pitch < min_row_pitch) {
        return reporter.Fail("Map(DO_NOT_WAIT) returned too-small RowPitch=%u (min=%u)",
                             (unsigned)args.row_pitch,
                             (unsigned)min_row_pitch);
      }
      const uint32_t expected = 0xFF00FF00u;  // green
      if ((args.pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
        PrintDeviceRemovedReasonIfAny(kTestName, device.get());
        return reporter.Fail("Map(DO_NOT_WAIT) pixel mismatch at (0,0): got 0x%08lX expected ~0x%08lX",
                             (unsigned long)args.pixel,
                             (unsigned long)expected);
      }
    } else {
      return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(DO_NOT_WAIT)", args.hr, device.get());
    }
  }

  // A blocking map should always succeed and yield the cleared pixels.
  {
    MapThreadArgs args;
    ZeroMemory(&args, sizeof(args));
    args.tex = staging.get();
    args.map_flags = 0;
    args.hr = E_FAIL;
    staging->AddRef();

    HANDLE thread = CreateThread(NULL, 0, &MapThreadProc, &args, 0, NULL);
    if (!thread) {
      staging->Release();
      return reporter.Fail("CreateThread(Map blocking) failed");
    }

    const DWORD wait = WaitForSingleObject(thread, 30000);
    CloseHandle(thread);
    if (wait == WAIT_TIMEOUT) {
      return reporter.Fail("Map(READ) appears to have hung (>30000ms)");
    }

    if (FAILED(args.hr)) {
      return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(READ)", args.hr, device.get());
    }
    if (!args.has_pixel) {
      return reporter.Fail("Map(READ) returned NULL pData");
    }
    const UINT min_row_pitch = (UINT)(kWidth * 4);
    if (args.row_pitch < min_row_pitch) {
      return reporter.Fail("Map(READ) returned too-small RowPitch=%u (min=%u)",
                           (unsigned)args.row_pitch,
                           (unsigned)min_row_pitch);
    }

    const uint32_t expected = 0xFF00FF00u;  // green
    if ((args.pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("pixel mismatch at (0,0): got 0x%08lX expected ~0x%08lX",
                           (unsigned long)args.pixel,
                           (unsigned long)expected);
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunMapDoNotWait(argc, argv);
}
