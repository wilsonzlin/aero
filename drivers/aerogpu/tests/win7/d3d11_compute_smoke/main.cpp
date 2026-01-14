#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static void PrintD3D11DeviceRemovedReasonIfFailed(const char* test_name, ID3D11Device* device) {
  if (!device) {
    return;
  }
  HRESULT reason = device->GetDeviceRemovedReason();
  if (FAILED(reason)) {
    aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                               test_name,
                               aerogpu_test::HresultToString(reason).c_str());
  }
}

static int FailD3D11WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                      const char* test_name,
                                      const char* what,
                                      HRESULT hr,
                                      ID3D11Device* device) {
  PrintD3D11DeviceRemovedReasonIfFailed(test_name, device);
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
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

static const UINT kNumElements = 64;

struct ConstantBufferData {
  UINT factor;
  UINT add;
  UINT pad0;
  UINT pad1;
};

static const char kComputeStructuredHlsl[] = R"(
cbuffer Cb0 : register(b0) {
  uint factor;
  uint add;
  uint pad0;
  uint pad1;
};

StructuredBuffer<uint> in_buf : register(t0);
RWStructuredBuffer<uint> out_buf : register(u0);

[numthreads(1, 1, 1)]
void cs_structured_main(uint3 tid : SV_DispatchThreadID) {
  const uint idx = tid.x;
  out_buf[idx] = in_buf[idx] * factor + add;
}
)";

static const char kComputeRawHlsl[] = R"(
cbuffer Cb0 : register(b0) {
  uint factor;
  uint add;
  uint pad0;
  uint pad1;
};

ByteAddressBuffer in_buf : register(t0);
RWByteAddressBuffer out_buf : register(u0);

[numthreads(1, 1, 1)]
void cs_raw_main(uint3 tid : SV_DispatchThreadID) {
  const uint idx = tid.x;
  const uint v = in_buf.Load(idx * 4u);
  out_buf.Store(idx * 4u, v * factor + add);
}
)";

static int RunD3D11ComputeSmoke(int argc, char** argv) {
  const char* kTestName = "d3d11_compute_smoke";
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
  if (chosen_level < D3D_FEATURE_LEVEL_10_0) {
    const std::string skip_reason = aerogpu_test::FormatString(
        "feature level 0x%04X is below D3D_FEATURE_LEVEL_10_0 (0x%04X)",
        (unsigned)chosen_level,
        (unsigned)D3D_FEATURE_LEVEL_10_0);
    reporter.SetSkipped(skip_reason.c_str());
    aerogpu_test::PrintfStdout("SKIP: %s: %s", kTestName, skip_reason.c_str());
    return reporter.Pass();
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)",
                                    hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return reporter.FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)",
                                      hr_desc);
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

  D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS hw_opts;
  ZeroMemory(&hw_opts, sizeof(hw_opts));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, &hw_opts, sizeof(hw_opts));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(D3D10_X_HARDWARE_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout(
      "INFO: %s: compute_shaders_plus_raw_and_structured_buffers_via_shader_4_x=%u",
      kTestName,
      (unsigned)hw_opts.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x);
  if (!hw_opts.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x) {
    const std::string skip_reason =
        "D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS reports ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x=FALSE";
    reporter.SetSkipped(skip_reason.c_str());
    aerogpu_test::PrintfStdout("SKIP: %s: %s", kTestName, skip_reason.c_str());
    return reporter.Pass();
  }

  std::vector<unsigned char> cs_structured_bytes;
  std::vector<unsigned char> cs_raw_bytes;
  std::string shader_err;

  std::vector<uint32_t> input(kNumElements);
  for (UINT i = 0; i < kNumElements; ++i) {
    input[i] = i * 3u + 1u;
  }

  // Constant buffer used by both compute shaders (we update it between dispatches).
  D3D11_BUFFER_DESC cb_desc;
  ZeroMemory(&cb_desc, sizeof(cb_desc));
  cb_desc.ByteWidth = sizeof(ConstantBufferData);
  cb_desc.Usage = D3D11_USAGE_DEFAULT;
  cb_desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
  cb_desc.CPUAccessFlags = 0;
  cb_desc.MiscFlags = 0;
  cb_desc.StructureByteStride = 0;

  ConstantBufferData cb_data_structured;
  cb_data_structured.factor = 3;
  cb_data_structured.add = 7;
  cb_data_structured.pad0 = 0;
  cb_data_structured.pad1 = 0;

  D3D11_SUBRESOURCE_DATA cb_init;
  ZeroMemory(&cb_init, sizeof(cb_init));
  cb_init.pSysMem = &cb_data_structured;

  ComPtr<ID3D11Buffer> cb;
  hr = device->CreateBuffer(&cb_desc, &cb_init, cb.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(constant buffer)", hr);
  }

  ID3D11Buffer* cbs[] = {cb.get()};
  context->CSSetConstantBuffers(0, 1, cbs);

  // -----------------------------
  // Structured buffer path (SRV + UAV)
  // -----------------------------
  if (!aerogpu_test::CompileHlslToBytecode(kComputeStructuredHlsl,
                                           strlen(kComputeStructuredHlsl),
                                           "d3d11_compute_smoke_structured.hlsl",
                                           "cs_structured_main",
                                           "cs_4_0",
                                           &cs_structured_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile structured compute shader: %s", shader_err.c_str());
  }
  if (dump) {
    DumpBytesToFile(kTestName,
                    &reporter,
                    L"d3d11_compute_smoke_cs_structured.dxbc",
                    cs_structured_bytes.empty() ? NULL : &cs_structured_bytes[0],
                    (UINT)cs_structured_bytes.size());
  }

  ComPtr<ID3D11ComputeShader> cs_structured;
  hr = device->CreateComputeShader(&cs_structured_bytes[0],
                                   cs_structured_bytes.size(),
                                   NULL,
                                   cs_structured.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateComputeShader(structured)", hr);
  }

  // Input structured buffer (SRV).
  D3D11_BUFFER_DESC in_desc;
  ZeroMemory(&in_desc, sizeof(in_desc));
  in_desc.ByteWidth = kNumElements * sizeof(uint32_t);
  in_desc.Usage = D3D11_USAGE_DEFAULT;
  in_desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
  in_desc.CPUAccessFlags = 0;
  in_desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
  in_desc.StructureByteStride = sizeof(uint32_t);

  D3D11_SUBRESOURCE_DATA in_init;
  ZeroMemory(&in_init, sizeof(in_init));
  in_init.pSysMem = &input[0];

  ComPtr<ID3D11Buffer> in_buf;
  hr = device->CreateBuffer(&in_desc, &in_init, in_buf.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(input SRV)", hr);
  }

  D3D11_SHADER_RESOURCE_VIEW_DESC srv_desc;
  ZeroMemory(&srv_desc, sizeof(srv_desc));
  srv_desc.Format = DXGI_FORMAT_UNKNOWN;
  srv_desc.ViewDimension = D3D11_SRV_DIMENSION_BUFFER;
  srv_desc.Buffer.FirstElement = 0;
  srv_desc.Buffer.NumElements = kNumElements;

  ComPtr<ID3D11ShaderResourceView> srv;
  hr = device->CreateShaderResourceView(in_buf.get(), &srv_desc, srv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateShaderResourceView(input SRV)", hr);
  }

  // Output structured buffer (UAV).
  D3D11_BUFFER_DESC out_desc;
  ZeroMemory(&out_desc, sizeof(out_desc));
  out_desc.ByteWidth = kNumElements * sizeof(uint32_t);
  out_desc.Usage = D3D11_USAGE_DEFAULT;
  out_desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
  out_desc.CPUAccessFlags = 0;
  out_desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
  out_desc.StructureByteStride = sizeof(uint32_t);

  ComPtr<ID3D11Buffer> out_buf;
  hr = device->CreateBuffer(&out_desc, NULL, out_buf.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(output UAV)", hr);
  }

  D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc;
  ZeroMemory(&uav_desc, sizeof(uav_desc));
  uav_desc.Format = DXGI_FORMAT_UNKNOWN;
  uav_desc.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
  uav_desc.Buffer.FirstElement = 0;
  uav_desc.Buffer.NumElements = kNumElements;
  uav_desc.Buffer.Flags = 0;

  ComPtr<ID3D11UnorderedAccessView> uav;
  hr = device->CreateUnorderedAccessView(out_buf.get(), &uav_desc, uav.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateUnorderedAccessView(output UAV)", hr);
  }

  // Dispatch compute work.
  context->CSSetShader(cs_structured.get(), NULL, 0);
  ID3D11ShaderResourceView* srvs[] = {srv.get()};
  context->CSSetShaderResources(0, 1, srvs);
  ID3D11UnorderedAccessView* uavs[] = {uav.get()};
  context->CSSetUnorderedAccessViews(0, 1, uavs, NULL);
  context->Dispatch(kNumElements, 1, 1);

  // Explicitly unbind to avoid CopyResource ambiguity on some runtimes/drivers.
  context->CSSetShader(NULL, NULL, 0);
  ID3D11ShaderResourceView* null_srvs[] = {NULL};
  context->CSSetShaderResources(0, 1, null_srvs);
  ID3D11UnorderedAccessView* null_uavs[] = {NULL};
  context->CSSetUnorderedAccessViews(0, 1, null_uavs, NULL);

  // Copy the output to a staging buffer and read it back on the CPU.
  D3D11_BUFFER_DESC st_desc;
  ZeroMemory(&st_desc, sizeof(st_desc));
  st_desc.ByteWidth = out_desc.ByteWidth;
  st_desc.Usage = D3D11_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  st_desc.MiscFlags = 0;
  st_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> staging;
  hr = device->CreateBuffer(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(staging)", hr);
  }

  context->CopyResource(staging.get(), out_buf.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(staging.get(), 0);
    return reporter.Fail("Map(staging) returned NULL pData");
  }

  const uint32_t* out_u32 = (const uint32_t*)map.pData;
  UINT mismatch_index = 0;
  uint32_t mismatch_got = 0;
  uint32_t mismatch_expected = 0;
  bool mismatch = false;
  for (UINT i = 0; i < kNumElements; ++i) {
    const uint32_t expected = input[i] * cb_data_structured.factor + cb_data_structured.add;
    const uint32_t got = out_u32[i];
    if (got != expected) {
      mismatch = true;
      mismatch_index = i;
      mismatch_got = got;
      mismatch_expected = expected;
      break;
    }
  }

  if (dump) {
    DumpBytesToFile(kTestName,
                    &reporter,
                    L"d3d11_compute_smoke_out_structured.bin",
                    map.pData,
                    out_desc.ByteWidth);
  }

  context->Unmap(staging.get(), 0);

  if (mismatch) {
    PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
    return reporter.Fail("structured output mismatch at index %u: got 0x%08lX expected 0x%08lX",
                         (unsigned)mismatch_index,
                         (unsigned long)mismatch_got,
                         (unsigned long)mismatch_expected);
  }

  // -----------------------------
  // Raw buffer path (ByteAddressBuffer + RWByteAddressBuffer)
  // -----------------------------
  ConstantBufferData cb_data_raw;
  cb_data_raw.factor = 5;
  cb_data_raw.add = 11;
  cb_data_raw.pad0 = 0;
  cb_data_raw.pad1 = 0;
  context->UpdateSubresource(cb.get(), 0, NULL, &cb_data_raw, 0, 0);
  context->CSSetConstantBuffers(0, 1, cbs);

  if (!aerogpu_test::CompileHlslToBytecode(kComputeRawHlsl,
                                           strlen(kComputeRawHlsl),
                                           "d3d11_compute_smoke_raw.hlsl",
                                           "cs_raw_main",
                                           "cs_4_0",
                                           &cs_raw_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile raw compute shader: %s", shader_err.c_str());
  }
  if (dump) {
    DumpBytesToFile(kTestName,
                    &reporter,
                    L"d3d11_compute_smoke_cs_raw.dxbc",
                    cs_raw_bytes.empty() ? NULL : &cs_raw_bytes[0],
                    (UINT)cs_raw_bytes.size());
  }

  ComPtr<ID3D11ComputeShader> cs_raw;
  hr = device->CreateComputeShader(&cs_raw_bytes[0], cs_raw_bytes.size(), NULL, cs_raw.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateComputeShader(raw)", hr);
  }

  // Input raw buffer (SRV).
  D3D11_BUFFER_DESC raw_in_desc;
  ZeroMemory(&raw_in_desc, sizeof(raw_in_desc));
  raw_in_desc.ByteWidth = kNumElements * sizeof(uint32_t);
  raw_in_desc.Usage = D3D11_USAGE_DEFAULT;
  raw_in_desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
  raw_in_desc.CPUAccessFlags = 0;
  raw_in_desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS;
  raw_in_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> raw_in_buf;
  hr = device->CreateBuffer(&raw_in_desc, &in_init, raw_in_buf.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(input raw SRV)", hr);
  }

  D3D11_SHADER_RESOURCE_VIEW_DESC raw_srv_desc;
  ZeroMemory(&raw_srv_desc, sizeof(raw_srv_desc));
  raw_srv_desc.Format = DXGI_FORMAT_R32_TYPELESS;
  raw_srv_desc.ViewDimension = D3D11_SRV_DIMENSION_BUFFEREX;
  raw_srv_desc.BufferEx.FirstElement = 0;
  raw_srv_desc.BufferEx.NumElements = kNumElements;
  raw_srv_desc.BufferEx.Flags = D3D11_BUFFEREX_SRV_FLAG_RAW;

  ComPtr<ID3D11ShaderResourceView> raw_srv;
  hr = device->CreateShaderResourceView(raw_in_buf.get(), &raw_srv_desc, raw_srv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateShaderResourceView(input raw SRV)", hr);
  }

  // Output raw buffer (UAV).
  D3D11_BUFFER_DESC raw_out_desc;
  ZeroMemory(&raw_out_desc, sizeof(raw_out_desc));
  raw_out_desc.ByteWidth = kNumElements * sizeof(uint32_t);
  raw_out_desc.Usage = D3D11_USAGE_DEFAULT;
  raw_out_desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
  raw_out_desc.CPUAccessFlags = 0;
  raw_out_desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS;
  raw_out_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> raw_out_buf;
  hr = device->CreateBuffer(&raw_out_desc, NULL, raw_out_buf.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(output raw UAV)", hr);
  }

  D3D11_UNORDERED_ACCESS_VIEW_DESC raw_uav_desc;
  ZeroMemory(&raw_uav_desc, sizeof(raw_uav_desc));
  raw_uav_desc.Format = DXGI_FORMAT_R32_TYPELESS;
  raw_uav_desc.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
  raw_uav_desc.Buffer.FirstElement = 0;
  raw_uav_desc.Buffer.NumElements = kNumElements;
  raw_uav_desc.Buffer.Flags = D3D11_BUFFER_UAV_FLAG_RAW;

  ComPtr<ID3D11UnorderedAccessView> raw_uav;
  hr = device->CreateUnorderedAccessView(raw_out_buf.get(), &raw_uav_desc, raw_uav.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateUnorderedAccessView(output raw UAV)", hr);
  }

  context->CSSetShader(cs_raw.get(), NULL, 0);
  ID3D11ShaderResourceView* raw_srvs[] = {raw_srv.get()};
  context->CSSetShaderResources(0, 1, raw_srvs);
  ID3D11UnorderedAccessView* raw_uavs[] = {raw_uav.get()};
  context->CSSetUnorderedAccessViews(0, 1, raw_uavs, NULL);
  context->Dispatch(kNumElements, 1, 1);

  context->CSSetShader(NULL, NULL, 0);
  context->CSSetShaderResources(0, 1, null_srvs);
  context->CSSetUnorderedAccessViews(0, 1, null_uavs, NULL);

  D3D11_BUFFER_DESC raw_st_desc;
  ZeroMemory(&raw_st_desc, sizeof(raw_st_desc));
  raw_st_desc.ByteWidth = raw_out_desc.ByteWidth;
  raw_st_desc.Usage = D3D11_USAGE_STAGING;
  raw_st_desc.BindFlags = 0;
  raw_st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  raw_st_desc.MiscFlags = 0;
  raw_st_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> raw_staging;
  hr = device->CreateBuffer(&raw_st_desc, NULL, raw_staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(staging raw)", hr);
  }

  context->CopyResource(raw_staging.get(), raw_out_buf.get());
  context->Flush();

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(raw_staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging raw)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(raw_staging.get(), 0);
    return reporter.Fail("Map(staging raw) returned NULL pData");
  }

  out_u32 = (const uint32_t*)map.pData;
  mismatch_index = 0;
  mismatch_got = 0;
  mismatch_expected = 0;
  mismatch = false;
  for (UINT i = 0; i < kNumElements; ++i) {
    const uint32_t expected = input[i] * cb_data_raw.factor + cb_data_raw.add;
    const uint32_t got = out_u32[i];
    if (got != expected) {
      mismatch = true;
      mismatch_index = i;
      mismatch_got = got;
      mismatch_expected = expected;
      break;
    }
  }

  if (dump) {
    DumpBytesToFile(kTestName,
                    &reporter,
                    L"d3d11_compute_smoke_out_raw.bin",
                    map.pData,
                    raw_out_desc.ByteWidth);
  }

  context->Unmap(raw_staging.get(), 0);

  if (mismatch) {
    PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
    return reporter.Fail("raw output mismatch at index %u: got 0x%08lX expected 0x%08lX (factor=%u add=%u)",
                         (unsigned)mismatch_index,
                         (unsigned long)mismatch_got,
                         (unsigned long)mismatch_expected,
                         (unsigned)cb_data_raw.factor,
                         (unsigned)cb_data_raw.add);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11ComputeSmoke(argc, argv);
}
