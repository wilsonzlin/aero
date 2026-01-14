#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "aerogpu_cmd.h"
#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_blend_state_validate.h"

namespace {

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

D3D10DDI_HBLENDSTATE AllocBlendStateStorage(TestDevice* dev,
                                           const AEROGPU_DDIARG_CREATEBLENDSTATE* desc,
                                           std::vector<uint8_t>* out_storage) {
  D3D10DDI_HBLENDSTATE h{};
  if (!dev || !out_storage) {
    return h;
  }

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateBlendStateSize(dev->hDevice, desc);
  out_storage->assign(static_cast<size_t>(size), 0);
  h.pDrvPrivate = out_storage->data();
  return h;
}

AEROGPU_DDIARG_CREATEBLENDSTATE MakeValidBlendDesc() {
  AEROGPU_DDIARG_CREATEBLENDSTATE desc{};
  desc.enable = 1;
  desc.src_factor = AEROGPU_BLEND_SRC_ALPHA;
  desc.dst_factor = AEROGPU_BLEND_INV_SRC_ALPHA;
  desc.blend_op = AEROGPU_BLEND_OP_ADD;
  desc.color_write_mask = 0xFu;
  desc.src_factor_alpha = AEROGPU_BLEND_ONE;
  desc.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  desc.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  return desc;
}

bool TestInvalidEnableReturnsInvalidArg() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(invalid enable)")) {
    return false;
  }

  auto desc = MakeValidBlendDesc();
  desc.enable = 2;  // invalid (>1)

  std::vector<uint8_t> storage;
  D3D10DDI_HBLENDSTATE hState = AllocBlendStateStorage(&dev, &desc, &storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateBlendState rejects enable=2")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestWriteMaskHighBitsReturnsInvalidArg() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(invalid write mask)")) {
    return false;
  }

  auto desc = MakeValidBlendDesc();
  desc.color_write_mask = 0x1Fu;  // invalid: only low 4 bits allowed

  std::vector<uint8_t> storage;
  D3D10DDI_HBLENDSTATE hState = AllocBlendStateStorage(&dev, &desc, &storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateBlendState rejects write mask high bits")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestInvalidBlendFactorReturnsInvalidArg() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(invalid blend factor)")) {
    return false;
  }

  auto desc = MakeValidBlendDesc();
  desc.src_factor = AEROGPU_BLEND_INV_CONSTANT + 1u;

  std::vector<uint8_t> storage;
  D3D10DDI_HBLENDSTATE hState = AllocBlendStateStorage(&dev, &desc, &storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateBlendState rejects out-of-range blend factor")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestInvalidBlendOpReturnsInvalidArg() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(invalid blend op)")) {
    return false;
  }

  auto desc = MakeValidBlendDesc();
  desc.blend_op = AEROGPU_BLEND_OP_MAX + 1u;

  std::vector<uint8_t> storage;
  D3D10DDI_HBLENDSTATE hState = AllocBlendStateStorage(&dev, &desc, &storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateBlendState rejects out-of-range blend op")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDestroyAfterFailedCreateIsSafe() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(destroy after failed create)")) {
    return false;
  }

  auto desc = MakeValidBlendDesc();
  desc.enable = 2;  // invalid (>1)

  std::vector<uint8_t> storage;
  D3D10DDI_HBLENDSTATE hState = AllocBlendStateStorage(&dev, &desc, &storage);
  if (!Check(hState.pDrvPrivate != nullptr, "blend state storage")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateBlendState rejects enable=2 (for destroy-after-failure test)")) {
    return false;
  }

  // Some runtimes may still call Destroy on failure; this must not crash.
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
  rts[1].dest_blend = aerogpu::d3d10_11::kD3dBlendZero;  // supported but mismatched vs RT0.

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

}  // namespace

int main() {
  bool ok = true;
  ok &= TestInvalidEnableReturnsInvalidArg();
  ok &= TestWriteMaskHighBitsReturnsInvalidArg();
  ok &= TestInvalidBlendFactorReturnsInvalidArg();
  ok &= TestInvalidBlendOpReturnsInvalidArg();
  ok &= TestDestroyAfterFailedCreateIsSafe();
  ok &= TestValidateAndConvertRejectsPerRtFactorMismatch();
  ok &= TestValidateAndConvertRejectsD3d10_1Src1Factor();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_blend_state_validation_tests\n");
  return 0;
}
