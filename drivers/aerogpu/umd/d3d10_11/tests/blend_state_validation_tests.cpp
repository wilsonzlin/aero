#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"

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

  // Numeric values match D3D10_BLEND / D3D11_BLEND.
  constexpr uint32_t kBlendZero = 1;
  constexpr uint32_t kBlendOne = 2;
  constexpr uint32_t kBlendSrcColor = 3; // unsupported by AeroGPU protocol
  constexpr uint32_t kBlendOpAdd = 1;

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kBlendSrcColor;
  desc.DestBlend = kBlendZero;
  desc.BlendOp = kBlendOpAdd;
  desc.SrcBlendAlpha = kBlendOne;
  desc.DestBlendAlpha = kBlendZero;
  desc.BlendOpAlpha = kBlendOpAdd;
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

bool TestPerRenderTargetMismatchReturnsNotImpl() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(rt mismatch)")) {
    return false;
  }

  // Numeric values match D3D10_BLEND / D3D11_BLEND.
  constexpr uint32_t kBlendZero = 1;
  constexpr uint32_t kBlendOne = 2;
  constexpr uint32_t kBlendSrcAlpha = 5;
  constexpr uint32_t kBlendInvSrcAlpha = 6;
  constexpr uint32_t kBlendOpAdd = 1;

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kBlendSrcAlpha;
  desc.DestBlend = kBlendInvSrcAlpha;
  desc.BlendOp = kBlendOpAdd;
  desc.SrcBlendAlpha = kBlendOne;
  desc.DestBlendAlpha = kBlendZero;
  desc.BlendOpAlpha = kBlendOpAdd;
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

  constexpr uint32_t kBlendZero = 1;
  constexpr uint32_t kBlendOne = 2;
  constexpr uint32_t kBlendOpAdd = 1;

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 1; // not representable by AeroGPU protocol
  desc.SrcBlend = kBlendOne;
  desc.DestBlend = kBlendZero;
  desc.BlendOp = kBlendOpAdd;
  desc.SrcBlendAlpha = kBlendOne;
  desc.DestBlendAlpha = kBlendZero;
  desc.BlendOpAlpha = kBlendOpAdd;
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

  constexpr uint32_t kBlendZero = 1;
  constexpr uint32_t kBlendOne = 2;
  constexpr uint32_t kBlendOpAdd = 1;

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.AlphaToCoverageEnable = 0;
  desc.SrcBlend = kBlendOne;
  desc.DestBlend = kBlendZero;
  desc.BlendOp = kBlendOpAdd;
  desc.SrcBlendAlpha = kBlendOne;
  desc.DestBlendAlpha = kBlendZero;
  desc.BlendOpAlpha = kBlendOpAdd;
  for (uint32_t i = 0; i < 8; ++i) {
    desc.BlendEnable[i] = 0;
    desc.RenderTargetWriteMask[i] = 0xF;
  }

  // Bits outside RGBA are not representable by the AeroGPU protocol.
  desc.RenderTargetWriteMask[0] = 0x1F;

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

} // namespace

int main() {
  if (!TestUnsupportedBlendFactorReturnsNotImpl()) {
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
  return 0;
}
