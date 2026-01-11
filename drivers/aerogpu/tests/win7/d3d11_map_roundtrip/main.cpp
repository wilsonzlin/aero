#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static int FailD3D11WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                      const char* test_name,
                                      const char* what,
                                      HRESULT hr,
                                      ID3D11Device* device) {
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

static void WritePixelBGRA(void* data, int row_pitch, int x, int y, uint32_t bgra) {
  uint8_t* base = (uint8_t*)data;
  uint8_t* p = base + y * row_pitch + x * 4;
  p[0] = (uint8_t)(bgra & 0xFF);
  p[1] = (uint8_t)((bgra >> 8) & 0xFF);
  p[2] = (uint8_t)((bgra >> 16) & 0xFF);
  p[3] = (uint8_t)((bgra >> 24) & 0xFF);
}

static uint32_t CheckerColor(int x, int y) {
  const int tile = 4;
  const int v = ((x / tile) ^ (y / tile)) & 1;
  return v ? 0xFF00FF00u : 0xFFFF0000u;  // green / red (0xAARRGGBB)
}

static int RunMapRoundtrip(int argc, char** argv) {
  const char* kTestName = "d3d11_map_roundtrip";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
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

  D3D_FEATURE_LEVEL feature_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                       D3D_FEATURE_LEVEL_10_1,
                                       D3D_FEATURE_LEVEL_10_0,
                                       D3D_FEATURE_LEVEL_9_3,
                                       D3D_FEATURE_LEVEL_9_2,
                                       D3D_FEATURE_LEVEL_9_1};

  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;

  HRESULT hr = D3D11CreateDevice(NULL,
                                 D3D_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 feature_levels,
                                 ARRAYSIZE(feature_levels),
                                 D3D11_SDK_VERSION,
                                 device.put(),
                                 &chosen_level,
                                 context.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D11CreateDevice(HARDWARE)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

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

    if (!GetModuleHandleW(L"d3d11.dll")) {
      return reporter.Fail("d3d11.dll is not loaded");
    }
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_11 = GetProcAddress(umd, "OpenAdapter11");
    if (!open_adapter_11) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_11 = GetProcAddress(umd, "_OpenAdapter11@4");
    }
    if (!open_adapter_11) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter11 (D3D11 entrypoint)");
    }
  }

  const int kWidth = 37;
  const int kHeight = 23;

  D3D11_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.SampleDesc.Quality = 0;
  desc.Usage = D3D11_USAGE_STAGING;
  desc.BindFlags = 0;
  desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE;
  desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> tex;
  hr = device->CreateTexture2D(&desc, NULL, tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(tex.get(), 0, D3D11_MAP_WRITE, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(WRITE)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(tex.get(), 0);
    return reporter.Fail("Map(WRITE) returned NULL pData");
  }
  if (map.RowPitch < (UINT)(kWidth * 4)) {
    context->Unmap(tex.get(), 0);
    return reporter.Fail("Map(WRITE) returned RowPitch=%u (< %u)",
                         (unsigned)map.RowPitch,
                         (unsigned)(kWidth * 4));
  }

  for (int y = 0; y < kHeight; ++y) {
    for (int x = 0; x < kWidth; ++x) {
      WritePixelBGRA(map.pData, (int)map.RowPitch, x, y, CheckerColor(x, y));
    }
  }
  context->Unmap(tex.get(), 0);

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(tex.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(READ)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(tex.get(), 0);
    return reporter.Fail("Map(READ) returned NULL pData");
  }

  for (int y = 0; y < kHeight; ++y) {
    for (int x = 0; x < kWidth; ++x) {
      const uint32_t got = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, x, y);
      const uint32_t expected = CheckerColor(x, y);
      if ((got & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
        context->Unmap(tex.get(), 0);
        return reporter.Fail("pixel mismatch at (%d,%d): got 0x%08lX expected 0x%08lX",
                             x,
                             y,
                             (unsigned long)got,
                             (unsigned long)expected);
      }
    }
  }

  if (dump) {
    const std::wstring dir = aerogpu_test::GetModuleDir();
    std::string err;
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_map_roundtrip.bmp");
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path,
                                      kWidth,
                                      kHeight,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
  }

  context->Unmap(tex.get(), 0);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunMapRoundtrip(argc, argv);
}
