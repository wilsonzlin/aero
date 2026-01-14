#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_cmd.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

uint32_t F32Bits(float v) {
  uint32_t bits = 0;
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
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
    return 0;
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
  if (!Check(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header), "stream size_bytes >= header")) {
    return false;
  }
  if (!Check(stream->size_bytes <= len, "stream size_bytes within buffer")) {
    return false;
  }

  const size_t stream_len = static_cast<size_t>(stream->size_bytes);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream_len) {
    if (!Check((offset & 3u) == 0, "packet offset 4-byte aligned")) {
      return false;
    }
    if (!Check(stream_len - offset >= sizeof(aerogpu_cmd_hdr), "packet header fits")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= header")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size 4-byte aligned")) {
      return false;
    }
    if (!Check(hdr->size_bytes <= stream_len - offset, "packet size within stream")) {
      return false;
    }
    offset += hdr->size_bytes;
  }
  return Check(offset == stream_len, "packet walk ends at stream_len");
}

CmdLoc FindLastOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  CmdLoc loc{};
  const size_t stream_len = StreamBytesUsed(buf, len);
  if (stream_len == 0) {
    return loc;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return loc;
}

struct Harness {
  std::vector<uint8_t> last_stream;
  std::vector<HRESULT> errors;

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                  const void* cmd_stream,
                                                  uint32_t cmd_stream_size_bytes,
                                                  const AEROGPU_WDDM_SUBMIT_ALLOCATION*,
                                                  uint32_t,
                                                  uint64_t*) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    return S_OK;
  }

  static void AEROGPU_APIENTRY SetError(void* user, HRESULT hr) {
    if (!user) {
      return;
    }
    reinterpret_cast<Harness*>(user)->errors.push_back(hr);
  }
};

struct TestDevice {
  Harness harness;

  D3D10DDI_HADAPTER hAdapter = {};
  D3D10DDI_ADAPTERFUNCS adapter_funcs = {};

  D3D10DDI_HDEVICE hDevice = {};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs = {};
  std::vector<uint8_t> device_mem;

  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks = {};
};

bool InitTestDevice(TestDevice* out) {
  if (!out) {
    return false;
  }

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;

  D3D10DDIARG_OPENADAPTER open = {};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10_2(&open);
  if (!Check(hr == S_OK, "OpenAdapter10_2")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create = {};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned non-trivial size")) {
    return false;
  }

  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = &out->callbacks;

  hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  out->hDevice = create.hDevice;
  return true;
}

struct TestBlendState {
  D3D10DDI_HBLENDSTATE hState = {};
  std::vector<uint8_t> storage;
};

struct TestRasterizerState {
  D3D10DDI_HRASTERIZERSTATE hState = {};
  std::vector<uint8_t> storage;
};

struct TestDepthStencilState {
  D3D10DDI_HDEPTHSTENCILSTATE hState = {};
  std::vector<uint8_t> storage;
};

bool CreateBlendState(TestDevice* dev, const AEROGPU_DDIARG_CREATEBLENDSTATE& desc, TestBlendState* out) {
  if (!dev || !out) {
    return false;
  }
  const SIZE_T size = dev->device_funcs.pfnCalcPrivateBlendStateSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateBlendStateSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hState.pDrvPrivate = out->storage.data();
  const HRESULT hr = dev->device_funcs.pfnCreateBlendState(dev->hDevice, &desc, out->hState);
  return Check(hr == S_OK, "CreateBlendState");
}

bool CreateRasterizerState(TestDevice* dev, const AEROGPU_DDIARG_CREATERASTERIZERSTATE& desc, TestRasterizerState* out) {
  if (!dev || !out) {
    return false;
  }
  const SIZE_T size = dev->device_funcs.pfnCalcPrivateRasterizerStateSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateRasterizerStateSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hState.pDrvPrivate = out->storage.data();
  const HRESULT hr = dev->device_funcs.pfnCreateRasterizerState(dev->hDevice, &desc, out->hState);
  return Check(hr == S_OK, "CreateRasterizerState");
}

bool CreateDepthStencilState(TestDevice* dev,
                             const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE& desc,
                             TestDepthStencilState* out) {
  if (!dev || !out) {
    return false;
  }
  const SIZE_T size = dev->device_funcs.pfnCalcPrivateDepthStencilStateSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateDepthStencilStateSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hState.pDrvPrivate = out->storage.data();
  const HRESULT hr = dev->device_funcs.pfnCreateDepthStencilState(dev->hDevice, &desc, out->hState);
  return Check(hr == S_OK, "CreateDepthStencilState");
}

bool TestSetBlendStateEmitsPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(blend)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.enable = 1;
  desc.src_factor = AEROGPU_BLEND_CONSTANT;
  desc.dst_factor = AEROGPU_BLEND_INV_CONSTANT;
  desc.blend_op = AEROGPU_BLEND_OP_SUBTRACT;
  desc.color_write_mask = 0x3u;
  desc.src_factor_alpha = AEROGPU_BLEND_SRC_ALPHA;
  desc.dst_factor_alpha = AEROGPU_BLEND_INV_SRC_ALPHA;
  desc.blend_op_alpha = AEROGPU_BLEND_OP_ADD;

  TestBlendState bs{};
  if (!Check(CreateBlendState(&dev, desc, &bs), "CreateBlendState helper")) {
    return false;
  }

  const float blend_factor[4] = {0.25f, 0.5f, 0.75f, 1.0f};
  const uint32_t sample_mask = 0x0F0F0F0Fu;
  dev.device_funcs.pfnSetBlendState(dev.hDevice, bs.hState, blend_factor, sample_mask);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream(blend)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.enable == 1u, "blend.enable")) {
    return false;
  }
  if (!Check(cmd->state.src_factor == AEROGPU_BLEND_CONSTANT, "blend.src_factor")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor == AEROGPU_BLEND_INV_CONSTANT, "blend.dst_factor")) {
    return false;
  }
  if (!Check(cmd->state.blend_op == AEROGPU_BLEND_OP_SUBTRACT, "blend.blend_op")) {
    return false;
  }
  if (!Check(cmd->state.color_write_mask == 0x3u, "blend.color_write_mask")) {
    return false;
  }
  if (!Check(cmd->state.src_factor_alpha == AEROGPU_BLEND_SRC_ALPHA, "blend.src_factor_alpha")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor_alpha == AEROGPU_BLEND_INV_SRC_ALPHA, "blend.dst_factor_alpha")) {
    return false;
  }
  if (!Check(cmd->state.blend_op_alpha == AEROGPU_BLEND_OP_ADD, "blend.blend_op_alpha")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == F32Bits(blend_factor[0]), "blend.factor[0]")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[1] == F32Bits(blend_factor[1]), "blend.factor[1]")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[2] == F32Bits(blend_factor[2]), "blend.factor[2]")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[3] == F32Bits(blend_factor[3]), "blend.factor[3]")) {
    return false;
  }
  if (!Check(cmd->state.sample_mask == sample_mask, "blend.sample_mask")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, bs.hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetNullBlendStateEmitsDefaultPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(null blend)")) {
    return false;
  }

  const uint32_t sample_mask = 0x12345678u;
  D3D10DDI_HBLENDSTATE null_state{};
  dev.device_funcs.pfnSetBlendState(dev.hDevice, null_state, /*blend_factor=*/nullptr, sample_mask);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState(null)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(null blend)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted (null)")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.enable == 0u, "blend.enable default")) {
    return false;
  }
  if (!Check(cmd->state.src_factor == AEROGPU_BLEND_ONE, "blend.src_factor default")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor == AEROGPU_BLEND_ZERO, "blend.dst_factor default")) {
    return false;
  }
  if (!Check(cmd->state.blend_op == AEROGPU_BLEND_OP_ADD, "blend.blend_op default")) {
    return false;
  }
  if (!Check(cmd->state.src_factor_alpha == AEROGPU_BLEND_ONE, "blend.src_factor_alpha default")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor_alpha == AEROGPU_BLEND_ZERO, "blend.dst_factor_alpha default")) {
    return false;
  }
  if (!Check(cmd->state.blend_op_alpha == AEROGPU_BLEND_OP_ADD, "blend.blend_op_alpha default")) {
    return false;
  }
  if (!Check(cmd->state.color_write_mask == 0xFu, "blend.color_write_mask default")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == F32Bits(1.0f), "blend.constant[0] default")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[1] == F32Bits(1.0f), "blend.constant[1] default")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[2] == F32Bits(1.0f), "blend.constant[2] default")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[3] == F32Bits(1.0f), "blend.constant[3] default")) {
    return false;
  }
  if (!Check(cmd->state.sample_mask == sample_mask, "blend.sample_mask default")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetNullBlendStateUsesProvidedBlendFactor() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(null blend factor)")) {
    return false;
  }

  const float blend_factor[4] = {0.125f, 0.25f, 0.5f, 0.75f};
  const uint32_t sample_mask = 0x76543210u;
  D3D10DDI_HBLENDSTATE null_state{};
  dev.device_funcs.pfnSetBlendState(dev.hDevice, null_state, blend_factor, sample_mask);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState(null, blend_factor)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(null blend factor)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted (null, blend_factor)")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.enable == 0u, "blend.enable default (null, blend_factor)")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == F32Bits(blend_factor[0]), "blend.constant[0] override")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[1] == F32Bits(blend_factor[1]), "blend.constant[1] override")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[2] == F32Bits(blend_factor[2]), "blend.constant[2] override")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[3] == F32Bits(blend_factor[3]), "blend.constant[3] override")) {
    return false;
  }
  if (!Check(cmd->state.sample_mask == sample_mask, "blend.sample_mask override")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetBlendStateNullBlendFactorDefaultsToOnes() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(blend null factor)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.enable = 1;
  desc.src_factor = AEROGPU_BLEND_CONSTANT;
  desc.dst_factor = AEROGPU_BLEND_INV_CONSTANT;
  desc.blend_op = AEROGPU_BLEND_OP_SUBTRACT;
  desc.color_write_mask = 0x3u;
  desc.src_factor_alpha = AEROGPU_BLEND_SRC_ALPHA;
  desc.dst_factor_alpha = AEROGPU_BLEND_INV_SRC_ALPHA;
  desc.blend_op_alpha = AEROGPU_BLEND_OP_ADD;

  TestBlendState bs{};
  if (!Check(CreateBlendState(&dev, desc, &bs), "CreateBlendState helper (null blend factor)")) {
    return false;
  }

  const float first_factor[4] = {0.25f, 0.5f, 0.75f, 0.125f};
  const uint32_t sample_mask = 0x0F0F0F0Fu;
  dev.device_funcs.pfnSetBlendState(dev.hDevice, bs.hState, first_factor, sample_mask);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush after SetBlendState(initial factor)")) {
    return false;
  }

  // Passing a null blend_factor should reset it to {1,1,1,1}.
  dev.device_funcs.pfnSetBlendState(dev.hDevice, bs.hState, /*blend_factor=*/nullptr, sample_mask);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState(blend_factor=null)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(blend_factor=null)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted (blend_factor=null)")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == F32Bits(1.0f), "blend.constant[0] null")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[1] == F32Bits(1.0f), "blend.constant[1] null")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[2] == F32Bits(1.0f), "blend.constant[2] null")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[3] == F32Bits(1.0f), "blend.constant[3] null")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, bs.hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateRasterizerStateRejectsUnsupportedFillMode() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(rs fill mode)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATERASTERIZERSTATE desc = {};
  // Invalid: `fill_mode` is `enum aerogpu_fill_mode` (0..1).
  desc.fill_mode = AEROGPU_FILL_WIREFRAME + 1u;
  desc.cull_mode = AEROGPU_CULL_BACK;
  desc.front_ccw = 0;
  desc.scissor_enable = 0;
  desc.depth_bias = 0;
  desc.depth_clip_enable = 1;

  D3D10DDI_HRASTERIZERSTATE hState = {};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateRasterizerStateSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateRasterizerStateSize returned non-trivial size (invalid FillMode)")) {
    return false;
  }
  std::vector<uint8_t> storage(static_cast<size_t>(size), 0);
  hState.pDrvPrivate = storage.data();

  const HRESULT hr = dev.device_funcs.pfnCreateRasterizerState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateRasterizerState should return E_INVALIDARG for invalid fill_mode")) {
    return false;
  }

  // Destroy should be safe even after a failed create.
  dev.device_funcs.pfnDestroyRasterizerState(dev.hDevice, hState);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateRasterizerStateRejectsUnsupportedCullMode() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(rs cull mode)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATERASTERIZERSTATE desc = {};
  // Invalid: `cull_mode` is `enum aerogpu_cull_mode` (0..2).
  desc.fill_mode = AEROGPU_FILL_WIREFRAME;
  desc.cull_mode = AEROGPU_CULL_BACK + 1u;
  desc.front_ccw = 0;
  desc.scissor_enable = 0;
  desc.depth_bias = 0;
  desc.depth_clip_enable = 1;

  D3D10DDI_HRASTERIZERSTATE hState = {};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateRasterizerStateSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateRasterizerStateSize returned non-trivial size (invalid cull_mode)")) {
    return false;
  }
  std::vector<uint8_t> storage(static_cast<size_t>(size), 0);
  hState.pDrvPrivate = storage.data();

  const HRESULT hr = dev.device_funcs.pfnCreateRasterizerState(dev.hDevice, &desc, hState);
  if (!Check(hr == E_INVALIDARG, "CreateRasterizerState should return E_INVALIDARG for invalid cull_mode")) {
    return false;
  }

  // Destroy should be safe even after a failed create.
  dev.device_funcs.pfnDestroyRasterizerState(dev.hDevice, hState);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRasterizerStateEmitsPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(rasterizer)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATERASTERIZERSTATE desc = {};
  desc.fill_mode = AEROGPU_FILL_WIREFRAME;
  desc.cull_mode = AEROGPU_CULL_FRONT;
  desc.front_ccw = 1;
  desc.scissor_enable = 1;
  desc.depth_bias = -5;
  desc.depth_clip_enable = 0;

  TestRasterizerState rs{};
  if (!Check(CreateRasterizerState(&dev, desc, &rs), "CreateRasterizerState helper")) {
    return false;
  }

  dev.device_funcs.pfnSetRasterizerState(dev.hDevice, rs.hState);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetRasterizerState")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(rasterizer)")) {
    return false;
  }

  CmdLoc loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!Check(loc.hdr != nullptr, "SET_RASTERIZER_STATE emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_rasterizer_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.fill_mode == AEROGPU_FILL_WIREFRAME, "raster.fill_mode")) {
    return false;
  }
  if (!Check(cmd->state.cull_mode == AEROGPU_CULL_FRONT, "raster.cull_mode")) {
    return false;
  }
  if (!Check(cmd->state.front_ccw == 1u, "raster.front_ccw")) {
    return false;
  }
  if (!Check(cmd->state.scissor_enable == 1u, "raster.scissor_enable")) {
    return false;
  }
  if (!Check(cmd->state.depth_bias == -5, "raster.depth_bias")) {
    return false;
  }
  if (!Check((cmd->state.flags & AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE) != 0, "raster.depth_clip_disable flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyRasterizerState(dev.hDevice, rs.hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetNullRasterizerStateEmitsDefaultPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(null rasterizer)")) {
    return false;
  }

  D3D10DDI_HRASTERIZERSTATE null_state{};
  dev.device_funcs.pfnSetRasterizerState(dev.hDevice, null_state);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetRasterizerState(null)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(null rasterizer)")) {
    return false;
  }

  CmdLoc loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!Check(loc.hdr != nullptr, "SET_RASTERIZER_STATE emitted (null)")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_rasterizer_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.fill_mode == AEROGPU_FILL_SOLID, "raster.fill_mode default")) {
    return false;
  }
  if (!Check(cmd->state.cull_mode == AEROGPU_CULL_BACK, "raster.cull_mode default")) {
    return false;
  }
  if (!Check(cmd->state.front_ccw == 0u, "raster.front_ccw default")) {
    return false;
  }
  if (!Check(cmd->state.scissor_enable == 0u, "raster.scissor_enable default")) {
    return false;
  }
  if (!Check(cmd->state.depth_bias == 0, "raster.depth_bias default")) {
    return false;
  }
  if (!Check(cmd->state.flags == AEROGPU_RASTERIZER_FLAG_NONE, "raster.flags default")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}
 
bool TestDepthDisableDisablesDepthWrites() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(depth disable)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE desc = {};
  desc.depth_enable = 0;
  // D3D10/11 semantics: depth writes are ignored when depth testing is disabled.
  desc.depth_write_enable = 1;
  desc.depth_func = AEROGPU_COMPARE_GREATER_EQUAL;
  desc.stencil_enable = 0;
  desc.stencil_read_mask = 0xFFu;
  desc.stencil_write_mask = 0xFFu;

  TestDepthStencilState dss{};
  if (!Check(CreateDepthStencilState(&dev, desc, &dss), "CreateDepthStencilState helper (depth disable)")) {
    return false;
  }

  dev.device_funcs.pfnSetDepthStencilState(dev.hDevice, dss.hState, /*stencil_ref=*/0u);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetDepthStencilState(depth disabled)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(depth disabled)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(),
                              dev.harness.last_stream.size(),
                              AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!Check(loc.hdr != nullptr, "SET_DEPTH_STENCIL_STATE emitted (depth disabled)")) {
    return false;
  }

  const auto* cmd =
      reinterpret_cast<const aerogpu_cmd_set_depth_stencil_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.depth_enable == 0u, "dss.depth_enable == 0")) {
    return false;
  }
  if (!Check(cmd->state.depth_write_enable == 0u, "dss.depth_write_enable forced 0 when depth disabled")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDepthStencilState(dev.hDevice, dss.hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetDepthStencilStateEmitsPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(depth-stencil)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE desc = {};
  desc.depth_enable = 1;
  desc.depth_write_enable = 0;
  desc.depth_func = AEROGPU_COMPARE_GREATER_EQUAL;
  desc.stencil_enable = 1;
  desc.stencil_read_mask = 0x0Fu;
  desc.stencil_write_mask = 0xF0u;

  TestDepthStencilState dss{};
  if (!Check(CreateDepthStencilState(&dev, desc, &dss), "CreateDepthStencilState helper")) {
    return false;
  }

  dev.device_funcs.pfnSetDepthStencilState(dev.hDevice, dss.hState, /*stencil_ref=*/123u);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetDepthStencilState")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(depth-stencil)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(),
                              dev.harness.last_stream.size(),
                              AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!Check(loc.hdr != nullptr, "SET_DEPTH_STENCIL_STATE emitted")) {
    return false;
  }
  const auto* cmd =
      reinterpret_cast<const aerogpu_cmd_set_depth_stencil_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.depth_enable == 1u, "dss.depth_enable")) {
    return false;
  }
  if (!Check(cmd->state.depth_write_enable == 0u, "dss.depth_write_enable")) {
    return false;
  }
  if (!Check(cmd->state.depth_func == AEROGPU_COMPARE_GREATER_EQUAL, "dss.depth_func")) {
    return false;
  }
  if (!Check(cmd->state.stencil_enable == 1u, "dss.stencil_enable")) {
    return false;
  }
  if (!Check(cmd->state.stencil_read_mask == 0x0Fu, "dss.stencil_read_mask")) {
    return false;
  }
  if (!Check(cmd->state.stencil_write_mask == 0xF0u, "dss.stencil_write_mask")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDepthStencilState(dev.hDevice, dss.hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetNullDepthStencilStateEmitsDefaultPacket() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(null depth-stencil)")) {
    return false;
  }

  D3D10DDI_HDEPTHSTENCILSTATE null_state{};
  dev.device_funcs.pfnSetDepthStencilState(dev.hDevice, null_state, /*stencil_ref=*/0u);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetDepthStencilState(null)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()),
             "ValidateStream(null depth-stencil)")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(),
                              dev.harness.last_stream.size(),
                              AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!Check(loc.hdr != nullptr, "SET_DEPTH_STENCIL_STATE emitted (null)")) {
    return false;
  }
  const auto* cmd =
      reinterpret_cast<const aerogpu_cmd_set_depth_stencil_state*>(dev.harness.last_stream.data() + loc.offset);
  if (!Check(cmd->state.depth_enable == 1u, "dss.depth_enable default")) {
    return false;
  }
  if (!Check(cmd->state.depth_write_enable == 1u, "dss.depth_write_enable default")) {
    return false;
  }
  if (!Check(cmd->state.depth_func == AEROGPU_COMPARE_LESS, "dss.depth_func default")) {
    return false;
  }
  if (!Check(cmd->state.stencil_enable == 0u, "dss.stencil_enable default")) {
    return false;
  }
  if (!Check(cmd->state.stencil_read_mask == 0xFFu, "dss.stencil_read_mask default")) {
    return false;
  }
  if (!Check(cmd->state.stencil_write_mask == 0xFFu, "dss.stencil_write_mask default")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestSetBlendStateEmitsPacket();
  ok &= TestSetNullBlendStateEmitsDefaultPacket();
  ok &= TestSetNullBlendStateUsesProvidedBlendFactor();
  ok &= TestSetBlendStateNullBlendFactorDefaultsToOnes();
  ok &= TestCreateRasterizerStateRejectsUnsupportedFillMode();
  ok &= TestCreateRasterizerStateRejectsUnsupportedCullMode();
  ok &= TestSetRasterizerStateEmitsPacket();
  ok &= TestSetNullRasterizerStateEmitsDefaultPacket();
  ok &= TestDepthDisableDisablesDepthWrites();
  ok &= TestSetDepthStencilStateEmitsPacket();
  ok &= TestSetNullDepthStencilStateEmitsDefaultPacket();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_state_packets_tests\n");
  return 0;
}
