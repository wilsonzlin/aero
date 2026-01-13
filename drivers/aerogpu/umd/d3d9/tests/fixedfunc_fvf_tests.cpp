#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"

namespace aerogpu {
namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;

constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;

// D3DTSS_* texture stage state IDs (from d3d9types.h).
constexpr uint32_t kD3dTssColorOp = 1u;
// D3DTEXTUREOP values (from d3d9types.h).
constexpr uint32_t kD3dTopSelectArg1 = 2u;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

size_t StreamBytesUsed(const uint8_t* buf, size_t capacity) {
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = stream->size_bytes;
  if (used < sizeof(aerogpu_cmd_stream_header) || used > capacity) {
    return 0;
  }
  return used;
}

bool ValidateStream(const uint8_t* buf, size_t capacity) {
  if (!Check(buf != nullptr, "buffer must be non-null")) {
    return false;
  }
  if (!Check(capacity >= sizeof(aerogpu_cmd_stream_header), "buffer must contain stream header")) {
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
  if (!Check(stream->size_bytes <= capacity, "stream size_bytes within capacity")) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream->size_bytes) {
    if (!Check((offset & 3u) == 0, "packet offset 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes, "packet header within stream")) {
      return false;
    }

    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= hdr")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + hdr->size_bytes <= stream->size_bytes, "packet fits within stream")) {
      return false;
    }

    offset += hdr->size_bytes;
  }
  return Check(offset == stream->size_bytes, "parser consumed entire stream");
}

size_t CountOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

std::vector<const aerogpu_cmd_hdr*> CollectOpcodes(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  std::vector<const aerogpu_cmd_hdr*> out;
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return out;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.push_back(hdr);
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

struct CleanupDevice {
  D3D9DDI_ADAPTERFUNCS adapter_funcs{};
  D3D9DDI_DEVICEFUNCS device_funcs{};
  D3DDDI_HADAPTER hAdapter{};
  D3DDDI_HDEVICE hDevice{};
  std::vector<D3DDDI_HRESOURCE> resources{};
  bool has_adapter = false;
  bool has_device = false;

  ~CleanupDevice() {
    if (has_device && device_funcs.pfnDestroyResource) {
      for (auto& r : resources) {
        if (r.pDrvPrivate) {
          device_funcs.pfnDestroyResource(hDevice, r);
        }
      }
    }
    if (has_device && device_funcs.pfnDestroyDevice) {
      device_funcs.pfnDestroyDevice(hDevice);
    }
    if (has_adapter && adapter_funcs.pfnCloseAdapter) {
      adapter_funcs.pfnCloseAdapter(hAdapter);
    }
  }
};

bool CreateDevice(CleanupDevice* cleanup) {
  if (!cleanup) {
    return false;
  }

  D3DDDIARG_OPENADAPTER2 open{};
  open.Interface = 1;
  open.Version = 1;
  D3DDDI_ADAPTERCALLBACKS callbacks{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks2{};
  open.pAdapterCallbacks = &callbacks;
  open.pAdapterCallbacks2 = &callbacks2;
  open.pAdapterFuncs = &cleanup->adapter_funcs;

  HRESULT hr = ::OpenAdapter2(&open);
  if (!Check(hr == S_OK, "OpenAdapter2")) {
    return false;
  }
  if (!Check(open.hAdapter.pDrvPrivate != nullptr, "OpenAdapter2 returned adapter handle")) {
    return false;
  }
  cleanup->hAdapter = open.hAdapter;
  cleanup->has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup->adapter_funcs.pfnCreateDevice(&create_dev, &cleanup->device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  if (!Check(create_dev.hDevice.pDrvPrivate != nullptr, "CreateDevice returned device handle")) {
    return false;
  }
  cleanup->hDevice = create_dev.hDevice;
  cleanup->has_device = true;

  if (!Check(cleanup->device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetTexture != nullptr, "pfnSetTexture is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource is available")) {
    return false;
  }
  return true;
}

bool CreateDummyTexture(CleanupDevice* cleanup, D3DDDI_HRESOURCE* out_tex) {
  if (!cleanup || !out_tex) {
    return false;
  }

  // D3DFMT_X8R8G8B8 = 22.
  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 3u; // D3DRTYPE_TEXTURE (conventional value; AeroGPU currently treats this as metadata)
  create_res.format = 22u;
  create_res.width = 2;
  create_res.height = 2;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  HRESULT hr = cleanup->device_funcs.pfnCreateResource(cleanup->hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(texture2d)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned hResource")) {
    return false;
  }

  cleanup->resources.push_back(create_res.hResource);
  *out_tex = create_res.hResource;
  return true;
}

struct VertexXyzrhwDiffuse {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
};

struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
};

bool TestFvfXyzrhwDiffuseEmitsSaneCommands() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|DIFFUSE)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }

  return true;
}

bool TestFvfXyzrhwDiffuseTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  const auto set_textures = CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE);
  if (!Check(!set_textures.empty(), "SET_TEXTURE packets collected")) {
    return false;
  }
  const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(set_textures.back());
  if (!Check(st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL, "SET_TEXTURE shader_stage == PIXEL")) {
    return false;
  }
  if (!Check(st->slot == 0, "SET_TEXTURE slot == 0")) {
    return false;
  }
  if (!Check(st->texture != 0, "SET_TEXTURE texture handle non-zero")) {
    return false;
  }

  return true;
}

bool TestStageStateChangeRebindsShadersIfImplemented() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(first)")) {
    return false;
  }

  // Mutate cached stage state directly (portable tests don't have a DDI entrypoint
  // for SetTextureStageState in the minimal header).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->texture_stage_states[0][kD3dTssColorOp] = kD3dTopSelectArg1;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(second)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(stage-state change)")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS present")) {
    return false;
  }

  // If stage-state-driven shader selection is implemented, we expect a second
  // BIND_SHADERS packet (different shader handles, or at least a re-bind).
  if (binds.size() >= 2) {
    const auto* first = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.front());
    const auto* last = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
    (void)first;
    if (!Check(last->vs != 0 && last->ps != 0, "rebind binds non-zero VS/PS")) {
      return false;
    }
  }

  return true;
}

} // namespace
} // namespace aerogpu

int main() {
  if (!aerogpu::TestFvfXyzrhwDiffuseEmitsSaneCommands()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwDiffuseTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestStageStateChangeRebindsShadersIfImplemented()) {
    return 1;
  }
  return 0;
}

