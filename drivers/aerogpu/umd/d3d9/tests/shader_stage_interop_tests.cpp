#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;

constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfUnsupportedXyz = kD3dFvfXyz;

// Trivial vs_2_0 token stream (no declaration):
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v0
//   end
static constexpr uint32_t kUserVsPassthroughPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x02000001u, // mov
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// Trivial ps_2_0 token stream (no declaration):
//   mov oC0, v0
//   end
static constexpr uint32_t kUserPsPassthroughColor[] = {
    0xFFFF0200u, // ps_2_0
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

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

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

CmdLoc FindLastOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  CmdLoc out{};
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return out;
  }
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.hdr = hdr;
      out.offset = offset;
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
  std::vector<D3D9DDI_HSHADER> shaders;
  bool has_adapter = false;
  bool has_device = false;

  ~CleanupDevice() {
    if (has_device && device_funcs.pfnDestroyShader) {
      for (auto& s : shaders) {
        if (s.pDrvPrivate) {
          device_funcs.pfnDestroyShader(hDevice, s);
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

  if (!Check(cleanup->device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyShader != nullptr, "pfnDestroyShader")) {
    return false;
  }
  return true;
}

struct VertexXyzrhwDiffuse {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
};

bool TestVsOnlyBindsFixedfuncPs() {
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

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure at least one bind references the user VS and binds a non-null PS.
  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestPsOnlyBindsFixedfuncVs() {
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

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure at least one bind references the user PS and binds a non-null VS.
  bool saw_user_ps_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle");
}

bool TestUnsupportedFvfPsOnlyFailsWithoutDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfUnsupportedXyz);
  if (!Check(hr == S_OK, "SetFVF(unsupported XYZ)")) {
    return false;
  }

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const size_t baseline_size = dev->cmd.size();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(PS-only, unsupported FVF) returns INVALIDCALL")) {
    return false;
  }

  // ensure_draw_pipeline_locked is invoked before any UP uploads, so the draw must
  // fail without emitting any draw packets.
  if (!Check(dev->cmd.size() == baseline_size, "no additional commands emitted")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(negative)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 0, "no DRAW opcodes emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }

  // The command stream must never contain null shader binds.
  CmdLoc bind = FindLastOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (bind.hdr) {
    const auto* bind_cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(bind.hdr);
    if (!Check(bind_cmd->vs != 0 && bind_cmd->ps != 0, "BIND_SHADERS must not bind null handles")) {
      return false;
    }
  }
  return true;
}

} // namespace aerogpu

int main() {
  if (!aerogpu::TestVsOnlyBindsFixedfuncPs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyBindsFixedfuncVs()) {
    return 1;
  }
  if (!aerogpu::TestUnsupportedFvfPsOnlyFailsWithoutDraw()) {
    return 1;
  }
  return 0;
}
