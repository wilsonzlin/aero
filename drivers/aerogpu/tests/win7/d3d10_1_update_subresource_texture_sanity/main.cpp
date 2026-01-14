#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d10_1.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

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

static void DumpBytesToFile(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT byte_count) {
  if (!file_name || !data || byte_count == 0) {
    return;
  }
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring path = aerogpu_test::JoinPath(dir, file_name);
  HANDLE h =
      CreateFileW(path.c_str(), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    aerogpu_test::PrintfStdout("INFO: %s: dump CreateFileW(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    return;
  }
  DWORD written = 0;
  if (!WriteFile(h, data, byte_count, &written, NULL) || written != byte_count) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name,
                               (unsigned)byte_count,
                               path.c_str());
    if (reporter) {
      reporter->AddArtifactPathW(path);
    }
  }
  CloseHandle(h);
}

static void DumpTightBgra32(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT row_pitch,
                            int width,
                            int height) {
  if (!data || width <= 0 || height <= 0 || row_pitch < (UINT)(width * 4)) {
    return;
  }
  std::vector<uint8_t> tight((size_t)width * (size_t)height * 4u, 0);
  for (int y = 0; y < height; ++y) {
    const uint8_t* src_row = (const uint8_t*)data + (size_t)y * (size_t)row_pitch;
    memcpy(&tight[(size_t)y * (size_t)width * 4u], src_row, (size_t)width * 4u);
  }
  DumpBytesToFile(test_name, reporter, file_name, &tight[0], (UINT)tight.size());
}

static uint32_t PackBGRA(uint8_t b, uint8_t g, uint8_t r, uint8_t a) {
  return ((uint32_t)b) | ((uint32_t)g << 8) | ((uint32_t)r << 16) | ((uint32_t)a << 24);
}

static uint32_t ExpectedBasePixel(int x, int y) {
  const uint8_t b = (uint8_t)(x & 0xFF);
  const uint8_t g = (uint8_t)(y & 0xFF);
  const uint8_t r = (uint8_t)((x ^ y) & 0xFF);
  const uint8_t a = 0xFFu;
  return PackBGRA(b, g, r, a);
}

static uint32_t ExpectedPatchPixel(int x, int y) {
  const uint8_t b = (uint8_t)((x * 3 + 17) & 0xFF);
  const uint8_t g = (uint8_t)((y * 5 + 101) & 0xFF);
  const uint8_t r = (uint8_t)((x + y + 11) & 0xFF);
  const uint8_t a = 0xFFu;
  return PackBGRA(b, g, r, a);
}

static void FillUploadBGRA8(uint8_t* dst,
                            int width,
                            int height,
                            int row_pitch,
                            int x_offset,
                            int y_offset,
                            bool patch_pattern) {
  for (int y = 0; y < height; ++y) {
    uint8_t* row = dst + y * row_pitch;
    for (int x = 0; x < width; ++x) {
      const int gx = x + x_offset;
      const int gy = y + y_offset;
      const uint32_t v = patch_pattern ? ExpectedPatchPixel(gx, gy) : ExpectedBasePixel(gx, gy);
      uint8_t* p = row + x * 4;
      p[0] = (uint8_t)((v >> 0) & 0xFF);
      p[1] = (uint8_t)((v >> 8) & 0xFF);
      p[2] = (uint8_t)((v >> 16) & 0xFF);
      p[3] = (uint8_t)((v >> 24) & 0xFF);
    }
  }
}

static int RunD3D10_1UpdateSubresourceTextureSanity(int argc, char** argv) {
  const char* kTestName = "d3d10_1_update_subresource_texture_sanity";
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
                            D3D10_1_SDK_VERSION,
                            device.put());
    if (SUCCEEDED(hr)) {
      break;
    }
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D10CreateDevice1(HARDWARE)", hr);
  }

  // This test is specifically intended to exercise the D3D10.1 runtime path (d3d10_1.dll), which
  // should in turn use the UMD's OpenAdapter10_2 entrypoint.
  if (!GetModuleHandleW(L"d3d10_1.dll")) {
    return reporter.Fail("d3d10_1.dll is not loaded");
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult(
            "IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return reporter.FailHresult(
              "IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
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

  const int kWidth = 64;
  const int kHeight = 64;

  D3D10_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.SampleDesc.Quality = 0;
  desc.Usage = D3D10_USAGE_DEFAULT;
  desc.BindFlags = 0;
  desc.CPUAccessFlags = 0;
  desc.MiscFlags = 0;

  ComPtr<ID3D10Texture2D> tex;
  hr = device->CreateTexture2D(&desc, NULL, tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(DEFAULT)", hr);
  }

  const int upload_row_pitch = kWidth * 4 + 4;
  std::vector<uint8_t> upload((size_t)upload_row_pitch * (size_t)kHeight, 0);
  FillUploadBGRA8(&upload[0], kWidth, kHeight, upload_row_pitch, 0, 0, false);

  device->UpdateSubresource(tex.get(), 0, NULL, &upload[0], (UINT)upload_row_pitch, 0);

  const int kPatchLeft = 7;
  const int kPatchTop = 9;
  const int kPatchWidth = 17;
  const int kPatchHeight = 13;
  const int kPatchRight = kPatchLeft + kPatchWidth;
  const int kPatchBottom = kPatchTop + kPatchHeight;
  if (kPatchRight > kWidth || kPatchBottom > kHeight) {
    return reporter.Fail("internal error: patch box out of bounds");
  }

  D3D10_BOX patch_box;
  patch_box.left = (UINT)kPatchLeft;
  patch_box.top = (UINT)kPatchTop;
  patch_box.front = 0;
  patch_box.right = (UINT)kPatchRight;
  patch_box.bottom = (UINT)kPatchBottom;
  patch_box.back = 1;

  const int patch_row_pitch = kPatchWidth * 4 + 8;
  std::vector<uint8_t> patch((size_t)patch_row_pitch * (size_t)kPatchHeight, 0);
  FillUploadBGRA8(&patch[0], kPatchWidth, kPatchHeight, patch_row_pitch, kPatchLeft, kPatchTop, true);

  device->UpdateSubresource(tex.get(), 0, &patch_box, &patch[0], (UINT)patch_row_pitch, 0);

  D3D10_TEXTURE2D_DESC st_desc = desc;
  st_desc.Usage = D3D10_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;
  st_desc.MiscFlags = 0;

  ComPtr<ID3D10Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(STAGING)", hr);
  }

  device->CopyResource(staging.get(), tex.get());
  device->Flush();

  D3D10_MAPPED_TEXTURE2D map;
  ZeroMemory(&map, sizeof(map));
  hr = staging->Map(0, D3D10_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(staging, READ)", hr, device.get());
  }
  if (!map.pData) {
    staging->Unmap(0);
    return reporter.Fail("Map(staging, READ) returned NULL pData");
  }
  const int kTightRowPitch = kWidth * 4;
  if (map.RowPitch < (UINT)kTightRowPitch) {
    staging->Unmap(0);
    return reporter.Fail("unexpected RowPitch: got %lu expected >= %d",
                         (unsigned long)map.RowPitch,
                         kTightRowPitch);
  }

  if (dump) {
    const std::wstring dir = aerogpu_test::GetModuleDir();
    const std::wstring bmp_path =
        aerogpu_test::JoinPath(dir, L"d3d10_1_update_subresource_texture_sanity.bmp");
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(
            bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
    DumpTightBgra32(kTestName,
                    &reporter,
                    L"d3d10_1_update_subresource_texture_sanity.bin",
                    map.pData,
                    map.RowPitch,
                    kWidth,
                    kHeight);
  }

  for (int y = 0; y < kHeight; ++y) {
    for (int x = 0; x < kWidth; ++x) {
      const bool in_patch =
          (x >= kPatchLeft && x < kPatchRight && y >= kPatchTop && y < kPatchBottom);
      const uint32_t exp = in_patch ? ExpectedPatchPixel(x, y) : ExpectedBasePixel(x, y);
      const uint32_t got = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, x, y);
      if (got != exp) {
        staging->Unmap(0);
        PrintDeviceRemovedReasonIfAny(kTestName, device.get());
        return reporter.Fail("pixel mismatch at (%d,%d) [%s]: got BGRA=0x%08lX expected BGRA=0x%08lX",
                             x,
                             y,
                             in_patch ? "box update region" : "base region",
                             (unsigned long)got,
                             (unsigned long)exp);
      }
    }
  }

  staging->Unmap(0);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D10_1UpdateSubresourceTextureSanity(argc, argv);
}

