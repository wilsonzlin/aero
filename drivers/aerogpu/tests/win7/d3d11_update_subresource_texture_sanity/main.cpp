#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static int FailD3D11WithRemovedReason(const char* test_name,
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
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static void FillBGRA8Pattern(uint8_t* dst, int width, int height, int row_pitch) {
  for (int y = 0; y < height; ++y) {
    uint8_t* row = dst + y * row_pitch;
    for (int x = 0; x < width; ++x) {
      uint8_t* p = row + x * 4;
      // BGRA8. Keep A at 0xFF to make it obvious if alpha gets clobbered.
      p[0] = (uint8_t)(x & 0xFF);            // B
      p[1] = (uint8_t)(y & 0xFF);            // G
      p[2] = (uint8_t)((x ^ y) & 0xFF);      // R
      p[3] = (uint8_t)(0xFFu);               // A
    }
  }
}

static int RunD3D11UpdateSubresourceTextureSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_update_subresource_texture_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

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
    return aerogpu_test::FailHresult(kTestName, "D3D11CreateDevice(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return aerogpu_test::FailHresult(kTestName,
                                         "IDXGIDevice::GetAdapter (required for --require-vid/--require-did)",
                                         hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return aerogpu_test::FailHresult(
              kTestName, "IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
        }
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                                   kTestName,
                                   ad.Description,
                                   (unsigned)ad.VendorId,
                                   (unsigned)ad.DeviceId);
        if (!allow_microsoft && ad.VendorId == 0x1414) {
          return aerogpu_test::Fail(kTestName,
                                    "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                    "Install AeroGPU driver or pass --allow-microsoft.",
                                    (unsigned)ad.VendorId,
                                    (unsigned)ad.DeviceId);
        }
        if (has_require_vid && ad.VendorId != require_vid) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.VendorId,
                                    (unsigned)require_vid);
        }
        if (has_require_did && ad.DeviceId != require_did) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.DeviceId,
                                    (unsigned)require_did);
        }
        if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
            !(ad.VendorId == 0x1414 && allow_microsoft) &&
            !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu "
                                    "or use --require-vid/--require-did)",
                                    ad.Description);
        }
      }
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(
        kTestName, "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  const int kWidth = 64;
  const int kHeight = 64;

  D3D11_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.SampleDesc.Quality = 0;
  desc.Usage = D3D11_USAGE_DEFAULT;
  desc.BindFlags = 0;
  desc.CPUAccessFlags = 0;
  desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> tex;
  hr = device->CreateTexture2D(&desc, NULL, tex.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(DEFAULT)", hr);
  }

  // Use a padded row pitch (not tightly packed) to catch bugs where the driver incorrectly assumes
  // RowPitch == Width*BytesPerPixel for UpdateSubresource uploads.
  const int upload_row_pitch = kWidth * 4 + 16;
  std::vector<uint8_t> upload((size_t)upload_row_pitch * (size_t)kHeight, 0);
  FillBGRA8Pattern(&upload[0], kWidth, kHeight, upload_row_pitch);

  // Exercises pfnUpdateSubresourceUP on Win7.
  context->UpdateSubresource(tex.get(), 0, NULL, &upload[0], upload_row_pitch, 0);

  D3D11_TEXTURE2D_DESC st_desc = desc;
  st_desc.Usage = D3D11_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  st_desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(STAGING)", hr);
  }

  context->CopyResource(staging.get(), tex.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging, READ)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(staging.get(), 0);
    return aerogpu_test::Fail(kTestName, "Map(staging, READ) returned NULL pData");
  }
  const int kTightRowPitch = kWidth * 4;
  if (map.RowPitch < (UINT)kTightRowPitch) {
    context->Unmap(staging.get(), 0);
    return aerogpu_test::Fail(kTestName,
                              "unexpected RowPitch: got %lu expected >= %d",
                              (unsigned long)map.RowPitch,
                              kTightRowPitch);
  }

  if (dump) {
    const std::wstring dir = aerogpu_test::GetModuleDir();
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_update_subresource_texture_sanity.bmp"),
                                      kWidth,
                                      kHeight,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    }
  }

  for (int y = 0; y < kHeight; ++y) {
    const uint8_t* got_row = (const uint8_t*)map.pData + (size_t)y * (size_t)map.RowPitch;
    const uint8_t* exp_row = &upload[(size_t)y * (size_t)upload_row_pitch];
    for (int x = 0; x < kWidth; ++x) {
      const uint8_t* got_px = got_row + x * 4;
      const uint8_t* exp_px = exp_row + x * 4;
      if (got_px[0] != exp_px[0] || got_px[1] != exp_px[1] || got_px[2] != exp_px[2] ||
          got_px[3] != exp_px[3]) {
        const uint32_t got = aerogpu_test::ReadPixelBGRA(got_row, (int)map.RowPitch, x, 0);
        const uint32_t exp = aerogpu_test::ReadPixelBGRA(exp_row, upload_row_pitch, x, 0);
        context->Unmap(staging.get(), 0);
        return aerogpu_test::Fail(
            kTestName,
            "pixel mismatch at (%d,%d): got BGRA=0x%08lX expected BGRA=0x%08lX",
            x,
            y,
            (unsigned long)got,
            (unsigned long)exp);
      }
    }
  }

  context->Unmap(staging.get(), 0);

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11UpdateSubresourceTextureSanity(argc, argv);
}
