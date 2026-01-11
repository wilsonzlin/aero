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

static uint8_t PatternByte(size_t i) {
  // Non-trivial deterministic pattern (avoid a constant buffer full of 0s which may accidentally pass
  // through a buggy copy path).
  return (uint8_t)((i * 131u + 7u) & 0xFFu);
}

static int RunD3D11MapDynamicBufferSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_map_dynamic_buffer_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu]",
        kTestName);
    return 0;
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
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

  const UINT kByteWidth = 4096;

  D3D11_BUFFER_DESC dyn_desc;
  ZeroMemory(&dyn_desc, sizeof(dyn_desc));
  dyn_desc.ByteWidth = kByteWidth;
  dyn_desc.Usage = D3D11_USAGE_DYNAMIC;
  dyn_desc.BindFlags = D3D11_BIND_VERTEX_BUFFER;
  dyn_desc.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE;
  dyn_desc.MiscFlags = 0;
  dyn_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> dynamic_buf;
  hr = device->CreateBuffer(&dyn_desc, NULL, dynamic_buf.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(dynamic)", hr);
  }

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(dynamic_buf.get(), 0, D3D11_MAP_WRITE_DISCARD, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(dynamic, WRITE_DISCARD)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(dynamic_buf.get(), 0);
    return aerogpu_test::Fail(kTestName, "Map(dynamic, WRITE_DISCARD) returned NULL pData");
  }

  uint8_t* dst = (uint8_t*)map.pData;
  for (size_t i = 0; i < kByteWidth; ++i) {
    dst[i] = PatternByte(i);
  }

  context->Unmap(dynamic_buf.get(), 0);

  D3D11_BUFFER_DESC st_desc;
  ZeroMemory(&st_desc, sizeof(st_desc));
  st_desc.ByteWidth = kByteWidth;
  st_desc.Usage = D3D11_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  st_desc.MiscFlags = 0;
  st_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> staging_buf;
  hr = device->CreateBuffer(&st_desc, NULL, staging_buf.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(staging)", hr);
  }

  context->CopyResource(staging_buf.get(), dynamic_buf.get());
  context->Flush();

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging_buf.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging, READ)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(staging_buf.get(), 0);
    return aerogpu_test::Fail(kTestName, "Map(staging, READ) returned NULL pData");
  }

  const uint8_t* got = (const uint8_t*)map.pData;
  for (size_t i = 0; i < kByteWidth; ++i) {
    const uint8_t expected = PatternByte(i);
    if (got[i] != expected) {
      context->Unmap(staging_buf.get(), 0);
      return aerogpu_test::Fail(kTestName,
                                "byte mismatch at offset %lu: got 0x%02X expected 0x%02X",
                                (unsigned long)i,
                                (unsigned)got[i],
                                (unsigned)expected);
    }
  }

  context->Unmap(staging_buf.get(), 0);

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11MapDynamicBufferSanity(argc, argv);
}

