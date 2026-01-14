#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <unordered_set>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// Host-test helper (defined in `src/aerogpu_d3d9_driver.cpp` under "Host-side
// test entrypoints"). Unit tests may call this directly when the device vtable
// does not expose SetTextureStageState in a given build configuration.
HRESULT AEROGPU_D3D9_CALL device_set_texture_stage_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value);

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;

constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzrhwTex1 = kD3dFvfXyzRhw | kD3dFvfTex1;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuseTex1 = kD3dFvfXyz | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzTex1 = kD3dFvfXyz | kD3dFvfTex1;

// D3D9 shader stage IDs used by the DDI (from d3d9umddi.h). Keep local numeric
// definitions so portable builds don't require the Windows SDK/WDK.
constexpr uint32_t kD3dShaderStageVs = 0u;
constexpr uint32_t kD3dShaderStagePs = 1u;

// D3DTSS_* texture stage state IDs (from d3d9types.h).
constexpr uint32_t kD3dTssColorOp = 1u;
constexpr uint32_t kD3dTssColorArg1 = 2u;
constexpr uint32_t kD3dTssColorArg2 = 3u;
constexpr uint32_t kD3dTssAlphaOp = 4u;
constexpr uint32_t kD3dTssAlphaArg1 = 5u;
constexpr uint32_t kD3dTssAlphaArg2 = 6u;
// D3DTEXTUREOP values (from d3d9types.h).
constexpr uint32_t kD3dTopDisable = 1u;
constexpr uint32_t kD3dTopSelectArg1 = 2u;
constexpr uint32_t kD3dTopSelectArg2 = 3u;
constexpr uint32_t kD3dTopModulate = 4u;
constexpr uint32_t kD3dTopModulate2x = 5u;
constexpr uint32_t kD3dTopModulate4x = 6u;
constexpr uint32_t kD3dTopAdd = 7u;
constexpr uint32_t kD3dTopSubtract = 10u;

// D3DTA_* source selector (from d3d9types.h).
constexpr uint32_t kD3dTaDiffuse = 0u;
constexpr uint32_t kD3dTaTexture = 2u;
constexpr uint32_t kD3dTaTFactor = 3u;

// D3DRS_* render state IDs (from d3d9types.h).
constexpr uint32_t kD3dRsTextureFactor = 60u; // D3DRS_TEXTUREFACTOR

// D3DTRANSFORMSTATETYPE numeric values (from d3d9types.h).
constexpr uint32_t kD3dTransformView = 2u;
constexpr uint32_t kD3dTransformProjection = 3u;
constexpr uint32_t kD3dTransformWorld0 = 256u;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

template <size_t N>
bool ShaderBytecodeEquals(const Shader* shader, const uint32_t (&expected)[N]) {
  if (!shader) {
    return false;
  }
  if (shader->bytecode.size() != sizeof(expected)) {
    return false;
  }
  return std::memcmp(shader->bytecode.data(), expected, sizeof(expected)) == 0;
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
  std::vector<D3D9DDI_HVERTEXDECL> vertex_decls{};
  std::vector<D3D9DDI_HSHADER> shaders{};
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
    if (has_device && device_funcs.pfnDestroyVertexDecl) {
      for (auto& d : vertex_decls) {
        if (d.pDrvPrivate) {
          device_funcs.pfnDestroyVertexDecl(hDevice, d);
        }
      }
    }
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
  if (!Check(cleanup->device_funcs.pfnCreateVertexDecl != nullptr, "pfnCreateVertexDecl is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetVertexDecl != nullptr, "pfnSetVertexDecl is available")) {
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
  if (!Check(cleanup->device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyShader != nullptr, "pfnDestroyShader is available")) {
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

struct VertexXyzrhwTex1 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
};

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  uint32_t color;
};

struct VertexXyzDiffuseTex1 {
  float x;
  float y;
  float z;
  uint32_t color;
  float u;
  float v;
};

struct VertexXyzTex1 {
  float x;
  float y;
  float z;
  float u;
  float v;
};

#pragma pack(push, 1)
struct D3DVERTEXELEMENT9_COMPAT {
  uint16_t Stream;
  uint16_t Offset;
  uint8_t Type;
  uint8_t Method;
  uint8_t Usage;
  uint8_t UsageIndex;
};
#pragma pack(pop)

static_assert(sizeof(D3DVERTEXELEMENT9_COMPAT) == 8, "D3DVERTEXELEMENT9_COMPAT must be 8 bytes");

constexpr uint8_t kD3dDeclTypeFloat2 = 1;
constexpr uint8_t kD3dDeclTypeFloat3 = 2;
constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

constexpr uint8_t kD3dDeclUsagePosition = 0;
constexpr uint8_t kD3dDeclUsageTexcoord = 5;
constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

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

  aerogpu_handle_t expected_input_layout = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl) {
      expected_input_layout = dev->fvf_vertex_decl->handle;
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF created internal input layout")) {
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

  // With no bound texture, the fixed-function fallback should not select a
  // texture-sampling PS even though the D3D9 default stage0 COLOROP is MODULATE.
  // (This is a common configuration for untextured apps that never touch stage
  // state but rely on vertex diffuse.)
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_ps != nullptr, "fixedfunc_ps created")) {
      return false;
    }
    if (!Check(dev->ps == dev->fixedfunc_ps, "fixed-function PS is bound (no texture)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
               "fixed-function PS bytecode (no texture -> passthrough)")) {
      return false;
    }
  }

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
    }
  }
  if (!Check(expected_vb != 0, "DrawPrimitiveUP created scratch vertex buffer")) {
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

  // Validate shader creation includes both stages.
  bool saw_vs = false;
  bool saw_ps = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps = true;
    }
  }
  if (!Check(saw_vs && saw_ps, "CREATE_SHADER_DXBC includes VS and PS stages")) {
    return false;
  }

  // Validate the input layout being set matches the internal FVF declaration.
  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal FVF layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer")) {
    return false;
  }

  // Validate draw parameters (trianglelist => 3 vertices).
  bool saw_draw3 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw3 = true;
      break;
    }
  }
  if (!Check(saw_draw3, "DRAW has expected vertex_count=3 instance_count=1")) {
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

bool TestFvfXyzDiffuseEmitsInputLayoutAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // XYZ vertices are transformed to clip-space by a draw-time CPU conversion
  // path (fixed-function emulation). With identity transforms, these inputs are
  // already clip-space.
  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE)")) {
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

bool TestFvfXyzDiffuseEmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  // Fixed-function emulation for XYZ vertices uses a WVP vertex shader and
  // uploads the matrix into reserved VS constants c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_xyz_diffuse) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE internal vertex decl matches expected layout")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_vs_xyz_diffuse != nullptr, "fixedfunc_vs_xyz_diffuse created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse, "XYZ|DIFFUSE binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColor),
               "XYZ|DIFFUSE VS bytecode matches kVsWvpPosColor")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "scratch VB storage contains uploaded vertices")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), tri, sizeof(tri)) == 0,
                 "scratch VB contains original XYZ|DIFFUSE vertices (no CPU conversion)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE WVP VS)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer
  // with the original stride.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE original stride)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (XYZ|DIFFUSE)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseDrawPrimitiveVbCpuTransformsAndBindsScratchVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const VertexXyzrhwDiffuse expected_clip[3] = {
      {-1.0f + tx, -1.0f + ty, 0.0f + tz, 1.0f, 0xFFFF0000u},
      {1.0f + tx, -1.0f + ty, 0.0f + tz, 1.0f, 0xFF00FF00u},
      {-1.0f + tx, 1.0f + ty, 0.0f + tz, 1.0f, 0xFF0000FFu},
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_xyz_diffuse) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }

  // Set a simple world translation; view/projection are identity.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE internal vertex decl matches expected layout")) {
    return false;
  }

  // Create a VB (non-UP draw path) with a leading dummy vertex, then draw starting
  // at vertex 1. This exercises `start_vertex` handling in the CPU-transform path.
  const VertexXyzDiffuse verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu},
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz|diffuse)")) {
    return false;
  }

  aerogpu_handle_t expected_clip_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    if (!Check(dev->fixedfunc_vs != nullptr, "fixedfunc_vs created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs, "XYZ|DIFFUSE binds passthrough VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColor),
               "XYZ|DIFFUSE VS bytecode passthrough")) {
      return false;
    }

    if (dev->fvf_vertex_decl) {
      expected_clip_input_layout = dev->fvf_vertex_decl->handle;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(expected_clip),
                 "scratch VB storage contains converted vertices")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), expected_clip, sizeof(expected_clip)) == 0,
                 "scratch VB contains expected clip-space vertices (XYZ|DIFFUSE VB draw)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE VB draw)")) {
    return false;
  }
  if (!Check(expected_clip_input_layout != 0, "clip-space decl handle non-zero (XYZ|DIFFUSE VB draw)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE VB CPU transform)")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE layout handle (VB draw)")) {
    return false;
  }

  bool saw_clip_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_clip_input_layout) {
      saw_clip_layout = true;
      break;
    }
  }
  if (!Check(saw_clip_layout, "SET_INPUT_LAYOUT binds clip-space layout handle (XYZ|DIFFUSE VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE VB clip-space)")) {
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

  aerogpu_handle_t expected_input_layout = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_tex1->handle;
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF TEX1 created internal input layout")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  auto* tex = reinterpret_cast<Resource*>(hTex.pDrvPrivate);
  if (!Check(tex != nullptr, "texture resource pointer")) {
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

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
    }
  }
  if (!Check(expected_vb != 0, "DrawPrimitiveUP TEX1 created scratch vertex buffer")) {
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

  // Validate shader creation includes both stages.
  bool saw_vs = false;
  bool saw_ps = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps = true;
    }
  }
  if (!Check(saw_vs && saw_ps, "CREATE_SHADER_DXBC includes VS and PS stages (TEX1)")) {
    return false;
  }

  // Validate the input layout being set matches the internal FVF declaration.
  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal TEX1 FVF layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (TEX1)")) {
    return false;
  }

  // Validate draw parameters (trianglelist => 3 vertices).
  bool saw_draw3 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw3 = true;
      break;
    }
  }
  if (!Check(saw_draw3, "DRAW has expected vertex_count=3 instance_count=1 (TEX1)")) {
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
  if (!Check(st->texture == tex->handle, "SET_TEXTURE uses created texture handle")) {
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

bool TestFvfXyzDiffuseTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
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

  const VertexXyzDiffuseTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1)")) {
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

bool TestFvfXyzDiffuseTex1EmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_xyz_diffuse_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE|TEX1 internal vertex decl matches expected layout")) {
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

  const VertexXyzDiffuseTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse tex1)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_vs_xyz_diffuse_tex1 != nullptr, "fixedfunc_vs_xyz_diffuse_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse_tex1, "XYZ|DIFFUSE|TEX1 binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0),
               "XYZ|DIFFUSE|TEX1 VS bytecode matches kVsWvpPosColorTex0")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "scratch VB storage contains uploaded vertices (TEX1)")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), tri, sizeof(tri)) == 0,
                 "scratch VB contains original XYZ|DIFFUSE|TEX1 vertices (no CPU conversion)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1 WVP VS)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE|TEX1 layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer
  // with the original stride.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE|TEX1 original stride)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseTex1DrawPrimitiveVbCpuTransformsAndBindsScratchVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const VertexXyzrhwDiffuseTex1 expected_clip[3] = {
      {-1.0f + tx, -1.0f + ty, 0.0f + tz, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f + tx, -1.0f + ty, 0.0f + tz, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f + tx, 1.0f + ty, 0.0f + tz, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_xyz_diffuse_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }

  // Set a simple world translation; view/projection are identity.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE|TEX1 internal vertex decl matches expected layout")) {
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

  // Create a VB (non-UP draw path) with a leading dummy vertex, then draw starting
  // at vertex 1. This exercises `start_vertex` handling in the CPU-transform path.
  const VertexXyzDiffuseTex1 verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu, 9.0f, 9.0f},
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse|tex1)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz|diffuse|tex1)")) {
    return false;
  }

  aerogpu_handle_t expected_clip_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    if (!Check(dev->fixedfunc_vs_xyz_diffuse_tex1 != nullptr, "fixedfunc_vs_xyz_diffuse_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse_tex1, "XYZ|DIFFUSE|TEX1 binds passthrough VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColorTex1),
               "XYZ|DIFFUSE|TEX1 VS bytecode passthrough")) {
      return false;
    }

    if (dev->fvf_vertex_decl_tex1) {
      expected_clip_input_layout = dev->fvf_vertex_decl_tex1->handle;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(expected_clip),
                 "scratch VB storage contains converted vertices (TEX1)")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), expected_clip, sizeof(expected_clip)) == 0,
                 "scratch VB contains expected clip-space vertices (XYZ|DIFFUSE|TEX1 VB draw)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE|TEX1 VB draw)")) {
    return false;
  }
  if (!Check(expected_clip_input_layout != 0, "clip-space decl handle non-zero (XYZ|DIFFUSE|TEX1 VB draw)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1 VB CPU transform)")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE|TEX1 layout handle (VB draw)")) {
    return false;
  }

  bool saw_clip_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_clip_input_layout) {
      saw_clip_layout = true;
      break;
    }
  }
  if (!Check(saw_clip_layout, "SET_INPUT_LAYOUT binds clip-space layout handle (XYZ|DIFFUSE|TEX1 VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE|TEX1 VB clip-space)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzrhwTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_tex1_nodiffuse) {
      expected_input_layout = dev->fvf_vertex_decl_tex1_nodiffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_tex1_nodiffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZRHW|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZRHW|TEX1 internal vertex decl matches expected layout")) {
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

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyzrhw tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1)")) {
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

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZRHW|TEX1 layout handle")) {
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

bool TestFvfXyzTex1EmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->fvf_vertex_decl_xyz_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|TEX1 internal vertex decl matches expected layout")) {
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

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F) >= 1, "SET_SHADER_CONSTANTS_F emitted")) {
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

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|TEX1 layout handle")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns")) {
    return false;
  }

  return true;
}

bool TestFvfXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  // Set a non-identity transform so the fixed-function WVP constant upload is
  // easy to spot (WVP columns are uploaded into c240..c243).
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
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

  // Create a vertex buffer (non-UP path) and populate it via Lock/Unlock.
  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;   // Buffer type is inferred from `size` by the UMD.
  create_vb.format = 0u; // Unused for buffers.
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(tri);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, tri, sizeof(tri));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/0, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (VB draw)")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzrhwTex1InfersFvfAndBindsShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZRHW|TEX1.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }

  // Verify implied FVF inference.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
      return false;
    }
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1 via decl)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1 via decl)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) >= 1, "CREATE_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure the decl's input layout handle is bound (not an internal FVF decl).
  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "vertex decl handle non-zero")) {
    return false;
  }
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzTex1InfersFvfAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZ|TEX1.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
    return false;
  }

  // Verify implied FVF inference.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
      return false;
    }
  }

  // Provide a simple transform to ensure the WVP constant upload is observable.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
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

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1 via decl)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 via decl)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl path)")) {
    return false;
  }

  return true;
}

bool TestSetTextureStageStateUpdatesPsForTex1NoDiffuseFvfs() {
  // ---------------------------------------------------------------------------
  // XYZRHW | TEX1
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
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

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZRHW|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    const VertexXyzrhwTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
    };

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyzrhw tex1)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZRHW|TEX1: PS bytecode modulate/texture")) {
        return false;
      }
    }

    // Validate SetTexture(stage0) hot-swaps the internal fixed-function PS variant
    // when fixed-function is active (no user shaders bound).
    {
      D3DDDI_HRESOURCE null_tex{};
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
      if (!Check(hr == S_OK, "XYZRHW|TEX1: SetTexture(stage0=null) succeeds")) {
        return false;
      }
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTexture(null)")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZRHW|TEX1: PS bytecode (no texture -> passthrough)")) {
        return false;
      }
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "XYZRHW|TEX1: SetTexture(stage0=texture) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTexture(texture)")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZRHW|TEX1: PS bytecode (texture restored -> modulate/texture)")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZRHW|TEX1: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZRHW|TEX1: PS bytecode disable->passthrough")) {
        return false;
      }
    }
  }

  // ---------------------------------------------------------------------------
  // XYZ | TEX1
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
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

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZ|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZ|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    const VertexXyzTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
    };

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZ|TEX1: PS bytecode modulate/texture")) {
        return false;
      }
    }

    // Validate SetTexture(stage0) hot-swaps the internal fixed-function PS variant
    // when fixed-function is active (no user shaders bound).
    {
      D3DDDI_HRESOURCE null_tex{};
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
      if (!Check(hr == S_OK, "XYZ|TEX1: SetTexture(stage0=null) succeeds")) {
        return false;
      }
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTexture(null)")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZ|TEX1: PS bytecode (no texture -> passthrough)")) {
        return false;
      }
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "XYZ|TEX1: SetTexture(stage0=texture) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTexture(texture)")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZ|TEX1: PS bytecode (texture restored -> modulate/texture)")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZ|TEX1: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZ|TEX1: PS bytecode disable->passthrough")) {
        return false;
      }
    }
  }

  return true;
}

bool TestPsOnlyInteropXyzrhwTex1SynthesizesVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). D3D9 expects the runtime to
  // interop fixed-function on the missing stage.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    if (!Check(dev->user_vs == nullptr, "PS-only interop: user_vs is NULL")) {
      return false;
    }
    if (!Check(dev->user_ps == user_ps, "PS-only interop: user_ps is bound")) {
      return false;
    }

    if (!Check(dev->fixedfunc_vs_tex1_nodiffuse != nullptr, "interop created fixedfunc_vs_tex1_nodiffuse")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_tex1_nodiffuse, "interop bound fixedfunc VS (XYZRHW|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1),
               "synthesized VS bytecode matches kVsPassthroughPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZRHW|TEX1)")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzTex1SynthesizesVsAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    if (!Check(dev->fixedfunc_vs_xyz_tex1 != nullptr, "interop created fixedfunc_vs_xyz_tex1")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_tex1, "interop bound fixedfunc VS (XYZ|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "synthesized VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|TEX1)")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  // The synthesized fixed-function VS for `XYZ | TEX1` requires a WVP upload
  // (reserved register range c240..c243).
  bool saw_wvp = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX && sc->start_register == 240 && sc->vec4_count == 4) {
      saw_wvp = true;
      break;
    }
  }
  if (!Check(saw_wvp, "PS-only interop uploaded WVP constants")) {
    return false;
  }
  return true;
}

bool TestPsOnlyInteropVertexDeclXyzrhwTex1SynthesizesVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Bind an explicit vertex decl matching XYZRHW|TEX1 (no SetFVF call). The driver
  // should infer the implied FVF and still be able to synthesize the fixed-function
  // VS when only a pixel shader is bound.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
      return false;
    }
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
    return false;
  }

  // Bind only a user pixel shader.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop via decl XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    if (!Check(dev->fixedfunc_vs_tex1_nodiffuse != nullptr, "interop created fixedfunc_vs_tex1_nodiffuse")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_tex1_nodiffuse, "interop bound fixedfunc VS (XYZRHW|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1),
               "synthesized VS bytecode matches kVsPassthroughPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop via decl XYZRHW|TEX1)")) {
    return false;
  }

  // Explicit vertex decl input layout must remain bound (no SetFVF internal decl).
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropVertexDeclXyzTex1SynthesizesVsAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Bind an explicit vertex decl matching XYZ|TEX1 (no SetFVF call). The driver
  // should infer the implied FVF and still be able to synthesize the WVP fixed-function
  // VS when only a pixel shader is bound.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
      return false;
    }
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
    return false;
  }

  // Bind only a user pixel shader.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop via decl XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    if (!Check(dev->fixedfunc_vs_xyz_tex1 != nullptr, "interop created fixedfunc_vs_xyz_tex1")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_tex1, "interop bound fixedfunc VS (XYZ|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "synthesized VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop via decl XYZ|TEX1)")) {
    return false;
  }

  // Explicit vertex decl input layout must remain bound (no SetFVF internal decl).
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  // Expect a WVP upload for the fixed-function VS (identity columns by default).
  constexpr float kIdentityCols[16] = {
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  bool saw_identity_wvp = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(kIdentityCols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, kIdentityCols, sizeof(kIdentityCols)) == 0) {
      saw_identity_wvp = true;
      break;
    }
  }
  if (!Check(saw_identity_wvp, "PS-only interop (decl XYZ|TEX1) uploaded identity WVP constants")) {
    return false;
  }

  return true;
}

bool TestSetTextureStageStateUpdatesPsForTex1NoDiffuseVertexDeclFvfs() {
  // ---------------------------------------------------------------------------
  // XYZRHW | TEX1 via SetVertexDecl (implied FVF)
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
        {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
        {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
    };

    D3D9DDI_HVERTEXDECL hDecl{};
    HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
        cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
    if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
      return false;
    }
    cleanup.vertex_decls.push_back(hDecl);

    hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
    if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
      return false;
    }

    aerogpu_handle_t decl_handle = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
        return false;
      }
      auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
      if (!Check(decl != nullptr, "vertex decl pointer")) {
        return false;
      }
      decl_handle = decl->handle;
    }
    if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
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

    const VertexXyzrhwTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
    };
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1 via decl)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1 via decl: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZRHW|TEX1 via decl: PS bytecode modulate/texture")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1 via decl: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZRHW|TEX1 via decl: PS bytecode disable->passthrough")) {
        return false;
      }
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1 via decl stage-state update)")) {
      return false;
    }
    // Ensure we never rebound an internal SetFVF decl: the explicit decl handle must
    // remain the active input layout.
    const auto layouts = CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT);
    if (!Check(!layouts.empty(), "SET_INPUT_LAYOUT packets collected")) {
      return false;
    }
    const auto* last_layout = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(layouts.back());
    if (!Check(last_layout->input_layout_handle == decl_handle,
               "XYZRHW|TEX1 via decl: SET_INPUT_LAYOUT uses explicit decl handle")) {
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // XYZ | TEX1 via SetVertexDecl (implied FVF)
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
        {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
        {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
    };

    D3D9DDI_HVERTEXDECL hDecl{};
    HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
        cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
    if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
      return false;
    }
    cleanup.vertex_decls.push_back(hDecl);

    hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
    if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
      return false;
    }

    aerogpu_handle_t decl_handle = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
        return false;
      }
      auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
      if (!Check(decl != nullptr, "vertex decl pointer")) {
        return false;
      }
      decl_handle = decl->handle;
    }
    if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
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

    const VertexXyzTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
    };
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1 via decl)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1 via decl: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsStage0ModulateTexture),
                 "XYZ|TEX1 via decl: PS bytecode modulate/texture")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1 via decl: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(ShaderBytecodeEquals(dev->ps, fixedfunc::kPsPassthroughColor),
                 "XYZ|TEX1 via decl: PS bytecode disable->passthrough")) {
        return false;
      }
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 via decl stage-state update)")) {
      return false;
    }
    const auto layouts = CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT);
    if (!Check(!layouts.empty(), "SET_INPUT_LAYOUT packets collected")) {
      return false;
    }
    const auto* last_layout = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(layouts.back());
    if (!Check(last_layout->input_layout_handle == decl_handle,
               "XYZ|TEX1 via decl: SET_INPUT_LAYOUT uses explicit decl handle")) {
      return false;
    }
  }

  return true;
}

bool TestGetTextureStageStateRoundTrips() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnGetTextureStageState != nullptr, "pfnGetTextureStageState is available")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value) -> HRESULT {
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      return cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    }
    return aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
  };

  uint32_t value = 0;
  HRESULT hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 COLOROP)")) {
    return false;
  }
  if (!Check(value == kD3dTopModulate, "Default stage0 COLOROP=MODULATE")) {
    return false;
  }

  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 ALPHAOP)")) {
    return false;
  }
  if (!Check(value == kD3dTopSelectArg1, "Default stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }

  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/1, kD3dTssColorOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage1 COLOROP)")) {
    return false;
  }
  if (!Check(value == kD3dTopDisable, "Default stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Set + get should round-trip.
  hr = SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage0 ALPHAOP=DISABLE)")) {
    return false;
  }
  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 ALPHAOP) after set")) {
    return false;
  }
  if (!Check(value == kD3dTopDisable, "stage0 ALPHAOP round-trips")) {
    return false;
  }

  // Validate invalid parameters: stage out of range.
  hr = SetTextureStageState(/*stage=*/16, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == D3DERR_INVALIDCALL, "SetTextureStageState(stage=16) returns INVALIDCALL")) {
    return false;
  }
  value = 0xDEADBEEFu;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/16, kD3dTssColorOp, &value);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetTextureStageState(stage=16) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(value == 0u, "GetTextureStageState(stage=16) zeroes output")) {
    return false;
  }

  // Validate invalid parameters: state out of range.
  hr = SetTextureStageState(/*stage=*/0, /*state=*/256, kD3dTopDisable);
  if (!Check(hr == D3DERR_INVALIDCALL, "SetTextureStageState(state=256) returns INVALIDCALL")) {
    return false;
  }
  value = 0xDEADBEEFu;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, /*state=*/256, &value);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetTextureStageState(state=256) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(value == 0u, "GetTextureStageState(state=256) zeroes output")) {
    return false;
  }

  // Validate invalid parameters: null output pointer.
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, nullptr);
  if (!Check(hr == E_INVALIDARG, "GetTextureStageState(pValue=null) returns E_INVALIDARG")) {
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

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      const HRESULT hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      return Check(hr2 == S_OK, msg);
    }
    // Fallback for minimal portable builds that don't expose SetTextureStageState.
    const HRESULT hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    return Check(hr2 == S_OK, msg);
  };

  // Ensure a known starting point for stage0 state (matches D3D9 defaults).
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  const auto DrawTri = [&](const char* tag) -> bool {
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    return Check(hr == S_OK, tag);
  };

  const auto ExpectFixedfuncPs = [&](auto const& expected_bytecode, const char* tag) -> bool {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_ps_tex1 != nullptr, "fixedfunc_ps_tex1 present")) {
      return false;
    }
    if (!Check(dev->ps == dev->fixedfunc_ps_tex1, "fixed-function PS is bound")) {
      return false;
    }
    return Check(ShaderBytecodeEquals(dev->ps, expected_bytecode), tag);
  };

  // Default stage0: COLOR = TEXTURE * DIFFUSE, ALPHA = TEXTURE.
  if (!DrawTri("DrawPrimitiveUP(first)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0ModulateTexture, "fixed-function PS bytecode (modulate/texture)")) {
    return false;
  }

  // Stage0: COLOR = TEXTURE * DIFFUSE, ALPHAOP = DISABLE (alpha from diffuse/current).
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(second)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0ModulateDiffuse, "fixed-function PS bytecode (modulate/diffuse)")) {
    return false;
  }

  // Stage0: COLOR = TEXTURE * DIFFUSE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopModulate, "SetTextureStageState(ALPHAOP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (modulate)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (modulate)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(third)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsTexturedModulateVertexColor,
                         "fixed-function PS bytecode (modulate/modulate)")) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE) (select)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(fourth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0TextureModulate, "fixed-function PS bytecode (texture/modulate)")) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHA = TEXTURE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (select)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(fifth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0TextureTexture, "fixed-function PS bytecode (texture/texture)")) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHAOP = DISABLE (alpha from diffuse/current).
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE) (texture)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(sixth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0TextureDiffuse, "fixed-function PS bytecode (texture/diffuse)")) {
    return false;
  }

  // Stage0: COLOR = DIFFUSE, ALPHA = TEXTURE.
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "SetTextureStageState(COLORARG1=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (diffuse)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (diffuse)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(seventh)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0DiffuseTexture, "fixed-function PS bytecode (diffuse/texture)")) {
    return false;
  }

  // Stage0: COLOR = DIFFUSE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopModulate, "SetTextureStageState(ALPHAOP=MODULATE) (diffuse)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (diffuse modulate)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (diffuse modulate)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(eighth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0DiffuseModulate, "fixed-function PS bytecode (diffuse/modulate)")) {
    return false;
  }

  // Stage0: COLOROP=DISABLE disables the entire stage, so alpha comes from diffuse/current.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopDisable, "SetTextureStageState(COLOROP=DISABLE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (disable)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (disable)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(ninth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsPassthroughColor, "fixed-function PS bytecode (disable -> passthrough)")) {
    return false;
  }

  // Restore default stage0 and ensure the shader rebinds back to texturing.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (restore)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(tenth)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsStage0ModulateTexture, "fixed-function PS bytecode (restore modulate/texture)")) {
    return false;
  }

  // If texture0 is unbound, do not select a texture-sampling shader even when stage0
  // state requests texturing.
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage0=null)")) {
      return false;
    }
  }
  if (!DrawTri("DrawPrimitiveUP(eleventh)")) {
    return false;
  }
  if (!ExpectFixedfuncPs(fixedfunc::kPsPassthroughColor, "fixed-function PS bytecode (no texture -> passthrough)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(stage-state change)")) {
    return false;
  }

  return true;
}

bool TestStage0OpExpansionSelectsShadersAndCaches() {
  struct Case {
    const char* name;
    // Stage0 state.
    uint32_t color_op = kD3dTopSelectArg1;
    uint32_t color_arg1 = kD3dTaDiffuse;
    uint32_t color_arg2 = kD3dTaDiffuse;
    uint32_t alpha_op = kD3dTopSelectArg1;
    uint32_t alpha_arg1 = kD3dTaTexture;
    uint32_t alpha_arg2 = kD3dTaDiffuse;

    // Optional render-state setup.
    bool set_tfactor = false;
    uint32_t tfactor = 0u;
    bool uses_tfactor = false;

    const void* expected_ps_bytes = nullptr;
    size_t expected_ps_size = 0;
  };

  const Case cases[] = {
      // Extended ops (RGB path). Keep ALPHA=TEXTURE so RGB expectations match common D3D9 usage.
      {"add", kD3dTopAdd, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       fixedfunc::kPsStage0AddTextureDiffuseAlphaTexture, sizeof(fixedfunc::kPsStage0AddTextureDiffuseAlphaTexture)},
      {"subtract_tex_minus_diff", kD3dTopSubtract, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       fixedfunc::kPsStage0SubtractTextureDiffuseAlphaTexture, sizeof(fixedfunc::kPsStage0SubtractTextureDiffuseAlphaTexture)},
      {"subtract_diff_minus_tex", kD3dTopSubtract, kD3dTaDiffuse, kD3dTaTexture, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       fixedfunc::kPsStage0SubtractDiffuseTextureAlphaTexture, sizeof(fixedfunc::kPsStage0SubtractDiffuseTextureAlphaTexture)},
      {"modulate2x", kD3dTopModulate2x, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       fixedfunc::kPsStage0Modulate2xTextureDiffuseAlphaTexture, sizeof(fixedfunc::kPsStage0Modulate2xTextureDiffuseAlphaTexture)},
      {"modulate4x", kD3dTopModulate4x, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       fixedfunc::kPsStage0Modulate4xTextureDiffuseAlphaTexture, sizeof(fixedfunc::kPsStage0Modulate4xTextureDiffuseAlphaTexture)},

      // TFACTOR source (select arg1).
      {"tfactor_select", kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse,
       /*set_tfactor=*/true, 0xFF3366CCu, /*uses_tfactor=*/true,
       fixedfunc::kPsStage0TextureFactor, sizeof(fixedfunc::kPsStage0TextureFactor)},
      // Default TFACTOR is white (0xFFFFFFFF). Verify the driver uploads c0 even
      // if the app never explicitly sets D3DRS_TEXTUREFACTOR.
      {"tfactor_default", kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/true,
       fixedfunc::kPsStage0TextureFactor, sizeof(fixedfunc::kPsStage0TextureFactor)},
  };

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  for (const auto& c : cases) {
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

    // Most cases require a bound texture so the stage0 path can sample it.
    // For the TFACTOR-only shader, binding a texture is optional but harmless.
    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    if (c.set_tfactor) {
      hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, c.tfactor);
      if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR)")) {
        return false;
      }
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* name) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      if (hr2 == S_OK) {
        return true;
      }
      std::fprintf(stderr, "FAIL: %s: SetTextureStageState(%s) hr=0x%08x\n", c.name, name, static_cast<unsigned>(hr2));
      return false;
    };

    // Override stage0 state.
    //
    // SetTextureStageState normally updates the stage0 fixed-function PS selection on
    // each call. To avoid creating intermediate PS variants (and emitting extra
    // CREATE_SHADER_DXBC packets), temporarily bind a dummy user PS so the stage0
    // selection hook is suppressed until we're done setting all state.
    {
      const uint8_t dummy_dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x11, 0x22, 0x33, 0x44};
      D3D9DDI_HSHADER hDummyPs{};
      hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                                kD3dShaderStagePs,
                                                dummy_dxbc,
                                                static_cast<uint32_t>(sizeof(dummy_dxbc)),
                                                &hDummyPs);
      if (!Check(hr == S_OK, "CreateShader(dummy PS)")) {
        return false;
      }
      cleanup.shaders.push_back(hDummyPs);

      hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hDummyPs);
      if (!Check(hr == S_OK, "SetShader(PS=dummy)")) {
        return false;
      }

      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, c.color_op, "COLOROP")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, c.color_arg1, "COLORARG1")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg2, c.color_arg2, "COLORARG2")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, c.alpha_op, "ALPHAOP")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, c.alpha_arg1, "ALPHAARG1")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg2, c.alpha_arg2, "ALPHAARG2")) {
        return false;
      }

      D3D9DDI_HSHADER null_shader{};
      hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, null_shader);
      if (!Check(hr == S_OK, "SetShader(PS=NULL)")) {
        return false;
      }
    }

    // Draw twice: the first draw may create/bind the internal fixed-function PS,
    // the second draw should reuse it without re-emitting CREATE_SHADER_DXBC.
    for (int i = 0; i < 2; ++i) {
      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }
    }

    // Validate the bound PS matches the expected variant.
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "PS must be bound")) {
        return false;
      }
      if (!Check(dev->ps->bytecode.size() == c.expected_ps_size, "expected PS bytecode size")) {
        return false;
      }
      if (!Check(std::memcmp(dev->ps->bytecode.data(), c.expected_ps_bytes, c.expected_ps_size) == 0,
                 "expected PS bytecode bytes")) {
        return false;
      }
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(stage0 op expansion)")) {
      return false;
    }

    // Confirm the expected PS bytecode was created at most once.
    size_t create_count = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
      const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
      if (cs->stage != AEROGPU_SHADER_STAGE_PIXEL) {
        continue;
      }
      if (cs->dxbc_size_bytes != c.expected_ps_size) {
        continue;
      }
      const size_t need = sizeof(aerogpu_cmd_create_shader_dxbc) + c.expected_ps_size;
      if (hdr->size_bytes < need) {
        continue;
      }
      const void* payload = reinterpret_cast<const uint8_t*>(cs) + sizeof(aerogpu_cmd_create_shader_dxbc);
      if (std::memcmp(payload, c.expected_ps_bytes, c.expected_ps_size) == 0) {
        ++create_count;
      }
    }
    if (!Check(create_count == 1, "PS variant CREATE_SHADER_DXBC emitted once (cached)")) {
      return false;
    }

    // TFACTOR cases: ensure the PS constant upload was emitted once (c0) and
    // contains the expected normalized RGBA value.
    if (c.uses_tfactor) {
      const uint32_t expected_tf = c.set_tfactor ? c.tfactor : 0xFFFFFFFFu;
      const float expected_a = static_cast<float>((expected_tf >> 24) & 0xFFu) * (1.0f / 255.0f);
      const float expected_r = static_cast<float>((expected_tf >> 16) & 0xFFu) * (1.0f / 255.0f);
      const float expected_g = static_cast<float>((expected_tf >> 8) & 0xFFu) * (1.0f / 255.0f);
      const float expected_b = static_cast<float>((expected_tf >> 0) & 0xFFu) * (1.0f / 255.0f);
      const float expected_vec[4] = {expected_r, expected_g, expected_b, expected_a};

      size_t tfactor_uploads = 0;
      for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
        const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
        if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 0 || sc->vec4_count != 1) {
          continue;
        }
        if (!Check(hdr->size_bytes >= sizeof(*sc) + sizeof(expected_vec), "SET_SHADER_CONSTANTS_F contains payload")) {
          return false;
        }
        const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
        if (!Check(std::fabs(payload[0] - expected_vec[0]) < 1e-6f &&
                       std::fabs(payload[1] - expected_vec[1]) < 1e-6f &&
                       std::fabs(payload[2] - expected_vec[2]) < 1e-6f &&
                       std::fabs(payload[3] - expected_vec[3]) < 1e-6f,
                   "TFACTOR constant payload matches expected RGBA")) {
          return false;
        }
        ++tfactor_uploads;
      }
      if (!Check(tfactor_uploads == 1, "TFACTOR constant upload emitted once (cached)")) {
        return false;
      }
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
  if (!aerogpu::TestFvfXyzDiffuseEmitsInputLayoutAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseEmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseDrawPrimitiveVbCpuTransformsAndBindsScratchVb()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwDiffuseTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1EmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1DrawPrimitiveVbCpuTransformsAndBindsScratchVb()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzTex1EmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzrhwTex1InfersFvfAndBindsShaders()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzTex1InfersFvfAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestSetTextureStageStateUpdatesPsForTex1NoDiffuseFvfs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzrhwTex1SynthesizesVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzTex1SynthesizesVsAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropVertexDeclXyzrhwTex1SynthesizesVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropVertexDeclXyzTex1SynthesizesVsAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestSetTextureStageStateUpdatesPsForTex1NoDiffuseVertexDeclFvfs()) {
    return 1;
  }
  if (!aerogpu::TestGetTextureStageStateRoundTrips()) {
    return 1;
  }
  if (!aerogpu::TestStageStateChangeRebindsShadersIfImplemented()) {
    return 1;
  }
  if (!aerogpu::TestStage0OpExpansionSelectsShadersAndCaches()) {
    return 1;
  }
  return 0;
}
