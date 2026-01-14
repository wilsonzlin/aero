#include <cstdint>
#include <cstdio>

#include "aerogpu_dxgi_format.h"

namespace {

using namespace aerogpu::d3d10_11;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

struct TestAdapter {
  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;
};

TestAdapter MakeAdapter(uint32_t abi_minor) {
  TestAdapter a{};
  a.umd_private_valid = true;
  a.umd_private.size_bytes = sizeof(a.umd_private);
  a.umd_private.struct_version = AEROGPU_UMDPRIV_STRUCT_VERSION_V1;
  a.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | (abi_minor & 0xFFFFu);
  a.umd_private.device_features = AEROGPU_UMDPRIV_FEATURE_TRANSFER;
  return a;
}

bool TestMappingAndCapsStable() {
  const TestAdapter abi11 = MakeAdapter(/*abi_minor=*/1);
  const TestAdapter abi12 = MakeAdapter(/*abi_minor=*/2);

  // Basic mapping sanity checks (including recently added B5 formats).
  if (!Check(DxgiFormatToAerogpu(kDxgiFormatB5G6R5Unorm) == AEROGPU_FORMAT_B5G6R5_UNORM,
             "B5G6R5 maps to AEROGPU_FORMAT_B5G6R5_UNORM")) {
    return false;
  }
  if (!Check(DxgiFormatToAerogpu(kDxgiFormatB5G5R5A1Unorm) == AEROGPU_FORMAT_B5G5R5A1_UNORM,
             "B5G5R5A1 maps to AEROGPU_FORMAT_B5G5R5A1_UNORM")) {
    return false;
  }

  // sRGB strict gating: ABI < 1.2 reports sRGB formats unsupported.
  if (!Check(AerogpuDxgiFormatCapsMask(&abi11, kDxgiFormatB8G8R8A8UnormSrgb) == kAerogpuDxgiFormatCapNone,
             "ABI 1.1: B8G8R8A8_UNORM_SRGB caps are empty")) {
    return false;
  }
  if (!Check(AerogpuDxgiFormatCapsMask(&abi12, kDxgiFormatB8G8R8A8UnormSrgb) != kAerogpuDxgiFormatCapNone,
             "ABI 1.2: B8G8R8A8_UNORM_SRGB caps are non-empty")) {
    return false;
  }

  // sRGB compat mapping: ABI < 1.2 maps sRGB DXGI formats to UNORM for the command stream.
  if (!Check(DxgiFormatToCompatDxgiFormat(&abi11, kDxgiFormatB8G8R8A8UnormSrgb) == kDxgiFormatB8G8R8A8Unorm,
             "ABI 1.1: sRGB DXGI -> UNORM DXGI compat mapping")) {
    return false;
  }
  if (!Check(DxgiFormatToAerogpuCompat(&abi11, kDxgiFormatB8G8R8A8UnormSrgb) == AEROGPU_FORMAT_B8G8R8A8_UNORM,
             "ABI 1.1: sRGB DXGI -> UNORM AeroGPU compat mapping")) {
    return false;
  }
  if (!Check(DxgiFormatToAerogpuCompat(&abi12, kDxgiFormatB8G8R8A8UnormSrgb) == AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB,
             "ABI 1.2: sRGB DXGI -> sRGB AeroGPU compat mapping")) {
    return false;
  }

  // BC strict gating: ABI < 1.2 reports BC formats unsupported.
  if (!Check(AerogpuDxgiFormatCapsMask(&abi11, kDxgiFormatBc1Unorm) == kAerogpuDxgiFormatCapNone,
             "ABI 1.1: BC1 caps are empty")) {
    return false;
  }
  const uint32_t bc1_caps = AerogpuDxgiFormatCapsMask(&abi12, kDxgiFormatBc1Unorm);
  if (!Check((bc1_caps & kAerogpuDxgiFormatCapTexture2D) != 0, "ABI 1.2: BC1 supports Texture2D")) {
    return false;
  }
  if (!Check((bc1_caps & kAerogpuDxgiFormatCapShaderSample) != 0, "ABI 1.2: BC1 supports shader sampling")) {
    return false;
  }
  if (!Check((bc1_caps & kAerogpuDxgiFormatCapRenderTarget) == 0, "ABI 1.2: BC1 is not a render target")) {
    return false;
  }

  // Compat support check should still reject BC formats on ABI 1.1.
  if (!Check(!AerogpuSupportsDxgiFormatCompat(&abi11, kDxgiFormatBc1Unorm, AerogpuFormatUsage::Texture2D),
             "ABI 1.1: compat support rejects BC1")) {
    return false;
  }
  if (!Check(AerogpuSupportsDxgiFormatCompat(&abi12, kDxgiFormatBc1Unorm, AerogpuFormatUsage::Texture2D),
             "ABI 1.2: compat support accepts BC1")) {
    return false;
  }

  // Multisample query helper is driven by the same caps policy.
  if (!Check(AerogpuSupportsMultisampleQualityLevels(&abi12, kDxgiFormatB8G8R8A8Unorm),
             "MSAA helper: B8G8R8A8_UNORM supports quality levels")) {
    return false;
  }
  if (!Check(!AerogpuSupportsMultisampleQualityLevels(&abi12, kDxgiFormatBc1Unorm),
             "MSAA helper: BC1 does not support quality levels")) {
    return false;
  }
  if (!Check(AerogpuSupportsMultisampleQualityLevels(&abi12, kDxgiFormatD32Float),
             "MSAA helper: D32_FLOAT supports quality levels")) {
    return false;
  }

  // Buffer-only formats used for raw/typed buffer views (SRV/UAV).
  if (!Check((AerogpuDxgiFormatCapsMask(&abi12, kDxgiFormatR32Typeless) & kAerogpuDxgiFormatCapBuffer) != 0,
             "R32_TYPELESS reports Buffer caps")) {
    return false;
  }
  if (!Check(!AerogpuSupportsDxgiFormat(&abi12, kDxgiFormatR32Typeless, AerogpuFormatUsage::IaIndexBuffer),
             "R32_TYPELESS is not an IA index-buffer format")) {
    return false;
  }
  if (!Check((AerogpuDxgiFormatCapsMask(&abi12, kDxgiFormatR32Float) & kAerogpuDxgiFormatCapBuffer) != 0,
             "R32_FLOAT reports Buffer caps")) {
    return false;
  }
  if (!Check((AerogpuDxgiFormatCapsMask(&abi12, kDxgiFormatR32Sint) & kAerogpuDxgiFormatCapBuffer) != 0,
             "R32_SINT reports Buffer caps")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestMappingAndCapsStable();

  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_dxgi_format_tests\n");
  return 0;
}
