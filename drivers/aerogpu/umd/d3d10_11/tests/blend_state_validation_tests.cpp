#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_blend_state_validate.h"

namespace {

using namespace aerogpu::d3d10_11;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

struct TestDevice {
  D3D10DDI_HADAPTER hAdapter = {};
  D3D10DDI_ADAPTERFUNCS adapter_funcs = {};

  D3D10DDI_HDEVICE hDevice = {};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs = {};
  std::vector<uint8_t> device_mem;
};

bool InitTestDevice(TestDevice* out) {
  if (!out) {
    return false;
  }

  D3D10DDIARG_OPENADAPTER open = {};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create = {};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
    return false;
  }

  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = nullptr;

  hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  out->hDevice = create.hDevice;
  return true;
}

D3D10DDI_HBLENDSTATE MakeBlendState(TestDevice* dev,
                                   const AEROGPU_DDIARG_CREATEBLENDSTATE& desc,
                                   std::vector<uint8_t>* out_storage) {
  D3D10DDI_HBLENDSTATE h{};
  if (!dev || !out_storage) {
    return h;
  }
  const SIZE_T size = dev->device_funcs.pfnCalcPrivateBlendStateSize(dev->hDevice, &desc);
  out_storage->assign(static_cast<size_t>(size), 0);
  h.pDrvPrivate = out_storage->data();
  return h;
}

bool TestUnsupportedBlendFactorReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kD3dBlendSrcColor; // unsupported by AeroGPU protocol
  desc.DestBlend = kD3dBlendZero;
  desc.BlendOp = kD3dBlendOpAdd;
  desc.SrcBlendAlpha = kD3dBlendOne;
  desc.DestBlendAlpha = kD3dBlendZero;
  desc.BlendOpAlpha = kD3dBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 1;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_NOTIMPL, "CreateBlendState should return E_NOTIMPL for SRC_COLOR")) {
    return false;
  }

  // Destroy should be safe even after a failed create.
  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestUnsupportedBlendFactorIgnoredWhenBlendDisabled() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(blend disabled)")) {
    return false;
  }

  // When blending is disabled, D3D ignores the blend factors/ops. Ensure the UMD
  // does not reject otherwise-unrepresentable factors in that case.
  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kD3dBlendSrcColor; // unsupported by AeroGPU protocol
  desc.DestBlend = kD3dBlendInvDestColor; // unsupported by AeroGPU protocol
  desc.BlendOp = kD3dBlendOpAdd;
  desc.SrcBlendAlpha = kD3dBlendSrc1Alpha; // unsupported by AeroGPU protocol
  desc.DestBlendAlpha = kD3dBlendInvSrc1Alpha; // unsupported by AeroGPU protocol
  desc.BlendOpAlpha = kD3dBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 0;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage (blend disabled)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == S_OK, "CreateBlendState should accept unsupported factors when blending is disabled")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestPerRenderTargetMismatchReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(rt mismatch)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kD3dBlendSrcAlpha;
  desc.DestBlend = kD3dBlendInvSrcAlpha;
  desc.BlendOp = kD3dBlendOpAdd;
  desc.SrcBlendAlpha = kD3dBlendOne;
  desc.DestBlendAlpha = kD3dBlendZero;
  desc.BlendOpAlpha = kD3dBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 1;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  // RT1 differs (write mask differs). Not representable by AeroGPU protocol.
  desc.RenderTargetWriteMask[1] = 0x7;

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_NOTIMPL, "CreateBlendState should return E_NOTIMPL for per-RT state mismatch")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestAlphaToCoverageReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(alpha-to-coverage)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 1; // not representable by AeroGPU protocol
  desc.SrcBlend = kD3dBlendOne;
  desc.DestBlend = kD3dBlendZero;
  desc.BlendOp = kD3dBlendOpAdd;
  desc.SrcBlendAlpha = kD3dBlendOne;
  desc.DestBlendAlpha = kD3dBlendZero;
  desc.BlendOpAlpha = kD3dBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 0;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage (alpha-to-coverage)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_NOTIMPL, "CreateBlendState should return E_NOTIMPL for AlphaToCoverageEnable")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestWriteMaskHighBitsReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(write mask high bits)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kD3dBlendOne;
  desc.DestBlend = kD3dBlendZero;
  desc.BlendOp = kD3dBlendOpAdd;
  desc.SrcBlendAlpha = kD3dBlendOne;
  desc.DestBlendAlpha = kD3dBlendZero;
  desc.BlendOpAlpha = kD3dBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 0;
    // Bits outside RGBA are not representable by the AeroGPU protocol.
    desc.RenderTargetWriteMask[i] = 0x1F;
  }

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage (write mask high bits)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_NOTIMPL, "CreateBlendState should return E_NOTIMPL for write mask high bits")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestValidateAndConvertRejectsPerRtFactorMismatch() {
  // Portable test for the shared validator: D3D10.1 blend states can encode
  // per-render-target factors/ops, but the protocol cannot. Reject mismatches
  // unless all RTs match RT0.
  aerogpu::d3d10_11::D3dRtBlendDesc rts[2]{};
  rts[0].blend_enable = true;
  rts[0].write_mask = 0xFu;
  rts[0].src_blend = aerogpu::d3d10_11::kD3dBlendSrcAlpha;
  rts[0].dest_blend = aerogpu::d3d10_11::kD3dBlendInvSrcAlpha;
  rts[0].blend_op = aerogpu::d3d10_11::kD3dBlendOpAdd;
  rts[0].src_blend_alpha = aerogpu::d3d10_11::kD3dBlendOne;
  rts[0].dest_blend_alpha = aerogpu::d3d10_11::kD3dBlendZero;
  rts[0].blend_op_alpha = aerogpu::d3d10_11::kD3dBlendOpAdd;

  rts[1] = rts[0];
  rts[1].dest_blend = aerogpu::d3d10_11::kD3dBlendZero; // supported but mismatched vs RT0.

  aerogpu::d3d10_11::AerogpuBlendStateBase out{};
  const HRESULT hr = aerogpu::d3d10_11::ValidateAndConvertBlendDesc(rts,
                                                                    /*rt_count=*/2,
                                                                    /*alpha_to_coverage_enable=*/false,
                                                                    &out);
  return Check(hr == E_NOTIMPL, "ValidateAndConvertBlendDesc rejects per-RT factor mismatch");
}

bool TestValidateAndConvertRejectsD3d10_1Src1Factor() {
  // D3D10.1 adds SRC1_* blend factors. The protocol has no representation for
  // dual-source blending, so these must be rejected when blending is enabled.
  aerogpu::d3d10_11::D3dRtBlendDesc rt{};
  rt.blend_enable = true;
  rt.write_mask = 0xFu;
  rt.src_blend = aerogpu::d3d10_11::kD3dBlendSrc1Alpha;
  rt.dest_blend = aerogpu::d3d10_11::kD3dBlendZero;
  rt.blend_op = aerogpu::d3d10_11::kD3dBlendOpAdd;
  rt.src_blend_alpha = aerogpu::d3d10_11::kD3dBlendOne;
  rt.dest_blend_alpha = aerogpu::d3d10_11::kD3dBlendZero;
  rt.blend_op_alpha = aerogpu::d3d10_11::kD3dBlendOpAdd;

  aerogpu::d3d10_11::AerogpuBlendStateBase out{};
  const HRESULT hr = aerogpu::d3d10_11::ValidateAndConvertBlendDesc(&rt,
                                                                     /*rt_count=*/1,
                                                                     /*alpha_to_coverage_enable=*/false,
                                                                     &out);
  return Check(hr == E_NOTIMPL, "ValidateAndConvertBlendDesc rejects D3D10.1 SRC1_ALPHA");
}

bool TestUnsupportedBlendOpReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(unsupported blend op)")) {
    return false;
  }

  // Numeric values match D3D10_BLEND / D3D11_BLEND and D3D10_BLEND_OP / D3D11_BLEND_OP.
  constexpr uint32_t kBlendZero = 1;
  constexpr uint32_t kBlendOne = 2;
  constexpr uint32_t kBlendSrcAlpha = 5;
  constexpr uint32_t kBlendInvSrcAlpha = 6;
  constexpr uint32_t kBlendOpInvalid = 6; // valid ops are 1..5

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kBlendSrcAlpha;
  desc.DestBlend = kBlendInvSrcAlpha;
  desc.BlendOp = kBlendOpInvalid;
  desc.SrcBlendAlpha = kBlendOne;
  desc.DestBlendAlpha = kBlendZero;
  desc.BlendOpAlpha = kBlendOpInvalid;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 1;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  std::vector<uint8_t> state_storage;
  D3D10DDI_HBLENDSTATE hState = MakeBlendState(&dev, desc, &state_storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage (unsupported blend op)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_NOTIMPL, "CreateBlendState should return E_NOTIMPL for unsupported blend op")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  if (!TestUnsupportedBlendFactorReturnsNotImpl()) {
    return 1;
  }
  if (!TestUnsupportedBlendFactorIgnoredWhenBlendDisabled()) {
    return 1;
  }
  if (!TestPerRenderTargetMismatchReturnsNotImpl()) {
    return 1;
  }
  if (!TestAlphaToCoverageReturnsNotImpl()) {
    return 1;
  }
  if (!TestWriteMaskHighBitsReturnsNotImpl()) {
    return 1;
  }
  if (!TestValidateAndConvertRejectsPerRtFactorMismatch()) {
    return 1;
  }
  if (!TestValidateAndConvertRejectsD3d10_1Src1Factor()) {
    return 1;
  }
  if (!TestUnsupportedBlendOpReturnsNotImpl()) {
    return 1;
  }
  return 0;
}
