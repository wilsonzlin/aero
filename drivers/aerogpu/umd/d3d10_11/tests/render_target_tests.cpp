#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "aerogpu_cmd.h"
#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_internal.h"

namespace {

using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm;
using aerogpu::d3d10_11::kD3D11BindRenderTarget;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

size_t StreamBytesUsed(const uint8_t* buf, size_t len) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = static_cast<size_t>(stream->size_bytes);
  if (used < sizeof(aerogpu_cmd_stream_header) || used > len) {
    return len;
  }
  return used;
}

bool ValidateStream(const uint8_t* buf, size_t len) {
  if (!Check(buf != nullptr, "stream buffer must be non-null")) {
    return false;
  }
  if (!Check(len >= sizeof(aerogpu_cmd_stream_header), "stream must contain header")) {
    return false;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "stream magic")) {
    return false;
  }
  if (!Check(stream->abi_version == AEROGPU_ABI_VERSION_U32, "stream abi_version")) {
    return false;
  }
  if (!Check(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE, "stream flags")) {
    return false;
  }
  if (!Check(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header), "stream size_bytes >= header")) {
    return false;
  }
  if (!Check(stream->size_bytes <= len, "stream size_bytes within buffer")) {
    return false;
  }

  const size_t stream_len = static_cast<size_t>(stream->size_bytes);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream_len) {
    if (!Check(stream_len - offset >= sizeof(aerogpu_cmd_hdr), "packet header fits")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= header")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size is 4-byte aligned")) {
      return false;
    }
    if (!Check(hdr->size_bytes <= stream_len - offset, "packet size within stream")) {
      return false;
    }
    offset += hdr->size_bytes;
  }
  return Check(offset == stream_len, "parser consumed stream");
}

CmdLoc FindLastOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  CmdLoc loc{};
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return loc;
  }

  const size_t stream_len = StreamBytesUsed(buf, len);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return loc;
}

std::vector<aerogpu_handle_t> CollectCreateTexture2DHandles(const uint8_t* buf, size_t len) {
  std::vector<aerogpu_handle_t> handles;
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return handles;
  }
  const size_t stream_len = StreamBytesUsed(buf, len);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_CREATE_TEXTURE2D) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(hdr);
      handles.push_back(cmd->texture_handle);
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return handles;
}

struct Harness {
  std::vector<uint8_t> last_stream;

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                  const void* cmd_stream,
                                                  uint32_t cmd_stream_size_bytes,
                                                  const AEROGPU_WDDM_SUBMIT_ALLOCATION*,
                                                  uint32_t,
                                                  uint64_t* out_fence) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    if (out_fence) {
      *out_fence = 0;
    }
    return S_OK;
  }
};

struct TestDevice {
  Harness harness{};

  D3D10DDI_HADAPTER hAdapter{};
  D3D10DDI_ADAPTERFUNCS adapter_funcs{};

  D3D10DDI_HDEVICE hDevice{};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs{};
  std::vector<uint8_t> device_mem;

  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks{};
};

bool CreateDevice(TestDevice* out) {
  if (!out) {
    return false;
  }

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;

  D3D10DDIARG_OPENADAPTER open{};
  open.pAdapterFuncs = &out->adapter_funcs;
  const HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create{};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
    return false;
  }

  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = &out->callbacks;

  const HRESULT create_hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(create_hr == S_OK, "CreateDevice")) {
    return false;
  }

  out->hDevice = create.hDevice;
  return true;
}

struct TestResource {
  D3D10DDI_HRESOURCE hResource{};
  std::vector<uint8_t> storage;
};

struct TestRtv {
  D3D10DDI_HRENDERTARGETVIEW hRtv{};
  std::vector<uint8_t> storage;
};

bool TestDestroyAfterFailedCreateResourceIsSafe() {
  TestDevice dev{};
  if (!Check(CreateDevice(&dev), "CreateDevice(failed CreateResource)")) {
    return false;
  }

  // Create a small buffer with invalid initial data (null pSysMem). Some
  // runtimes may still call DestroyResource on CreateResource failure; this must
  // not crash or double-destroy the private object.
  AEROGPU_DDI_SUBRESOURCE_DATA init{};
  init.pSysMem = nullptr;
  init.SysMemPitch = 0;
  init.SysMemSlicePitch = 0;

  AEROGPU_DDIARG_CREATERESOURCE desc{};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.ByteWidth = 16;
  desc.StructureByteStride = 0;
  desc.pInitialData = &init;
  desc.InitialDataCount = 1;

  TestResource buf{};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateResourceSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize(buffer) returned non-trivial size")) {
    return false;
  }
  buf.storage.assign(static_cast<size_t>(size), 0);
  buf.hResource.pDrvPrivate = buf.storage.data();

  const HRESULT hr = dev.device_funcs.pfnCreateResource(dev.hDevice, &desc, buf.hResource);
  if (!Check(hr == E_INVALIDARG, "CreateResource(buffer) rejects null pSysMem in initial data")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool CreateRenderTargetTexture2D(TestDevice* dev, TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc{};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = kD3D11BindRenderTarget;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.Width = 4;
  desc.Height = 4;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = kDxgiFormatB8G8R8A8Unorm;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  return Check(hr == S_OK, "CreateResource(tex2d)");
}

bool CreateRTV(TestDevice* dev, const TestResource* tex, TestRtv* out) {
  if (!dev || !tex || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERENDERTARGETVIEW desc{};
  desc.hResource = tex->hResource;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateRTVSize(dev->hDevice, &desc);
  if (!Check(size != 0, "CalcPrivateRTVSize")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hRtv.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateRTV(dev->hDevice, &desc, out->hRtv);
  return Check(hr == S_OK, "CreateRTV");
}

bool TestTwoRtvs() {
  TestDevice dev{};
  if (!Check(CreateDevice(&dev), "CreateDevice(TestTwoRtvs)")) {
    return false;
  }

  TestResource tex0{};
  TestResource tex1{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  if (!CreateRenderTargetTexture2D(&dev, &tex0) || !CreateRenderTargetTexture2D(&dev, &tex1)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }

  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush")) {
    return false;
  }

  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const uint8_t* buf = dev.harness.last_stream.data();
  const size_t len = dev.harness.last_stream.size();
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(buf, len);
  if (!Check(created.size() >= 2, "expected >=2 CREATE_TEXTURE2D")) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 2, "color_count==2")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == created[0], "colors[0] matches tex0")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "colors[1] matches tex1")) {
    return false;
  }

  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestClampAndNullEntries() {
  TestDevice dev{};
  if (!Check(CreateDevice(&dev), "CreateDevice(TestClampAndNullEntries)")) {
    return false;
  }

  constexpr uint32_t kRequested = AEROGPU_MAX_RENDER_TARGETS + 1;
  std::vector<TestResource> textures(kRequested);
  std::vector<TestRtv> rtvs(kRequested);
  for (uint32_t i = 0; i < kRequested; ++i) {
    if (!CreateRenderTargetTexture2D(&dev, &textures[i]) || !CreateRTV(&dev, &textures[i], &rtvs[i])) {
      return false;
    }
  }

  // Provide a >8 RTV array with a null entry in the middle and a non-null entry at slot 8 (ignored).
  D3D10DDI_HRENDERTARGETVIEW views[kRequested] = {};
  views[0] = rtvs[0].hRtv;
  views[1] = D3D10DDI_HRENDERTARGETVIEW{}; // null
  views[2] = rtvs[1].hRtv;
  views[7] = rtvs[2].hRtv;
  views[8] = rtvs[3].hRtv; // should be ignored by clamp

  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, kRequested, views, D3D10DDI_HDEPTHSTENCILVIEW{});
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush(clamp)")) {
    return false;
  }

  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const uint8_t* buf = dev.harness.last_stream.data();
  const size_t len = dev.harness.last_stream.size();
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(buf, len);
  if (!Check(created.size() >= kRequested, "expected CREATE_TEXTURE2D handles")) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (clamp)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == AEROGPU_MAX_RENDER_TARGETS, "color_count clamped to 8")) {
    return false;
  }

  if (!Check(set_rt->colors[0] == created[0], "colors[0]==tex0")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == 0, "colors[1]==0 (null)")) {
    return false;
  }
  if (!Check(set_rt->colors[2] == created[1], "colors[2]==tex1")) {
    return false;
  }
  for (uint32_t i = 3; i < 7; ++i) {
    if (!Check(set_rt->colors[i] == 0, "colors[3..6]==0")) {
      return false;
    }
  }
  if (!Check(set_rt->colors[7] == created[2], "colors[7]==tex2")) {
    return false;
  }

  for (uint32_t i = 0; i < kRequested; ++i) {
    dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtvs[i].hRtv);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, textures[i].hResource);
  }
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestUnbindAllRtvs() {
  TestDevice dev{};
  if (!Check(CreateDevice(&dev), "CreateDevice(TestUnbindAllRtvs)")) {
    return false;
  }

  TestResource tex0{};
  TestRtv rtv0{};
  if (!CreateRenderTargetTexture2D(&dev, &tex0) || !CreateRTV(&dev, &tex0, &rtv0)) {
    return false;
  }

  // Bind then unbind to ensure the "clear all RTVs" path is encoded.
  D3D10DDI_HRENDERTARGETVIEW bind_views[1] = {rtv0.hRtv};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/1, bind_views, D3D10DDI_HDEPTHSTENCILVIEW{});
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*views=*/nullptr, D3D10DDI_HDEPTHSTENCILVIEW{});
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush(unbind)")) {
    return false;
  }

  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const uint8_t* buf = dev.harness.last_stream.data();
  const size_t len = dev.harness.last_stream.size();
  const CmdLoc loc = FindLastOpcode(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (unbind)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 0, "color_count==0 (unbind)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == 0, "depth_stencil==0 (unbind)")) {
    return false;
  }
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "colors[i]==0 (unbind)")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestDestroyAfterFailedCreateResourceIsSafe();
  ok &= TestTwoRtvs();
  ok &= TestClampAndNullEntries();
  ok &= TestUnbindAllRtvs();
  return ok ? 0 : 1;
}
