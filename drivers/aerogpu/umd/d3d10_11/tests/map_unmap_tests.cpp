#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <thread>
#include <utility>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_blend_state_validate.h"
#include "aerogpu_cmd.h"

namespace {

using aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm;
using aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc1Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc1UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc2Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc2UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc3Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc3UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc7Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc7UnormSrgb;

using aerogpu::d3d10_11::kD3D11BindVertexBuffer;
using aerogpu::d3d10_11::kD3D11BindIndexBuffer;
using aerogpu::d3d10_11::kD3D11BindConstantBuffer;
using aerogpu::d3d10_11::kD3D11BindShaderResource;
using aerogpu::d3d10_11::kD3D11BindUnorderedAccess;
using aerogpu::d3d10_11::kD3D11BindRenderTarget;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool TestInternalDxgiFormatCompatHelpers() {
  using aerogpu::d3d10_11::Adapter;
  Adapter adapter{};
  adapter.umd_private_valid = false;

  uint32_t fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(&adapter, kDxgiFormatB8G8R8A8UnormSrgb);
  if (!Check(fmt == AEROGPU_FORMAT_B8G8R8A8_UNORM, "dxgi_format_to_aerogpu_compat maps sRGB->UNORM when sRGB unsupported")) {
    return false;
  }

  adapter.umd_private_valid = true;
  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 2; // ABI 1.2
  fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(&adapter, kDxgiFormatB8G8R8A8UnormSrgb);
  if (!Check(fmt == AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB, "dxgi_format_to_aerogpu_compat preserves sRGB when supported")) {
    return false;
  }

  adapter.umd_private.device_features = 0;
  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 1; // ABI 1.1
  if (!Check(!aerogpu::d3d10_11::SupportsTransfer(&adapter), "SupportsTransfer requires FEATURE_TRANSFER bit")) {
    return false;
  }

  adapter.umd_private.device_features = AEROGPU_UMDPRIV_FEATURE_TRANSFER;
  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 0; // ABI 1.0
  if (!Check(!aerogpu::d3d10_11::SupportsTransfer(&adapter), "SupportsTransfer requires ABI >= 1.1")) {
    return false;
  }

  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 1; // ABI 1.1
  if (!Check(aerogpu::d3d10_11::SupportsTransfer(&adapter), "SupportsTransfer true with FEATURE_TRANSFER + ABI >= 1.1")) {
    return false;
  }

  struct DummyDev {
    Adapter* adapter;
  };
  DummyDev dev{&adapter};
  if (!Check(aerogpu::d3d10_11::SupportsTransfer(&dev), "SupportsTransfer works when passed a device with ->adapter")) {
    return false;
  }

  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 1; // ABI 1.1
  if (!Check(!aerogpu::d3d10_11::SupportsBcFormats(&dev), "SupportsBcFormats requires ABI >= 1.2")) {
    return false;
  }
  adapter.umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 2; // ABI 1.2
  if (!Check(aerogpu::d3d10_11::SupportsBcFormats(&dev), "SupportsBcFormats true when ABI >= 1.2")) {
    return false;
  }
  if (!Check(aerogpu::d3d10_11::SupportsSrgbFormats(&dev), "SupportsSrgbFormats true when ABI >= 1.2")) {
    return false;
  }

  if (!Check(aerogpu::d3d10_11::aerogpu_sampler_filter_from_d3d_filter(0) == AEROGPU_SAMPLER_FILTER_NEAREST,
             "aerogpu_sampler_filter_from_d3d_filter maps MIN_MAG_MIP_POINT -> NEAREST")) {
    return false;
  }
  if (!Check(aerogpu::d3d10_11::aerogpu_sampler_filter_from_d3d_filter(0x15) == AEROGPU_SAMPLER_FILTER_LINEAR,
             "aerogpu_sampler_filter_from_d3d_filter maps non-zero filters -> LINEAR")) {
    return false;
  }
  if (!Check(aerogpu::d3d10_11::aerogpu_sampler_address_from_d3d_mode(1) == AEROGPU_SAMPLER_ADDRESS_REPEAT,
             "aerogpu_sampler_address_from_d3d_mode maps WRAP -> REPEAT")) {
    return false;
  }
  if (!Check(aerogpu::d3d10_11::aerogpu_sampler_address_from_d3d_mode(2) == AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT,
             "aerogpu_sampler_address_from_d3d_mode maps MIRROR -> MIRROR_REPEAT")) {
    return false;
  }
  if (!Check(aerogpu::d3d10_11::aerogpu_sampler_address_from_d3d_mode(3) == AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE,
             "aerogpu_sampler_address_from_d3d_mode maps CLAMP -> CLAMP_TO_EDGE")) {
    return false;
  }

  return true;
}

bool TestViewportHelperCachesDimsOnlyWhenEnabledForD3D10StyleDevice() {
  struct D3D10StyleDevice {
    aerogpu::CmdWriter cmd;
    uint32_t viewport_width = 111;
    uint32_t viewport_height = 222;

    D3D10StyleDevice() {
      cmd.reset();
    }
  };

  using aerogpu::d3d10_11::f32_bits;
  using aerogpu::d3d10_11::validate_and_emit_viewports_locked;

  D3D10StyleDevice dev{};
  std::vector<HRESULT> errors;

  // Disabled viewport should not clobber cached dimensions.
  dev.cmd.reset();
  errors.clear();
  AEROGPU_DDI_VIEWPORT vp_disabled{};
  vp_disabled.TopLeftX = 0.0f;
  vp_disabled.TopLeftY = 0.0f;
  vp_disabled.Width = 0.0f;
  vp_disabled.Height = 0.0f;
  vp_disabled.MinDepth = 0.0f;
  vp_disabled.MaxDepth = 1.0f;
  validate_and_emit_viewports_locked(&dev, /*num_viewports=*/1, &vp_disabled, [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "disabled viewport should not report an error")) {
    return false;
  }
  if (!Check(dev.viewport_width == 111 && dev.viewport_height == 222,
             "disabled viewport should not update cached viewport_width/height")) {
    return false;
  }
  if (!Check(dev.cmd.size() >= sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_set_viewport),
             "disabled viewport emits SET_VIEWPORT packet")) {
    return false;
  }
  const auto* disabled_pkt = reinterpret_cast<const aerogpu_cmd_set_viewport*>(
      dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
  if (!Check(disabled_pkt->hdr.opcode == AEROGPU_CMD_SET_VIEWPORT, "disabled viewport packet opcode")) {
    return false;
  }
  if (!Check(disabled_pkt->width_f32 == f32_bits(0.0f) && disabled_pkt->height_f32 == f32_bits(0.0f),
             "disabled viewport encodes 0 width/height")) {
    return false;
  }

  // Enabled viewport should update cached dimensions.
  dev.cmd.reset();
  errors.clear();
  AEROGPU_DDI_VIEWPORT vp_enabled = vp_disabled;
  vp_enabled.Width = 640.0f;
  vp_enabled.Height = 480.0f;
  validate_and_emit_viewports_locked(&dev, /*num_viewports=*/1, &vp_enabled, [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "enabled viewport should not report an error")) {
    return false;
  }
  if (!Check(dev.viewport_width == 640 && dev.viewport_height == 480,
             "enabled viewport should update cached viewport_width/height")) {
    return false;
  }
  const auto* enabled_pkt = reinterpret_cast<const aerogpu_cmd_set_viewport*>(
      dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
  if (!Check(enabled_pkt->hdr.opcode == AEROGPU_CMD_SET_VIEWPORT, "enabled viewport packet opcode")) {
    return false;
  }
  if (!Check(enabled_pkt->width_f32 == f32_bits(640.0f) && enabled_pkt->height_f32 == f32_bits(480.0f),
             "enabled viewport encodes width/height")) {
    return false;
  }

  // Reset should clear cached dimensions.
  dev.cmd.reset();
  errors.clear();
  validate_and_emit_viewports_locked(&dev,
                                     /*num_viewports=*/0,
                                     static_cast<const AEROGPU_DDI_VIEWPORT*>(nullptr),
                                     [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();
  if (!Check(errors.empty(), "viewport reset should not report an error")) {
    return false;
  }
  if (!Check(dev.viewport_width == 0 && dev.viewport_height == 0, "viewport reset clears cached viewport_width/height")) {
    return false;
  }

  return true;
}

bool TestViewportScissorHelpersDontReportNotImplWhenCmdAppendFails() {
  using aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked;
  using aerogpu::d3d10_11::validate_and_emit_viewports_locked;

  struct TinyCmdDevice {
    aerogpu::CmdWriter cmd;

    explicit TinyCmdDevice(uint8_t* buf, size_t cap) {
      cmd.set_span(buf, cap);
    }
  };

  std::vector<HRESULT> errors;

  // Provide enough space for the stream header but not enough space for any
  // subsequent packets, so append_fixed will fail.
  alignas(4) uint8_t tiny_buf[sizeof(aerogpu_cmd_stream_header)] = {};

  // Viewports: unsupported multi-viewport usage should *not* report E_NOTIMPL if
  // the packet cannot be encoded due to insufficient space.
  {
    TinyCmdDevice dev(tiny_buf, sizeof(tiny_buf));
    errors.clear();
    const AEROGPU_DDI_VIEWPORT vps[2] = {
        AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/0.0f, /*TopLeftY=*/0.0f, /*Width=*/1.0f, /*Height=*/1.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
        AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/1.0f, /*TopLeftY=*/2.0f, /*Width=*/3.0f, /*Height=*/4.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
    };
    validate_and_emit_viewports_locked(&dev, /*num_viewports=*/2, vps, [&](HRESULT hr) { errors.push_back(hr); });
    if (!Check(errors.size() == 1 && errors[0] == E_OUTOFMEMORY,
               "multi-viewport OOM reports only E_OUTOFMEMORY (no E_NOTIMPL)")) {
      return false;
    }
    if (!Check(dev.cmd.size() == sizeof(aerogpu_cmd_stream_header), "OOM prevents viewport packet emission")) {
      return false;
    }
  }

  // Scissor rects: same behavior.
  {
    TinyCmdDevice dev(tiny_buf, sizeof(tiny_buf));
    errors.clear();
    const AEROGPU_DDI_RECT rects[2] = {
        AEROGPU_DDI_RECT{/*left=*/0, /*top=*/0, /*right=*/1, /*bottom=*/1},
        AEROGPU_DDI_RECT{/*left=*/10, /*top=*/20, /*right=*/30, /*bottom=*/40},
    };
    validate_and_emit_scissor_rects_locked(&dev, /*num_rects=*/2, rects, [&](HRESULT hr) { errors.push_back(hr); });
    if (!Check(errors.size() == 1 && errors[0] == E_OUTOFMEMORY,
               "multi-scissor OOM reports only E_OUTOFMEMORY (no E_NOTIMPL)")) {
      return false;
    }
    if (!Check(dev.cmd.size() == sizeof(aerogpu_cmd_stream_header), "OOM prevents scissor packet emission")) {
      return false;
    }
  }

  return true;
}

bool TestRenderTargetHelpersClearStaleDsvHandles() {
  using aerogpu::d3d10_11::EmitSetRenderTargetsLocked;
  using aerogpu::d3d10_11::UnbindResourceFromOutputsLocked;
  using aerogpu::d3d10_11::Resource;

  struct DummyDevice {
    aerogpu::CmdWriter cmd;
    uint32_t current_rtv_count = 0;
    aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
    Resource* current_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
    aerogpu_handle_t current_dsv = 0;
    Resource* current_dsv_res = nullptr;

    DummyDevice() {
      cmd.reset();
    }
  };

  std::vector<HRESULT> errors;

  // EmitSetRenderTargetsLocked should normalize a stale DSV handle to 0 when the
  // cached resource pointer is null.
  {
    DummyDevice dev{};
    dev.current_dsv = 1234;
    dev.current_dsv_res = nullptr;

    const bool ok = EmitSetRenderTargetsLocked(&dev, [&](HRESULT hr) { errors.push_back(hr); });
    if (!Check(ok, "EmitSetRenderTargetsLocked should succeed")) {
      return false;
    }
    dev.cmd.finalize();

    if (!Check(errors.empty(), "EmitSetRenderTargetsLocked should not report errors")) {
      return false;
    }
    if (!Check(dev.current_dsv == 0, "NormalizeRenderTargetsLocked clears stale current_dsv when current_dsv_res is null")) {
      return false;
    }

    if (!Check(dev.cmd.size() >= sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_set_render_targets),
               "SET_RENDER_TARGETS packet emitted")) {
      return false;
    }
    const auto* pkt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(
        dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
    if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_RENDER_TARGETS, "SET_RENDER_TARGETS opcode")) {
      return false;
    }
    if (!Check(pkt->depth_stencil == 0, "SET_RENDER_TARGETS depth_stencil normalized to 0")) {
      return false;
    }
  }

  errors.clear();
  // UnbindResourceFromOutputsLocked should also clear stale DSV handles when it
  // has to re-emit the OM binding due to an RTV change.
  {
    DummyDevice dev{};
    Resource rtv_res{};
    dev.current_rtv_count = 1;
    dev.current_rtvs[0] = 111;
    dev.current_rtv_resources[0] = &rtv_res;
    dev.current_dsv = 222;
    dev.current_dsv_res = nullptr; // stale

    const bool ok = UnbindResourceFromOutputsLocked(&dev,
                                                    /*handle=*/111,
                                                    static_cast<const Resource*>(nullptr),
                                                    [&](HRESULT hr) { errors.push_back(hr); });
    if (!Check(ok, "UnbindResourceFromOutputsLocked should succeed")) {
      return false;
    }
    dev.cmd.finalize();

    if (!Check(errors.empty(), "UnbindResourceFromOutputsLocked should not report errors")) {
      return false;
    }
    if (!Check(dev.current_dsv == 0, "UnbindResourceFromOutputsLocked clears stale current_dsv when current_dsv_res is null")) {
      return false;
    }

    const auto* pkt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(
        dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
    if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_RENDER_TARGETS, "SET_RENDER_TARGETS opcode (unbind)")) {
      return false;
    }
    if (!Check(pkt->color_count == 1, "color_count preserved when unbinding RTV slot 0")) {
      return false;
    }
    if (!Check(pkt->colors[0] == 0, "RTV slot 0 unbound")) {
      return false;
    }
    if (!Check(pkt->depth_stencil == 0, "depth_stencil normalized to 0 on unbind emit")) {
      return false;
    }
  }

  return true;
}

bool TestPrimitiveTopologyHelperEmitsAndCaches() {
  using aerogpu::d3d10_11::SetPrimitiveTopologyLocked;

  struct DummyDevice {
    aerogpu::CmdWriter cmd;
    uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

    DummyDevice() {
      cmd.reset();
    }
  };

  DummyDevice dev{};
  std::vector<HRESULT> errors;

  // Setting the default topology again should be a no-op (no packet emission).
  if (!Check(SetPrimitiveTopologyLocked(&dev,
                                        AEROGPU_TOPOLOGY_TRIANGLELIST,
                                        [&](HRESULT hr) { errors.push_back(hr); }),
             "SetPrimitiveTopologyLocked(default) should succeed")) {
    return false;
  }
  if (!Check(errors.empty(), "SetPrimitiveTopologyLocked(default) should not report errors")) {
    return false;
  }
  if (!Check(dev.cmd.size() == sizeof(aerogpu_cmd_stream_header), "default topology does not emit a packet")) {
    return false;
  }

  // Changing topology should emit a packet and update cached state.
  if (!Check(SetPrimitiveTopologyLocked(&dev,
                                        AEROGPU_TOPOLOGY_LINELIST,
                                        [&](HRESULT hr) { errors.push_back(hr); }),
             "SetPrimitiveTopologyLocked(linelist) should succeed")) {
    return false;
  }
  dev.cmd.finalize();
  if (!Check(dev.current_topology == AEROGPU_TOPOLOGY_LINELIST, "current_topology updated")) {
    return false;
  }
  if (!Check(dev.cmd.size() >= sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_set_primitive_topology),
             "linelist emits SET_PRIMITIVE_TOPOLOGY packet")) {
    return false;
  }
  const auto* pkt = reinterpret_cast<const aerogpu_cmd_set_primitive_topology*>(
      dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
  if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY, "packet opcode")) {
    return false;
  }
  if (!Check(pkt->topology == AEROGPU_TOPOLOGY_LINELIST, "packet topology payload")) {
    return false;
  }

  // Re-applying the same topology should not append another packet.
  const size_t bytes_before = dev.cmd.size();
  if (!Check(SetPrimitiveTopologyLocked(&dev,
                                        AEROGPU_TOPOLOGY_LINELIST,
                                        [&](HRESULT hr) { errors.push_back(hr); }),
             "SetPrimitiveTopologyLocked(linelist again) should succeed")) {
    return false;
  }
  if (!Check(dev.cmd.size() == bytes_before, "re-applying same topology is a no-op")) {
    return false;
  }

  // OOM/insufficient-space should not update cached topology.
  alignas(4) uint8_t tiny_buf[sizeof(aerogpu_cmd_stream_header)] = {};
  struct TinyDevice {
    aerogpu::CmdWriter cmd;
    uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
    TinyDevice(uint8_t* buf, size_t cap) {
      cmd.set_span(buf, cap);
    }
  };

  TinyDevice tiny(tiny_buf, sizeof(tiny_buf));
  errors.clear();
  const bool ok = SetPrimitiveTopologyLocked(&tiny,
                                            AEROGPU_TOPOLOGY_TRIANGLESTRIP,
                                            [&](HRESULT hr) { errors.push_back(hr); });
  if (!Check(!ok, "SetPrimitiveTopologyLocked should fail when cmd append fails")) {
    return false;
  }
  if (!Check(errors.size() == 1 && errors[0] == E_OUTOFMEMORY, "cmd append failure reports E_OUTOFMEMORY")) {
    return false;
  }
  if (!Check(tiny.current_topology == AEROGPU_TOPOLOGY_TRIANGLELIST, "cached topology not updated on failure")) {
    return false;
  }

  return true;
}

bool TestSetTextureHelperEncodesPacket() {
  using aerogpu::d3d10_11::EmitSetTextureCmdLocked;

  struct DummyDevice {
    aerogpu::CmdWriter cmd;

    DummyDevice() {
      cmd.reset();
    }
  };

  std::vector<HRESULT> errors;
  DummyDevice dev{};
  const bool ok = EmitSetTextureCmdLocked(&dev,
                                         AEROGPU_SHADER_STAGE_VERTEX,
                                         /*slot=*/3,
                                         /*texture=*/static_cast<aerogpu_handle_t>(42),
                                         [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(ok, "EmitSetTextureCmdLocked should succeed")) {
    return false;
  }
  if (!Check(errors.empty(), "EmitSetTextureCmdLocked should not report errors")) {
    return false;
  }
  if (!Check(dev.cmd.size() >= sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_set_texture),
             "SET_TEXTURE packet emitted")) {
    return false;
  }
  const auto* pkt =
      reinterpret_cast<const aerogpu_cmd_set_texture*>(dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
  if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_TEXTURE, "SET_TEXTURE opcode")) {
    return false;
  }
  if (!Check(pkt->shader_stage == AEROGPU_SHADER_STAGE_VERTEX, "SET_TEXTURE shader_stage")) {
    return false;
  }
  if (!Check(pkt->slot == 3, "SET_TEXTURE slot")) {
    return false;
  }
  if (!Check(pkt->texture == 42, "SET_TEXTURE texture")) {
    return false;
  }
  if (!Check(pkt->reserved0 == 0, "SET_TEXTURE reserved0 cleared")) {
    return false;
  }

  // Insufficient-space path.
  alignas(4) uint8_t tiny_buf[sizeof(aerogpu_cmd_stream_header)] = {};
  struct TinyDevice {
    aerogpu::CmdWriter cmd;
    TinyDevice(uint8_t* buf, size_t cap) {
      cmd.set_span(buf, cap);
    }
  };
  TinyDevice tiny(tiny_buf, sizeof(tiny_buf));
  errors.clear();
  const bool ok2 = EmitSetTextureCmdLocked(&tiny,
                                          AEROGPU_SHADER_STAGE_PIXEL,
                                          /*slot=*/0,
                                          /*texture=*/static_cast<aerogpu_handle_t>(1),
                                          [&](HRESULT hr) { errors.push_back(hr); });
  if (!Check(!ok2, "EmitSetTextureCmdLocked should fail when cmd append fails")) {
    return false;
  }
  if (!Check(errors.size() == 1 && errors[0] == E_OUTOFMEMORY, "cmd append failure reports E_OUTOFMEMORY")) {
    return false;
  }

  return true;
}

bool TestSetSamplersHelperEncodesPacket() {
  using aerogpu::d3d10_11::EmitSetSamplersCmdLocked;

  struct DummyDevice {
    aerogpu::CmdWriter cmd;

    DummyDevice() {
      cmd.reset();
    }
  };

  std::vector<HRESULT> errors;

  // Happy path.
  DummyDevice dev{};
  const aerogpu_handle_t handles[3] = {11, 22, 33};
  const bool ok = EmitSetSamplersCmdLocked(&dev,
                                          AEROGPU_SHADER_STAGE_PIXEL,
                                          /*start_slot=*/4,
                                          /*sampler_count=*/3,
                                          handles,
                                          [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(ok, "EmitSetSamplersCmdLocked should succeed")) {
    return false;
  }
  if (!Check(errors.empty(), "EmitSetSamplersCmdLocked should not report errors")) {
    return false;
  }

  const uint32_t expected_packet_bytes =
      static_cast<uint32_t>(sizeof(aerogpu_cmd_set_samplers) + sizeof(handles));
  if (!Check(dev.cmd.size() >= sizeof(aerogpu_cmd_stream_header) + expected_packet_bytes,
             "SET_SAMPLERS packet emitted")) {
    return false;
  }

  const auto* pkt =
      reinterpret_cast<const aerogpu_cmd_set_samplers*>(dev.cmd.data() + sizeof(aerogpu_cmd_stream_header));
  if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_SAMPLERS, "SET_SAMPLERS opcode")) {
    return false;
  }
  if (!Check(pkt->hdr.size_bytes == expected_packet_bytes, "SET_SAMPLERS hdr.size_bytes")) {
    return false;
  }
  if (!Check(pkt->shader_stage == AEROGPU_SHADER_STAGE_PIXEL, "SET_SAMPLERS shader_stage")) {
    return false;
  }
  if (!Check(pkt->start_slot == 4, "SET_SAMPLERS start_slot")) {
    return false;
  }
  if (!Check(pkt->sampler_count == 3, "SET_SAMPLERS sampler_count")) {
    return false;
  }
  if (!Check(pkt->reserved0 == 0, "SET_SAMPLERS reserved0 cleared")) {
    return false;
  }
  const auto* payload =
      reinterpret_cast<const aerogpu_handle_t*>(reinterpret_cast<const uint8_t*>(pkt) + sizeof(*pkt));
  if (!Check(payload[0] == handles[0], "SET_SAMPLERS payload[0]")) {
    return false;
  }
  if (!Check(payload[1] == handles[1], "SET_SAMPLERS payload[1]")) {
    return false;
  }
  if (!Check(payload[2] == handles[2], "SET_SAMPLERS payload[2]")) {
    return false;
  }

  // Invalid argument path: non-zero count with null samplers pointer.
  DummyDevice invalid{};
  errors.clear();
  const bool ok_invalid = EmitSetSamplersCmdLocked(&invalid,
                                                  AEROGPU_SHADER_STAGE_VERTEX,
                                                  /*start_slot=*/0,
                                                  /*sampler_count=*/1,
                                                  /*samplers=*/nullptr,
                                                  [&](HRESULT hr) { errors.push_back(hr); });
  if (!Check(!ok_invalid, "EmitSetSamplersCmdLocked should fail when samplers==nullptr and sampler_count!=0")) {
    return false;
  }
  if (!Check(errors.size() == 1 && errors[0] == E_INVALIDARG, "invalid samplers pointer reports E_INVALIDARG")) {
    return false;
  }
  if (!Check(invalid.cmd.size() == sizeof(aerogpu_cmd_stream_header), "invalid args do not emit a packet")) {
    return false;
  }

  // Insufficient-space path.
  alignas(4) uint8_t tiny_buf[sizeof(aerogpu_cmd_stream_header)] = {};
  struct TinyDevice {
    aerogpu::CmdWriter cmd;
    TinyDevice(uint8_t* buf, size_t cap) {
      cmd.set_span(buf, cap);
    }
  };
  TinyDevice tiny(tiny_buf, sizeof(tiny_buf));
  errors.clear();
  const aerogpu_handle_t one_handle[1] = {7};
  const bool ok2 = EmitSetSamplersCmdLocked(&tiny,
                                           AEROGPU_SHADER_STAGE_PIXEL,
                                           /*start_slot=*/0,
                                           /*sampler_count=*/1,
                                           one_handle,
                                           [&](HRESULT hr) { errors.push_back(hr); });
  if (!Check(!ok2, "EmitSetSamplersCmdLocked should fail when cmd append fails")) {
    return false;
  }
  if (!Check(errors.size() == 1 && errors[0] == E_OUTOFMEMORY, "cmd append failure reports E_OUTOFMEMORY")) {
    return false;
  }

  return true;
}

bool TestTrackWddmAllocForSubmitLockedHelper() {
  using aerogpu::d3d10_11::WddmSubmitAllocation;

  struct TestResource {
    uint32_t backing_alloc_id = 0;
    uint32_t wddm_allocation_handle = 0;
  };

  struct TestDevice {
    std::vector<WddmSubmitAllocation> wddm_submit_allocation_handles;
    bool wddm_submit_allocation_list_oom = false;
  };

  TestDevice dev{};
  TestResource ignored_host{};
  ignored_host.backing_alloc_id = 0;
  ignored_host.wddm_allocation_handle = 123;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &ignored_host, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.empty(), "host-owned resources are ignored")) {
    return false;
  }

  TestResource ignored_no_handle{};
  ignored_no_handle.backing_alloc_id = 1;
  ignored_no_handle.wddm_allocation_handle = 0;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &ignored_no_handle, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.empty(), "resources without WDDM allocation handle are ignored")) {
    return false;
  }

  TestResource res_a{};
  res_a.backing_alloc_id = 1;
  res_a.wddm_allocation_handle = 100;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &res_a, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.size() == 1, "TrackWddmAllocForSubmitLocked appends new entries")) {
    return false;
  }
  if (!Check(dev.wddm_submit_allocation_handles[0].allocation_handle == 100, "allocation_handle recorded")) {
    return false;
  }
  if (!Check(dev.wddm_submit_allocation_handles[0].write == 0, "read-only usage does not set write flag")) {
    return false;
  }

  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &res_a, /*write=*/true, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.size() == 1, "duplicate allocations are de-duplicated")) {
    return false;
  }
  if (!Check(dev.wddm_submit_allocation_handles[0].write == 1, "write usage upgrades write flag")) {
    return false;
  }

  // Once upgraded to write, later read-only tracking must not downgrade.
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &res_a, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles[0].write == 1, "write flag is sticky once upgraded")) {
    return false;
  }

  TestResource res_b{};
  res_b.backing_alloc_id = 2;
  res_b.wddm_allocation_handle = 200;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &res_b, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.size() == 2, "multiple allocations are tracked")) {
    return false;
  }

  dev.wddm_submit_allocation_list_oom = true;
  TestResource res_c{};
  res_c.backing_alloc_id = 3;
  res_c.wddm_allocation_handle = 300;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(&dev, &res_c, /*write=*/false, [](HRESULT) {});
  if (!Check(dev.wddm_submit_allocation_handles.size() == 2, "oom poison flag prevents further allocation tracking")) {
    return false;
  }

  return true;
}

size_t AlignUp(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

uint32_t DivRoundUp(uint32_t v, uint32_t d) {
  return (v + (d - 1u)) / d;
}

struct DxgiTextureFormatLayout {
  uint32_t block_width = 1;
  uint32_t block_height = 1;
  uint32_t bytes_per_block = 4;
  bool valid = true;
};

DxgiTextureFormatLayout DxgiTextureFormat(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8X8UnormSrgb:
      return DxgiTextureFormatLayout{1, 1, 4, true};
    case kDxgiFormatB5G6R5Unorm:
    case kDxgiFormatB5G5R5A1Unorm:
      return DxgiTextureFormatLayout{1, 1, 2, true};
    case kDxgiFormatBc1Unorm:
    case kDxgiFormatBc1UnormSrgb:
      return DxgiTextureFormatLayout{4, 4, 8, true};
    case kDxgiFormatBc2Unorm:
    case kDxgiFormatBc2UnormSrgb:
    case kDxgiFormatBc3Unorm:
    case kDxgiFormatBc3UnormSrgb:
    case kDxgiFormatBc7Unorm:
    case kDxgiFormatBc7UnormSrgb:
      return DxgiTextureFormatLayout{4, 4, 16, true};
    default:
      // Tests default to 4BPP textures; use that as a safe fallback when a DXGI
      // format isn't modeled yet.
      return DxgiTextureFormatLayout{1, 1, 4, true};
  }
}

uint32_t DxgiTextureMinRowPitchBytes(uint32_t dxgi_format, uint32_t width) {
  if (width == 0) {
    return 0;
  }
  const DxgiTextureFormatLayout layout = DxgiTextureFormat(dxgi_format);
  if (!layout.valid || layout.block_width == 0 || layout.bytes_per_block == 0) {
    return 0;
  }
  const uint32_t blocks_w = DivRoundUp(width, layout.block_width);
  const uint64_t row_bytes = static_cast<uint64_t>(blocks_w) * static_cast<uint64_t>(layout.bytes_per_block);
  if (row_bytes == 0 || row_bytes > UINT32_MAX) {
    return 0;
  }
  return static_cast<uint32_t>(row_bytes);
}

uint32_t DxgiTextureNumRows(uint32_t dxgi_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const DxgiTextureFormatLayout layout = DxgiTextureFormat(dxgi_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return DivRoundUp(height, layout.block_height);
}

uint32_t CalcFullMipLevels(uint32_t width, uint32_t height) {
  uint32_t w = width ? width : 1u;
  uint32_t h = height ? height : 1u;
  uint32_t levels = 1;
  while (w > 1 || h > 1) {
    w = (w > 1) ? (w / 2) : 1u;
    h = (h > 1) ? (h / 2) : 1u;
    levels++;
  }
  return levels;
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
    // Fall back to the provided buffer length when the header is malformed. Callers that require
    // strict validation should call ValidateStream first.
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
  // Forward-compat: allow the submission buffer to be larger than the stream header's declared
  // size (the header carries bytes-used; trailing bytes are ignored).
  if (!Check(stream->size_bytes <= len, "stream size_bytes within submitted length")) {
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
  return true;
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

size_t CountOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  const size_t stream_len = StreamBytesUsed(buf, len);
  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

struct Allocation {
  AEROGPU_WDDM_ALLOCATION_HANDLE handle = 0;
  std::vector<uint8_t> bytes;
};

struct Harness {
  std::vector<uint8_t> last_stream;
  std::vector<AEROGPU_WDDM_SUBMIT_ALLOCATION> last_allocs;
  std::vector<HRESULT> errors;

  std::vector<Allocation> allocations;
  AEROGPU_WDDM_ALLOCATION_HANDLE next_handle = 1;

  // Optional async fence model used by tests that need to validate DO_NOT_WAIT
  // behavior without a real Win7/WDDM stack.
  bool async_fences = false;
  std::atomic<uint64_t> next_fence{1};
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> completed_fence{0};
  std::atomic<uint32_t> wait_call_count{0};
  std::atomic<uint32_t> last_wait_timeout_ms{0};
  std::mutex fence_mutex;
  std::condition_variable fence_cv;

  Allocation* FindAlloc(AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
    for (auto& a : allocations) {
      if (a.handle == handle) {
        return &a;
      }
    }
    return nullptr;
  }

  static HRESULT AEROGPU_APIENTRY AllocateBacking(void* user,
                                                  const AEROGPU_DDIARG_CREATERESOURCE* desc,
                                                  AEROGPU_WDDM_ALLOCATION_HANDLE* out_handle,
                                                  uint64_t* out_size_bytes,
                                                  uint32_t* out_row_pitch_bytes) {
    if (!user || !desc || !out_handle || !out_size_bytes) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);

    Allocation alloc{};
    alloc.handle = h->next_handle++;

    if (out_row_pitch_bytes) {
      *out_row_pitch_bytes = 0;
    }

    uint64_t bytes = 0;
    if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
      bytes = desc->ByteWidth;
    } else if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
      const uint32_t width = desc->Width ? desc->Width : 1u;
      const uint32_t height = desc->Height ? desc->Height : 1u;
      uint32_t mip_levels = desc->MipLevels;
      if (mip_levels == 0) {
        mip_levels = CalcFullMipLevels(width, height);
      }
      const uint32_t array_layers = desc->ArraySize ? desc->ArraySize : 1u;

      const uint32_t tight_row_pitch = DxgiTextureMinRowPitchBytes(desc->Format, width);
      const uint32_t row_pitch = static_cast<uint32_t>(AlignUp(tight_row_pitch ? tight_row_pitch : (width * 4u), 64));
      if (out_row_pitch_bytes) {
        *out_row_pitch_bytes = row_pitch;
      }

      uint64_t layer_stride = 0;
      uint32_t level_w = width;
      uint32_t level_h = height;
      for (uint32_t level = 0; level < mip_levels; ++level) {
        const uint32_t tight_pitch = DxgiTextureMinRowPitchBytes(desc->Format, level_w);
        const uint32_t pitch = (level == 0) ? row_pitch : (tight_pitch ? tight_pitch : (level_w * 4u));
        const uint32_t rows = DxgiTextureNumRows(desc->Format, level_h);
        layer_stride += static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows ? rows : level_h);
        level_w = (level_w > 1) ? (level_w / 2) : 1u;
        level_h = (level_h > 1) ? (level_h / 2) : 1u;
      }
      bytes = layer_stride * static_cast<uint64_t>(array_layers);
    } else {
      bytes = desc->ByteWidth;
    }

    // Mirror the UMD's conservative alignment expectations.
    bytes = AlignUp(static_cast<size_t>(bytes), 256);
    alloc.bytes.resize(static_cast<size_t>(bytes), 0);

    h->allocations.push_back(std::move(alloc));
    *out_handle = h->allocations.back().handle;
    *out_size_bytes = bytes;
    return S_OK;
  }

  static HRESULT AEROGPU_APIENTRY MapAllocation(void* user, AEROGPU_WDDM_ALLOCATION_HANDLE handle, void** out_cpu_ptr) {
    if (!user || !out_cpu_ptr || handle == 0) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    Allocation* alloc = h->FindAlloc(handle);
    if (!alloc) {
      return E_INVALIDARG;
    }
    *out_cpu_ptr = alloc->bytes.data();
    return S_OK;
  }

  static void AEROGPU_APIENTRY UnmapAllocation(void* user, AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
    (void)user;
    (void)handle;
  }

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                   const void* cmd_stream,
                                                   uint32_t cmd_stream_size_bytes,
                                                   const AEROGPU_WDDM_SUBMIT_ALLOCATION* allocs,
                                                   uint32_t alloc_count,
                                                   uint64_t* out_fence) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    if (!allocs || alloc_count == 0) {
      h->last_allocs.clear();
    } else {
      h->last_allocs.assign(allocs, allocs + alloc_count);
    }
    if (out_fence) {
      if (h->async_fences) {
        const uint64_t fence = h->next_fence.fetch_add(1, std::memory_order_relaxed);
        h->last_submitted_fence.store(fence, std::memory_order_relaxed);
        *out_fence = fence;
      } else {
        *out_fence = 0;
      }
    }
    return S_OK;
  }

  static uint64_t AEROGPU_APIENTRY QueryCompletedFence(void* user) {
    if (!user) {
      return 0;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    return h->completed_fence.load(std::memory_order_relaxed);
  }

  static HRESULT AEROGPU_APIENTRY WaitForFence(void* user, uint64_t fence, uint32_t timeout_ms) {
    if (!user) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    h->wait_call_count.fetch_add(1, std::memory_order_relaxed);
    h->last_wait_timeout_ms.store(timeout_ms, std::memory_order_relaxed);
    if (fence == 0) {
      return S_OK;
    }

    auto ready = [&]() { return h->completed_fence.load(std::memory_order_relaxed) >= fence; };
    if (ready()) {
      return S_OK;
    }
    if (timeout_ms == 0) {
      // `HRESULT_FROM_NT(STATUS_TIMEOUT)` is a SUCCEEDED() HRESULT on Win7-era
      // stacks; the UMD should still treat it as "not ready yet" for DO_NOT_WAIT.
      return static_cast<HRESULT>(0x10000102L);
    }

    std::unique_lock<std::mutex> lock(h->fence_mutex);
    if (timeout_ms == ~0u) {
      h->fence_cv.wait(lock, ready);
      return S_OK;
    }
    if (!h->fence_cv.wait_for(lock, std::chrono::milliseconds(timeout_ms), ready)) {
      // Match Win7-era status semantics used by the UMD poll path.
      return static_cast<HRESULT>(0x10000102L);
    }
    return S_OK;
  }

  static void AEROGPU_APIENTRY SetError(void* user, HRESULT hr) {
    if (!user) {
      return;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    h->errors.push_back(hr);
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

bool InitTestDevice(TestDevice* out, bool want_backing_allocations, bool async_fences) {
  if (!out) {
    return false;
  }

  out->harness.async_fences = async_fences;

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;
  if (async_fences) {
    out->callbacks.pfnWaitForFence = &Harness::WaitForFence;
  }
  if (want_backing_allocations) {
    out->callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
    out->callbacks.pfnMapAllocation = &Harness::MapAllocation;
    out->callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  }

  D3D10DDIARG_OPENADAPTER open = {};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  // CreateDevice contract.
  D3D10DDIARG_CREATEDEVICE create = {};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
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

bool CheckDeviceFuncsTableNoNullEntries(const AEROGPU_D3D10_11_DEVICEFUNCS& device_funcs, const char* label) {
  // The portable `AEROGPU_D3D10_11_DEVICEFUNCS` table is a flat ABI surface of function pointers.
  // We intentionally treat it as a dense array and assert that none of the entries are left in the
  // all-zero "NULL function pointer" state after device creation.
  constexpr size_t kSlotBytes = sizeof(decltype(AEROGPU_D3D10_11_DEVICEFUNCS{}.pfnDestroyDevice));
  static_assert(kSlotBytes > 0, "function pointer slot size must be non-zero");
  static_assert(sizeof(AEROGPU_D3D10_11_DEVICEFUNCS) % kSlotBytes == 0,
                "device funcs table must be densely packed into function pointer slots");

  const size_t slot_count = sizeof(AEROGPU_D3D10_11_DEVICEFUNCS) / kSlotBytes;
  const auto* bytes = reinterpret_cast<const uint8_t*>(&device_funcs);

  for (size_t i = 0; i < slot_count; i++) {
    bool all_zero = true;
    for (size_t j = 0; j < kSlotBytes; j++) {
      if (bytes[i * kSlotBytes + j] != 0) {
        all_zero = false;
        break;
      }
    }
    char msg[256] = {};
    std::snprintf(msg,
                  sizeof(msg),
                  "%s: device-funcs slot[%zu] must be initialized (non-NULL)",
                  label ? label : "device",
                  i);
    if (!Check(!all_zero, msg)) {
      return false;
    }
  }

  return true;
}

bool TestDeviceFuncsTableNoNullEntriesHostOwned() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(device-funcs host-owned)")) {
    return false;
  }

  const bool ok = CheckDeviceFuncsTableNoNullEntries(dev.device_funcs, "host-owned");

  if (dev.device_funcs.pfnDestroyDevice) {
    dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  }
  if (dev.adapter_funcs.pfnCloseAdapter) {
    dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  }

  return ok;
}

bool TestDeviceFuncsTableNoNullEntriesGuestBacked() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(device-funcs guest-backed)")) {
    return false;
  }

  const bool ok = CheckDeviceFuncsTableNoNullEntries(dev.device_funcs, "guest-backed");

  if (dev.device_funcs.pfnDestroyDevice) {
    dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  }
  if (dev.adapter_funcs.pfnCloseAdapter) {
    dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  }

  return ok;
}

struct TestResource {
  D3D10DDI_HRESOURCE hResource = {};
  std::vector<uint8_t> storage;
};

struct TestRenderTargetView {
  D3D10DDI_HRENDERTARGETVIEW hView = {};
  std::vector<uint8_t> storage;
};

struct TestShaderResourceView {
  D3D10DDI_HSHADERRESOURCEVIEW hView = {};
  std::vector<uint8_t> storage;
};

bool CreateBuffer(TestDevice* dev,
                  uint32_t byte_width,
                  uint32_t usage,
                  uint32_t bind_flags,
                  uint32_t cpu_access_flags,
                  TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.ByteWidth = byte_width;
  desc.StructureByteStride = 0;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(buffer)")) {
    return false;
  }
  return true;
}

bool CreateStagingBuffer(TestDevice* dev,
                         uint32_t byte_width,
                         uint32_t cpu_access_flags,
                         TestResource* out) {
  return CreateBuffer(dev,
                      byte_width,
                      AEROGPU_D3D11_USAGE_STAGING,
                      /*bind_flags=*/0,
                      cpu_access_flags,
                      out);
}

bool CreateBufferWithInitialData(TestDevice* dev,
                                 uint32_t byte_width,
                                 uint32_t usage,
                                 uint32_t bind_flags,
                                 uint32_t cpu_access_flags,
                                 const void* initial_bytes,
                                 TestResource* out) {
  if (!dev || !out || !initial_bytes) {
    return false;
  }

  AEROGPU_DDI_SUBRESOURCE_DATA init = {};
  init.pSysMem = initial_bytes;
  init.SysMemPitch = 0;
  init.SysMemSlicePitch = 0;

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.ByteWidth = byte_width;
  desc.StructureByteStride = 0;
  desc.pInitialData = &init;
  desc.InitialDataCount = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(buffer initial data)")) {
    return false;
  }
  return true;
}

bool CreateTexture2D(TestDevice* dev,
                     uint32_t width,
                     uint32_t height,
                     uint32_t usage,
                     uint32_t bind_flags,
                     uint32_t cpu_access_flags,
                     TestResource* out,
                     uint32_t dxgi_format = kDxgiFormatB8G8R8A8Unorm) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = dxgi_format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d)")) {
    return false;
  }
  return true;
}

bool CreateStagingTexture2DWithFormatAndDesc(TestDevice* dev,
                                             uint32_t width,
                                             uint32_t height,
                                             uint32_t dxgi_format,
                                             uint32_t cpu_access_flags,
                                             uint32_t mip_levels,
                                             uint32_t array_size,
                                             TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_STAGING;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = mip_levels;
  desc.ArraySize = array_size;
  desc.Format = dxgi_format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d)")) {
    return false;
  }
  return true;
}

bool CreateDynamicTexture2DWithFormatAndDesc(TestDevice* dev,
                                             uint32_t width,
                                             uint32_t height,
                                             uint32_t dxgi_format,
                                             uint32_t cpu_access_flags,
                                             uint32_t mip_levels,
                                             uint32_t array_size,
                                             TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  // Prefer a typical bind for dynamic textures (also exercises AEROGPU_RESOURCE_USAGE_TEXTURE).
  desc.BindFlags = kD3D11BindShaderResource;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DYNAMIC;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = mip_levels;
  desc.ArraySize = array_size;
  desc.Format = dxgi_format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(dynamic tex2d)")) {
    return false;
  }
  return true;
}

bool CreateStagingTexture2DWithFormat(TestDevice* dev,
                                      uint32_t width,
                                      uint32_t height,
                                      uint32_t dxgi_format,
                                      uint32_t cpu_access_flags,
                                      TestResource* out) {
  return CreateStagingTexture2DWithFormatAndDesc(dev,
                                                 width,
                                                 height,
                                                 dxgi_format,
                                                 cpu_access_flags,
                                                 /*mip_levels=*/1,
                                                 /*array_size=*/1,
                                                 out);
}

bool CreateStagingTexture2D(TestDevice* dev,
                            uint32_t width,
                            uint32_t height,
                            uint32_t cpu_access_flags,
                            TestResource* out) {
  return CreateStagingTexture2DWithFormat(dev, width, height, kDxgiFormatB8G8R8A8Unorm, cpu_access_flags, out);
}

bool CreateRenderTargetView(TestDevice* dev, TestResource* tex, TestRenderTargetView* out) {
  if (!dev || !tex || !out) {
    return false;
  }
  AEROGPU_DDIARG_CREATERENDERTARGETVIEW desc = {};
  desc.hResource = tex->hResource;
  const SIZE_T size = dev->device_funcs.pfnCalcPrivateRTVSize(dev->hDevice, &desc);
  if (!Check(size != 0, "CalcPrivateRTVSize returned non-zero size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hView.pDrvPrivate = out->storage.data();
  const HRESULT hr = dev->device_funcs.pfnCreateRTV(dev->hDevice, &desc, out->hView);
  if (!Check(hr == S_OK, "CreateRTV")) {
    return false;
  }
  return true;
}

bool CreateShaderResourceView(TestDevice* dev, TestResource* tex, TestShaderResourceView* out) {
  if (!dev || !tex || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW desc = {};
  desc.hResource = tex->hResource;
  desc.Format = 0;
  desc.ViewDimension = AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D;
  desc.MostDetailedMip = 0;
  desc.MipLevels = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateShaderResourceViewSize(dev->hDevice, &desc);
  // Unlike resources (which must at least hold a pointer-sized `hResource.pDrvPrivate`),
  // a view's private storage can be smaller than `sizeof(void*)` (our current SRV
  // backing struct is 4 bytes). Still require a non-zero size so the function is
  // implemented.
  if (!Check(size != 0, "CalcPrivateShaderResourceViewSize returned a non-zero size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hView.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateShaderResourceView(dev->hDevice, &desc, out->hView);
  if (!Check(hr == S_OK, "CreateShaderResourceView")) {
    return false;
  }
  return true;
}

bool CreateTexture2D(TestDevice* dev,
                     uint32_t width,
                     uint32_t height,
                     uint32_t usage,
                     uint32_t bind_flags,
                     uint32_t cpu_access_flags,
                     uint32_t dxgi_format,
                     TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = dxgi_format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d)")) {
    return false;
  }
  return true;
}

bool CreateTexture2DWithInitialData(TestDevice* dev,
                                    uint32_t width,
                                    uint32_t height,
                                    uint32_t usage,
                                    uint32_t bind_flags,
                                    uint32_t cpu_access_flags,
                                    const void* initial_bytes,
                                    uint32_t initial_row_pitch,
                                    TestResource* out,
                                    uint32_t dxgi_format = kDxgiFormatB8G8R8A8Unorm) {
  if (!dev || !out || !initial_bytes) {
    return false;
  }

  AEROGPU_DDI_SUBRESOURCE_DATA init = {};
  init.pSysMem = initial_bytes;
  init.SysMemPitch = initial_row_pitch;
  init.SysMemSlicePitch = 0;

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = dxgi_format;
  desc.pInitialData = &init;
  desc.InitialDataCount = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d initial data)")) {
    return false;
  }
  return true;
}

bool TestHostOwnedBufferUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) host-owned")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 0, "Map(buffer) should return RowPitch=0")) {
    return false;
  }
  if (!Check(mapped.DepthPitch == 0, "Map(buffer) should return DepthPitch=0")) {
    return false;
  }

  const uint8_t expected[16] = {
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF};
  std::memcpy(mapped.pData, expected, sizeof(expected));

  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size_bytes == 16")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits in stream")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedTextureUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(host-owned tex2d)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) host-owned tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 12, "RowPitch == width*4 for host-owned tex2d")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bpp = 4;
  const uint32_t bytes_per_row = width * bpp;
  const uint32_t row_pitch = mapped.RowPitch;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;
  std::vector<uint8_t> expected(total_bytes, 0);

  auto* dst = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t v = static_cast<uint8_t>((y * 17u) + x);
      dst[static_cast<size_t>(y) * row_pitch + x] = v;
      expected[static_cast<size_t>(y) * row_pitch + x] = v;
    }
  }

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after tex2d Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == expected.size(), "UPLOAD_RESOURCE size matches tex2d bytes")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits in stream")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected.data(), payload_size) == 0,
             "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateTexture2dSrgbFormatEncodesSrgbAerogpuFormat() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(create tex2d sRGB)")) {
    return false;
  }

  constexpr uint32_t width = 5;
  constexpr uint32_t height = 7;
  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              width,
                                              height,
                                              kDxgiFormatB8G8R8A8UnormSrgb,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(sRGB)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(sRGB tex2d)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }

  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->width == width, "CREATE_TEXTURE2D width matches")) {
    return false;
  }
  if (!Check(create_cmd->height == height, "CREATE_TEXTURE2D height matches")) {
    return false;
  }
  if (!Check(create_cmd->format == AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB,
             "CREATE_TEXTURE2D format is AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestB5Texture2DCreateMapUnmapEncodesAerogpuFormat() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(B5 tex2d)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_aerogpu_format;
  };

  static constexpr uint32_t kWidth = 7;
  static constexpr uint32_t kHeight = 3;
  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_B5G6R5_UNORM", kDxgiFormatB5G6R5Unorm, AEROGPU_FORMAT_B5G6R5_UNORM},
      {"DXGI_FORMAT_B5G5R5A1_UNORM", kDxgiFormatB5G5R5A1Unorm, AEROGPU_FORMAT_B5G5R5A1_UNORM},
  };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                kWidth,
                                                kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &tex),
               c.name)) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        tex.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) B5 tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch == kWidth * 2u, "Map RowPitch matches 16-bit format row bytes")) {
      return false;
    }

    // Write a recognizable pattern and unmap (smoke test).
    auto* dst = static_cast<uint8_t*>(mapped.pData);
    const uint32_t row_pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < kHeight; y++) {
      uint8_t* row = dst + static_cast<size_t>(y) * row_pitch;
      for (uint32_t x = 0; x < kWidth * 2u; x++) {
        row[x] = static_cast<uint8_t>((y + 1u) * 13u + x);
      }
    }

    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
    hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after B5 tex2d Unmap")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[128] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_aerogpu_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateTexture2DMipLevelsZeroAllocatesFullChain() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(create tex2d mips=0)")) {
    return false;
  }

  constexpr uint32_t width = 7;
  constexpr uint32_t height = 5;
  const uint32_t expected_mips = CalcFullMipLevels(width, height);
  if (!Check(expected_mips > 1, "test expects a non-trivial full mip chain")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     width,
                                                     height,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/0,
                                                     /*array_size=*/1,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(mips=0)")) {
    return false;
  }

  const uint32_t last_subresource = expected_mips - 1;
  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      last_subresource,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) last mip (mips=0)")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "StagingResourceMap returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 4, "last mip RowPitch == 4 (1x1 RGBA8)")) {
    return false;
  }
  static_cast<uint8_t*>(mapped.pData)[0] = 0xAB;
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, last_subresource);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(tex2d mips=0)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted (mips=0)")) {
    return false;
  }

  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->width == width, "CREATE_TEXTURE2D width matches (mips=0)")) {
    return false;
  }
  if (!Check(create_cmd->height == height, "CREATE_TEXTURE2D height matches (mips=0)")) {
    return false;
  }
  if (!Check(create_cmd->mip_levels == expected_mips, "CREATE_TEXTURE2D mip_levels == full mip chain")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedBufferUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) guest-backed")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 0, "Map(buffer) should return RowPitch=0")) {
    return false;
  }
  if (!Check(mapped.DepthPitch == 0, "Map(buffer) should return DepthPitch=0")) {
    return false;
  }

  const uint8_t expected[16] = {
      0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE};
  std::memcpy(mapped.pData, expected, sizeof(expected));

  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size_bytes == 16")) {
    return false;
  }

  bool found_alloc = false;
  uint8_t found_write = 1;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
      found_write = a.write;
    }
  }
  if (!Check(found_alloc, "guest-backed submit alloc list contains backing alloc")) {
    return false;
  }
  if (!Check(found_write == 0, "RESOURCE_DIRTY_RANGE should mark guest allocation as read-only")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTextureUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(guest-backed tex2d)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned non-zero RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bpp = 4;
  const uint32_t bytes_per_row = width * bpp;
  const uint32_t row_pitch = mapped.RowPitch;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;
  std::vector<uint8_t> expected(total_bytes, 0xCD);

  auto* dst = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    uint8_t* row = dst + static_cast<size_t>(y) * row_pitch;
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t v = static_cast<uint8_t>((y * 31u) + x);
      row[x] = v;
      expected[static_cast<size_t>(y) * row_pitch + x] = v;
    }
    if (row_pitch > bytes_per_row) {
      std::memset(row + bytes_per_row, 0xCD, row_pitch - bytes_per_row);
    }
  }

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after tex2d Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == expected.size(), "RESOURCE_DIRTY_RANGE size matches tex2d bytes")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed tex2d submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected.size(), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
             "guest-backed allocation bytes reflect CPU writes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedBcTextureUnmapDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(guest-backed bc tex2d)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &tex),
               "CreateStagingTexture2DWithFormat(guest-backed bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        tex.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned non-zero RowPitch")) {
      return false;
    }

    const uint32_t blocks_w = div_round_up(kWidth, 4);
    const uint32_t blocks_h = div_round_up(kHeight, 4);
    const uint32_t required_row_bytes = blocks_w * c.block_bytes;
    if (!Check(mapped.RowPitch >= required_row_bytes, "Map RowPitch large enough for BC row")) {
      return false;
    }
    const uint32_t expected_depth_pitch = mapped.RowPitch * blocks_h;
    if (!Check(mapped.DepthPitch == expected_depth_pitch, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    const uint32_t row_pitch = mapped.RowPitch;
    std::vector<uint8_t> expected(static_cast<size_t>(expected_depth_pitch), 0xCD);
    auto* dst = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      uint8_t* row = dst + static_cast<size_t>(y) * row_pitch;
      for (uint32_t x = 0; x < required_row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 31u + x);
        row[x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
      if (row_pitch > required_row_bytes) {
        std::memset(row + required_row_bytes, 0xCD, row_pitch - required_row_bytes);
      }
    }

    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
    hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after guest-backed bc tex2d Unmap")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d Unmap should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d Unmap should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->format == c.expected_format, "CREATE_TEXTURE2D format matches expected")) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
      return false;
    }

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == expected.size(), "RESOURCE_DIRTY_RANGE size matches BC tex2d bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (const auto& a : dev.harness.last_allocs) {
      if (a.handle == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed bc tex2d submit alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= expected.size(), "backing allocation large enough")) {
      return false;
    }
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
               "guest-backed allocation bytes reflect CPU writes")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestMapUsageValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE,
                                             /*map_flags=*/0,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(WRITE) on READ-only staging resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapCpuAccessValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(cpu access validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_READ,
                                             /*map_flags=*/0,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(READ) on WRITE-only staging resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(map flags)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE,
                                             /*map_flags=*/0x1,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "Map with unknown MapFlags bits should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestStagingMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(staging map flags)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev,
                                    /*width=*/3,
                                    /*height=*/2,
                                    AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                    &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                            tex.hResource,
                                                            /*subresource=*/0,
                                                            AEROGPU_DDI_MAP_WRITE,
                                                            /*map_flags=*/0x1,
                                                            &mapped);
  if (!Check(hr == E_INVALIDARG, "StagingResourceMap with unknown MapFlags bits should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapAlreadyMappedFails() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(map already mapped)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map should succeed initially")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped2 = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_WRITE,
                               /*map_flags=*/0,
                               &mapped2);
  if (!Check(hr == E_FAIL, "Map on already mapped subresource should fail")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.empty(), "Unmap after failed Map should not report errors")) {
    return false;
  }

  mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_WRITE,
                               /*map_flags=*/0,
                               &mapped);
  if (!Check(hr == S_OK, "Map should succeed again after Unmap")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev,
                                    /*width=*/3,
                                    /*height=*/2,
                                    AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                    &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE tex_map = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_WRITE,
                                              /*map_flags=*/0,
                                              &tex_map);
  if (!Check(hr == S_OK, "StagingResourceMap should succeed initially")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE tex_map2 = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_WRITE,
                                              /*map_flags=*/0,
                                              &tex_map2);
  if (!Check(hr == E_FAIL, "StagingResourceMap on already mapped subresource should fail")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.empty(), "Valid StagingResourceUnmap after failed Map should not report errors")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapSubresourceValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(map subresource validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/1,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == E_INVALIDARG, "Map on buffer with subresource!=0 should fail")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/2,
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2D(mips=2, array=2)")) {
    return false;
  }

  mapped = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/4,
                                              AEROGPU_DDI_MAP_WRITE,
                                              /*map_flags=*/0,
                                              &mapped);
  if (!Check(hr == E_INVALIDARG, "StagingResourceMap with out-of-range subresource should fail")) {
    return false;
  }

  // Sanity: the last valid subresource should still map successfully.
  mapped = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/3,
                                              AEROGPU_DDI_MAP_WRITE,
                                              /*map_flags=*/0,
                                              &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap on last subresource should succeed")) {
    return false;
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/3);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestStagingMapTypeValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(staging map type validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE_DISCARD,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(WRITE_DISCARD) on STAGING should fail")) {
    return false;
  }
  mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE,
                               /*map_flags=*/0,
                               &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(WRITE_NO_OVERWRITE) on STAGING should fail")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev,
                                    /*width=*/3,
                                    /*height=*/2,
                                    AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                    &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  mapped = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_WRITE_DISCARD,
                                              /*map_flags=*/0,
                                              &mapped);
  if (!Check(hr == E_INVALIDARG, "StagingResourceMap(WRITE_DISCARD) on STAGING should fail")) {
    return false;
  }
  mapped = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              tex.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE,
                                              /*map_flags=*/0,
                                              &mapped);
  if (!Check(hr == E_INVALIDARG, "StagingResourceMap(WRITE_NO_OVERWRITE) on STAGING should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestStagingReadWriteMapAllowed() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(staging read/write map)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev,
                                 /*byte_width=*/16,
                                 AEROGPU_D3D11_CPU_ACCESS_READ | AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                 &buf),
             "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_READ_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(READ_WRITE) on STAGING cpu_read|cpu_write buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map(READ_WRITE) returned non-null pointer")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 0 && mapped.DepthPitch == 0, "Map(READ_WRITE) buffer pitches are 0")) {
    return false;
  }

  uint8_t expected[16] = {};
  for (size_t i = 0; i < sizeof(expected); ++i) {
    expected[i] = static_cast<uint8_t>(i * 11u);
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap(READ_WRITE)")) {
    return false;
  }
  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned Unmap(READ_WRITE) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned Unmap(READ_WRITE) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches Map size")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(expected) <= stream_len, "UPLOAD_RESOURCE payload bounds")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, sizeof(expected)) == 0, "UPLOAD_RESOURCE payload matches")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapDoNotWaitReportsStillDrawing() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/true),
             "InitTestDevice(map DO_NOT_WAIT)")) {
    return false;
  }

  TestResource src{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  // Record a copy so the staging READ buffer has an associated "GPU write" fence.
  dev.device_funcs.pfnCopyResource(dev.hDevice, buf.hResource, src.hResource);

  dev.harness.completed_fence.store(0, std::memory_order_relaxed);
  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush to create pending fence")) {
    return false;
  }
  const uint64_t pending_fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(pending_fence != 0, "Flush returned a non-zero fence")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(~0u, std::memory_order_relaxed);
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_READ,
                                       AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                                       &mapped);
  if (!Check(hr == DXGI_ERROR_WAS_STILL_DRAWING, "Map(DO_NOT_WAIT) should return DXGI_ERROR_WAS_STILL_DRAWING")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(DO_NOT_WAIT) should issue exactly one fence wait poll")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == 0,
             "Map(DO_NOT_WAIT) should pass timeout_ms=0 to fence wait")) {
    return false;
  }

  // Mark the fence complete and retry; DO_NOT_WAIT should now succeed.
  dev.harness.completed_fence.store(pending_fence, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  mapped = {};
  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(~0u, std::memory_order_relaxed);
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                               &mapped);
  if (!Check(hr == S_OK, "Map(DO_NOT_WAIT) should succeed once fence is complete")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(DO_NOT_WAIT) retry should poll fence once")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == 0,
             "Map(DO_NOT_WAIT) retry should still pass timeout_ms=0")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned a non-null pointer")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapDoNotWaitIgnoresUnrelatedInFlightWork() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/true),
             "InitTestDevice(map DO_NOT_WAIT unrelated fences)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  // Record a copy that writes into `dst`.
  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  dev.harness.completed_fence.store(0, std::memory_order_relaxed);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CopyResource")) {
    return false;
  }
  const uint64_t fence1 = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(fence1 != 0, "CopyResource submission produced a non-zero fence")) {
    return false;
  }

  // Mark the copy fence complete.
  dev.harness.completed_fence.store(fence1, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  // Submit unrelated work (a standalone Flush) to advance the device's latest fence.
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush unrelated work")) {
    return false;
  }
  const uint64_t fence2 = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(fence2 > fence1, "Unrelated submission produced a later fence")) {
    return false;
  }

  // Keep `fence2` incomplete while `fence1` is complete.
  dev.harness.completed_fence.store(fence1, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  // Map(DO_NOT_WAIT) should succeed because the last write to `dst` (fence1) is complete, even
  // though newer unrelated work (fence2) is still in flight.
  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(~0u, std::memory_order_relaxed);

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                               &mapped);
  if (!Check(hr == S_OK, "Map(DO_NOT_WAIT) should not fail due to unrelated in-flight work")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(DO_NOT_WAIT) should issue exactly one fence wait poll")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == 0,
             "Map(DO_NOT_WAIT) should pass timeout_ms=0 to fence wait")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned a non-null pointer")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapBlockingWaitUsesInfiniteTimeout() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/true),
             "InitTestDevice(map blocking wait)")) {
    return false;
  }

  TestResource src{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  // Record a copy so the staging READ buffer has an associated "GPU write" fence.
  dev.device_funcs.pfnCopyResource(dev.hDevice, buf.hResource, src.hResource);

  dev.harness.completed_fence.store(0, std::memory_order_relaxed);
  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush to create pending fence")) {
    return false;
  }
  const uint64_t pending_fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(pending_fence != 0, "Flush returned a non-zero fence")) {
    return false;
  }

  // Simulate completion so a blocking Map can succeed, but still force the UMD
  // to call into the wait callback (its pre-check uses the UMD's internal fence
  // cache, not the harness value).
  dev.harness.completed_fence.store(pending_fence, std::memory_order_relaxed);

  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(0, std::memory_order_relaxed);

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_READ,
                                             /*map_flags=*/0,
                                             &mapped);
  if (!Check(hr == S_OK, "Map(READ) should succeed once fence is complete")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(READ) should issue exactly one blocking fence wait")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == ~0u,
             "Map(READ) should pass timeout_ms=~0u to fence wait")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned a non-null pointer")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestInvalidUnmapReportsError() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(invalid unmap)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.size() == 1, "Unmap without Map should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "Unmap without Map should report E_INVALIDARG")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map after invalid Unmap")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/1);
  if (!Check(dev.harness.errors.size() == 1, "Unmap with wrong subresource should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "Unmap wrong subresource should report E_INVALIDARG")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.empty(), "Valid Unmap should not report errors")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestInvalidSpecializedUnmapReportsError() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(invalid specialized unmap)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  // Unmap without a prior Map should report E_INVALIDARG.
  dev.harness.errors.clear();
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.size() == 1, "StagingResourceUnmap without Map should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "StagingResourceUnmap without Map should report E_INVALIDARG")) {
    return false;
  }

  // Map/unmap mismatch should also report E_INVALIDARG.
  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "StagingResourceMap returned non-null pointer")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/1);
  if (!Check(dev.harness.errors.size() == 1, "StagingResourceUnmap wrong subresource should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "StagingResourceUnmap wrong subresource should report E_INVALIDARG")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.empty(), "Valid StagingResourceUnmap should not report errors")) {
    return false;
  }

  // Dynamic Unmap wrappers should also report E_INVALIDARG when called without Map.
  TestResource dyn_vb{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &dyn_vb),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnDynamicIABufferUnmap(dev.hDevice, dyn_vb.hResource);
  if (!Check(dev.harness.errors.size() == 1, "DynamicIABufferUnmap without Map should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "DynamicIABufferUnmap without Map should report E_INVALIDARG")) {
    return false;
  }

  TestResource dyn_cb{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &dyn_cb),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnDynamicConstantBufferUnmap(dev.hDevice, dyn_cb.hResource);
  if (!Check(dev.harness.errors.size() == 1, "DynamicConstantBufferUnmap without Map should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG,
             "DynamicConstantBufferUnmap without Map should report E_INVALIDARG")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dyn_cb.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, dyn_vb.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDynamicMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic map flags)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE_DISCARD,
                                             /*map_flags=*/0x1,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "MapDiscard with unknown MapFlags bits should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDynamicMapTypeValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic map type)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(WRITE) on DYNAMIC resource should fail")) {
    return false;
  }

  mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(READ) on DYNAMIC resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapDefaultImmutableRejected() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(map default/immutable)")) {
    return false;
  }

  TestResource def_buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/16,
                          AEROGPU_D3D11_USAGE_DEFAULT,
                          kD3D11BindVertexBuffer,
                          /*cpu_access_flags=*/0,
                          &def_buf),
             "CreateBuffer(default)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       def_buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == E_INVALIDARG, "Map on DEFAULT resource should fail")) {
    return false;
  }
  dev.device_funcs.pfnDestroyResource(dev.hDevice, def_buf.hResource);

  const uint8_t init_bytes[16] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15};
  TestResource imm_buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(init_bytes),
                                         AEROGPU_D3D11_USAGE_IMMUTABLE,
                                         kD3D11BindVertexBuffer,
                                         /*cpu_access_flags=*/0,
                                         init_bytes,
                                         &imm_buf),
             "CreateBufferWithInitialData(immutable)")) {
    return false;
  }
  mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               imm_buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &mapped);
  if (!Check(hr == E_INVALIDARG, "Map on IMMUTABLE resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, imm_buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedDynamicIABufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic ia host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicIABufferMapDiscard host-owned")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicIABufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(i * 7u);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicIABufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicIABufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned dynamic ia Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned dynamic ia Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "dynamic VB CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches dynamic VB")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned dynamic ia submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedDynamicIABufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(dynamic ia guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicIABufferMapDiscard guest-backed")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicIABufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0xA0u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicIABufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicIABufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed dynamic ia Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed dynamic ia Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "dynamic VB CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches dynamic VB")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed dynamic ia submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDynamicBufferUsageValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DEFAULT,
                          kD3D11BindVertexBuffer,
                          /*cpu_access_flags=*/0,
                          &buf),
             "CreateBuffer(default VB)")) {
    return false;
  }

  void* data = nullptr;
  const HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == E_INVALIDARG, "DynamicIABufferMapDiscard on non-dynamic resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedDynamicConstantBufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic cb host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicConstantBufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicConstantBufferMapDiscard host-owned")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicConstantBufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0x20u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicConstantBufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicConstantBufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned dynamic CB Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned dynamic CB Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "dynamic CB CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches dynamic CB")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned dynamic CB submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedDynamicConstantBufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(dynamic cb guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicConstantBufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicConstantBufferMapDiscard guest-backed")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicConstantBufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0xC0u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicConstantBufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicConstantBufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed dynamic CB Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed dynamic CB Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "dynamic CB CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches dynamic CB")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed dynamic CB submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceBufferReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(copy buffer host-owned)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  const uint8_t expected[16] = {0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F, 0xAA, 0xBB,
                                0xCC, 0xDD, 0xEE, 0xFF, 0x10, 0x20, 0x30, 0x40};

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       src.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) src buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &readback);
  if (!Check(hr == S_OK, "Map(READ) dst buffer")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(std::memcmp(readback.pData, expected, sizeof(expected)) == 0, "CopyResource buffer bytes")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER) == 1, "COPY_BUFFER emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER);
  if (!Check(copy_loc.hdr != nullptr, "COPY_BUFFER location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_buffer*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0, "COPY_BUFFER must not have WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceBufferReadbackPadsSize() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(copy buffer host-owned padded size)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/15, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/15, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  const uint8_t expected[15] = {0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F, 0xAA, 0xBB,
                                0xCC, 0xDD, 0xEE, 0xFF, 0x10, 0x20, 0x30};

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       src.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) src buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &readback);
  if (!Check(hr == S_OK, "Map(READ) dst buffer")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(std::memcmp(readback.pData, expected, sizeof(expected)) == 0, "CopyResource buffer bytes")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER);
  if (!Check(copy_loc.hdr != nullptr, "COPY_BUFFER emitted")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_buffer*>(stream + copy_loc.offset);
  if (!Check(copy_cmd->dst_offset_bytes == 0, "COPY_BUFFER dst_offset_bytes == 0")) {
    return false;
  }
  if (!Check(copy_cmd->src_offset_bytes == 0, "COPY_BUFFER src_offset_bytes == 0")) {
    return false;
  }
  if (!Check(copy_cmd->size_bytes == 16, "COPY_BUFFER size_bytes padded to 16")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSubmitAllocListTracksBoundConstantBuffer() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(track CB alloc)")) {
    return false;
  }

  TestResource cb{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &cb),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(dynamic CB)")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_buffer*>(dev.harness.last_stream.data() + create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE backing = create_cmd->backing_alloc_id;
  if (!Check(backing != 0, "CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  // Flush clears the device's referenced allocation list. Binding the CB should
  // repopulate it before the next submission.
  D3D10DDI_HRESOURCE buffers[1] = {cb.hResource};
  dev.device_funcs.pfnVsSetConstantBuffers(dev.hDevice, /*start_slot=*/0, /*buffer_count=*/1, buffers);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after VsSetConstantBuffers")) {
    return false;
  }

  bool found = false;
  uint8_t found_write = 1;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == backing) {
      found = true;
      found_write = a.write;
      break;
    }
  }
  if (!Check(found, "submit alloc list contains bound constant buffer allocation")) {
    return false;
  }
  if (!Check(found_write == 0, "bound constant buffer allocation should be read-only")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, cb.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceTextureReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(copy tex2d host-owned)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t row_pitch = mapped.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * row_pitch + x] = static_cast<uint8_t>((y + 1) * 19u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &readback);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst tex2d")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(readback.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 19u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * row_pitch + x] == expected, "CopyResource tex2d pixel bytes")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0, "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(copy bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch == row_bytes, "Map RowPitch matches tight BC row bytes (host-owned)")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0, "CopyResource bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0,
               "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedCopySubresourceRegionBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(copy subresource bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch == row_bytes, "Map RowPitch matches tight BC row bytes (host-owned)")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                   dst.hResource,
                                                   /*dst_subresource=*/0,
                                                   /*dst_x=*/0,
                                                   /*dst_y=*/0,
                                                   /*dst_z=*/0,
                                                   src.hResource,
                                                   /*src_subresource=*/0,
                                                   /*pSrcBox=*/nullptr);
    if (!Check(hr == S_OK, "CopySubresourceRegion(bc) returns S_OK")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0,
               "CopySubresourceRegion bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0,
               "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestSubmitAllocListTracksBoundShaderResource() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(track SRV alloc)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(texture)")) {
    return false;
  }

  CmdLoc create_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_texture2d*>(dev.harness.last_stream.data() + create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE backing = create_cmd->backing_alloc_id;
  if (!Check(backing != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  TestShaderResourceView srv{};
  if (!Check(CreateShaderResourceView(&dev, &tex, &srv), "CreateShaderResourceView")) {
    return false;
  }

  D3D10DDI_HSHADERRESOURCEVIEW views[1] = {srv.hView};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*view_count=*/1, views);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after VsSetShaderResources")) {
    return false;
  }

  bool found = false;
  uint8_t found_write = 1;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == backing) {
      found = true;
      found_write = a.write;
      break;
    }
  }
  if (!Check(found, "submit alloc list contains bound shader resource allocation")) {
    return false;
  }
  if (!Check(found_write == 0, "bound shader resource allocation should be read-only")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hView);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

static bool FindSubmitAlloc(const std::vector<AEROGPU_WDDM_SUBMIT_ALLOCATION>& allocs,
                            AEROGPU_WDDM_ALLOCATION_HANDLE handle,
                            uint8_t* out_write) {
  if (out_write) {
    *out_write = 0;
  }
  if (handle == 0) {
    return false;
  }
  for (const auto& a : allocs) {
    if (a.handle == handle) {
      if (out_write) {
        *out_write = a.write;
      }
      return true;
    }
  }
  return false;
}

bool TestSubmitAllocWriteFlagsForDraw() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(draw write flags)")) {
    return false;
  }

  // Create a guest-backed vertex buffer (read-only from the GPU's perspective).
  TestResource vb{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/64,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &vb),
             "CreateBuffer(VB)")) {
    return false;
  }
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateBuffer(VB)")) {
    return false;
  }
  CmdLoc vb_create_loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(vb_create_loc.hdr != nullptr, "CREATE_BUFFER emitted (VB)")) {
    return false;
  }
  const auto* vb_create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_buffer*>(dev.harness.last_stream.data() + vb_create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE vb_alloc = vb_create_cmd->backing_alloc_id;
  if (!Check(vb_alloc != 0, "VB backing_alloc_id != 0")) {
    return false;
  }

  // Create a guest-backed SRV texture (read-only in the draw).
  TestResource srv_tex{};
  if (!Check(CreateTexture2D(&dev,
                             /*width=*/4,
                             /*height=*/4,
                             AEROGPU_D3D11_USAGE_DEFAULT,
                             kD3D11BindShaderResource,
                             /*cpu_access_flags=*/0,
                             &srv_tex),
             "CreateTexture2D(SRV tex)")) {
    return false;
  }
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateTexture2D(SRV tex)")) {
    return false;
  }
  CmdLoc srv_create_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(srv_create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted (SRV tex)")) {
    return false;
  }
  const auto* srv_create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_texture2d*>(dev.harness.last_stream.data() + srv_create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE srv_alloc = srv_create_cmd->backing_alloc_id;
  if (!Check(srv_alloc != 0, "SRV tex backing_alloc_id != 0")) {
    return false;
  }
  TestShaderResourceView srv{};
  if (!Check(CreateShaderResourceView(&dev, &srv_tex, &srv), "CreateShaderResourceView(SRV)")) {
    return false;
  }

  // Create a guest-backed render target texture (written by the draw).
  TestResource rtv_tex{};
  if (!Check(CreateTexture2D(&dev,
                             /*width=*/4,
                             /*height=*/4,
                             AEROGPU_D3D11_USAGE_DEFAULT,
                             kD3D11BindRenderTarget,
                             /*cpu_access_flags=*/0,
                             &rtv_tex),
             "CreateTexture2D(RTV tex)")) {
    return false;
  }
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateTexture2D(RTV tex)")) {
    return false;
  }
  CmdLoc rtv_create_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(rtv_create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted (RTV tex)")) {
    return false;
  }
  const auto* rtv_create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_texture2d*>(dev.harness.last_stream.data() + rtv_create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE rtv_alloc = rtv_create_cmd->backing_alloc_id;
  if (!Check(rtv_alloc != 0, "RTV tex backing_alloc_id != 0")) {
    return false;
  }
  TestRenderTargetView rtv{};
  if (!Check(CreateRenderTargetView(&dev, &rtv_tex, &rtv), "CreateRenderTargetView(RTV)")) {
    return false;
  }

  // Bind state: VB + SRV, and draw into RTV.
  D3D10DDI_HRENDERTARGETVIEW rtvs[1] = {rtv.hView};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice,
                                       /*num_views=*/1,
                                       rtvs,
                                       D3D10DDI_HDEPTHSTENCILVIEW{});
  dev.device_funcs.pfnSetVertexBuffer(dev.hDevice, vb.hResource, /*stride=*/16, /*offset=*/0);
  D3D10DDI_HSHADERRESOURCEVIEW views[1] = {srv.hView};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*view_count=*/1, views);
  dev.device_funcs.pfnDraw(dev.hDevice, /*vertex_count=*/3, /*start_vertex=*/0);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after draw")) {
    return false;
  }

  uint8_t vb_write = 1;
  uint8_t srv_write = 1;
  uint8_t rtv_write = 0;
  if (!Check(FindSubmitAlloc(dev.harness.last_allocs, vb_alloc, &vb_write), "submit alloc list contains VB allocation")) {
    return false;
  }
  if (!Check(FindSubmitAlloc(dev.harness.last_allocs, srv_alloc, &srv_write), "submit alloc list contains SRV allocation")) {
    return false;
  }
  if (!Check(FindSubmitAlloc(dev.harness.last_allocs, rtv_alloc, &rtv_write), "submit alloc list contains RTV allocation")) {
    return false;
  }

  if (!Check(vb_write == 0, "VB allocation should be read-only")) {
    return false;
  }
  if (!Check(srv_write == 0, "SRV allocation should be read-only")) {
    return false;
  }
  if (!Check(rtv_write == 1, "RTV allocation should be marked write")) {
    return false;
  }

  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv.hView);
  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hView);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, rtv_tex.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, srv_tex.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, vb.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceBufferReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(copy buffer)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  const uint8_t expected[16] = {0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F, 0xAA, 0xBB,
                                0xCC, 0xDD, 0xEE, 0xFF, 0x10, 0x20, 0x30, 0x40};

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       src.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) src buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &readback);
  if (!Check(hr == S_OK, "Map(READ) dst buffer")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(std::memcmp(readback.pData, expected, sizeof(expected)) == 0, "CopyResource buffer bytes")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER) == 1, "COPY_BUFFER emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER);
  if (!Check(copy_loc.hdr != nullptr, "COPY_BUFFER location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_buffer*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_BUFFER has WRITEBACK_DST flag")) {
    return false;
  }

  std::vector<uint32_t> backing_ids;
  size_t off = sizeof(aerogpu_cmd_stream_header);
  while (off + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(stream + off);
    if (hdr->opcode == AEROGPU_CMD_CREATE_BUFFER) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + off);
      backing_ids.push_back(cmd->backing_alloc_id);
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - off) {
      break;
    }
    off += hdr->size_bytes;
  }
  if (!Check(backing_ids.size() == 2, "expected exactly 2 CREATE_BUFFER commands")) {
    return false;
  }
  for (uint32_t id : backing_ids) {
    bool found = false;
    for (const auto& a : dev.harness.last_allocs) {
      if (a.handle == id) {
        found = true;
      }
    }
    if (!Check(found, "submit alloc list contains backing allocation")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceTextureReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(copy tex2d)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t row_pitch = mapped.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * row_pitch + x] = static_cast<uint8_t>((y + 1) * 19u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &readback);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst tex2d")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(readback.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 19u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * row_pitch + x] == expected, "CopyResource tex2d pixel bytes")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestClearRtvB5FormatsProduceCorrectReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(clear rtv b5)")) {
    return false;
  }

  constexpr uint32_t kWidth = 3;
  constexpr uint32_t kHeight = 2;

  auto float_to_unorm = [](float v, uint32_t max) -> uint32_t {
    // Mirror the UMD's "ordered comparisons" behavior: treat NaNs as zero.
    if (!(v > 0.0f)) {
      return 0;
    }
    if (v >= 1.0f) {
      return max;
    }
    const float scaled = v * static_cast<float>(max) + 0.5f;
    if (!(scaled > 0.0f)) {
      return 0;
    }
    if (scaled >= static_cast<float>(max)) {
      return max;
    }
    return static_cast<uint32_t>(scaled);
  };

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    float clear_rgba[4];
  };

  auto pack_565 = [&](const float rgba[4]) -> uint16_t {
    const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
    const uint16_t g6 = static_cast<uint16_t>(float_to_unorm(rgba[1], 63));
    const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
    return static_cast<uint16_t>((r5 << 11) | (g6 << 5) | b5);
  };

  auto pack_5551 = [&](const float rgba[4]) -> uint16_t {
    const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
    const uint16_t g5 = static_cast<uint16_t>(float_to_unorm(rgba[1], 31));
    const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
    const uint16_t a1 = static_cast<uint16_t>(float_to_unorm(rgba[3], 1));
    return static_cast<uint16_t>((a1 << 15) | (r5 << 10) | (g5 << 5) | b5);
  };

  const Case kCases[] = {
      {"DXGI_FORMAT_B5G6R5_UNORM", kDxgiFormatB5G6R5Unorm, {1.0f, 0.5f, 0.0f, 1.0f}},
      {"DXGI_FORMAT_B5G5R5A1_UNORM", kDxgiFormatB5G5R5A1Unorm, {0.25f, 0.5f, 1.0f, 0.6f}},
  };

  for (const Case& c : kCases) {
    TestResource rt{};
    if (!Check(CreateTexture2D(&dev,
                               /*width=*/kWidth,
                               /*height=*/kHeight,
                               AEROGPU_D3D11_USAGE_DEFAULT,
                               kD3D11BindRenderTarget,
                               /*cpu_access_flags=*/0,
                               c.dxgi_format,
                               &rt),
               "CreateTexture2D(render target)")) {
      return false;
    }

    TestRenderTargetView rtv{};
    if (!Check(CreateRenderTargetView(&dev, &rt, &rtv), "CreateRenderTargetView")) {
      return false;
    }

    TestResource staging{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &staging),
               "CreateStagingTexture2DWithFormat(readback)")) {
      return false;
    }

    dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/1, &rtv.hView, D3D10DDI_HDEPTHSTENCILVIEW{});
    dev.device_funcs.pfnClearRTV(dev.hDevice, rtv.hView, c.clear_rgba);

    dev.device_funcs.pfnCopyResource(dev.hDevice, staging.hResource, rt.hResource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        staging.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_READ,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) after ClearRTV + CopyResource")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch > kWidth * 2, "RowPitch should include padding for guest-backed B5 texture")) {
      return false;
    }

    uint16_t expected = 0;
    if (c.dxgi_format == kDxgiFormatB5G6R5Unorm) {
      expected = pack_565(c.clear_rgba);
    } else if (c.dxgi_format == kDxgiFormatB5G5R5A1Unorm) {
      expected = pack_5551(c.clear_rgba);
    } else {
      return false;
    }
    const uint8_t* bytes = static_cast<const uint8_t*>(mapped.pData);
    const uint32_t pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < kHeight; ++y) {
      for (uint32_t x = 0; x < kWidth; ++x) {
        uint16_t actual = 0;
        std::memcpy(&actual, bytes + static_cast<size_t>(y) * pitch + static_cast<size_t>(x) * 2, sizeof(actual));
        if (!Check(actual == expected, c.name)) {
          return false;
        }
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, staging.hResource, /*subresource=*/0);

    dev.device_funcs.pfnDestroyResource(dev.hDevice, staging.hResource);
    dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv.hView);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, rt.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc guest-backed)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc guest-backed)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch >= row_bytes, "Map RowPitch >= tight BC row bytes")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
      // Leave padding bytes untouched (they are initially zero); expected remains zero.
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0, "CopyResource bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedCopySubresourceRegionBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy subresource bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc guest-backed)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc guest-backed)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch >= row_bytes, "Map RowPitch >= tight BC row bytes")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                   dst.hResource,
                                                   /*dst_subresource=*/0,
                                                   /*dst_x=*/0,
                                                   /*dst_y=*/0,
                                                   /*dst_z=*/0,
                                                   src.hResource,
                                                   /*src_subresource=*/0,
                                                   /*pSrcBox=*/nullptr);
    if (!Check(hr == S_OK, "CopySubresourceRegion(bc guest-backed) returns S_OK")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0,
               "CopySubresourceRegion bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP buffer host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t expected[16] = {0x00, 0x02, 0x04, 0x06, 0x10, 0x20, 0x30, 0x40,
                                0x55, 0x66, 0x77, 0x88, 0x99, 0xAB, 0xBC, 0xCD};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          expected,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size_bytes matches")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(expected) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, sizeof(expected)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPBufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP buffer guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t expected[16] = {0xF0, 0xE1, 0xD2, 0xC3, 0xB4, 0xA5, 0x96, 0x87,
                                0x78, 0x69, 0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          expected,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size_bytes matches")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPTextureUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> sysmem(static_cast<size_t>(bytes_per_row) * height);
  for (uint32_t i = 0; i < sysmem.size(); i++) {
    sysmem[i] = static_cast<uint8_t>(0x40u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == bytes_per_row, "CREATE_TEXTURE2D row_pitch_bytes tight")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sysmem.size(), "UPLOAD_RESOURCE size_bytes matches")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sysmem.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, sysmem.data(), sysmem.size()) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPTexture2DMipArrayUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP mip+array tex2d host-owned)")) {
    return false;
  }

  static constexpr uint32_t kWidth = 4;
  static constexpr uint32_t kHeight = 4;
  static constexpr uint32_t kMipLevels = 3;
  static constexpr uint32_t kArraySize = 2;

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     kWidth,
                                                     kHeight,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/0,
                                                     kMipLevels,
                                                     kArraySize,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(mip+array)")) {
    return false;
  }

  // subresource=4 corresponds to mip1 layer1 when mip_levels=3.
  const uint32_t subresource = 4;
  const uint32_t mip1_row_bytes = DxgiTextureMinRowPitchBytes(kDxgiFormatB8G8R8A8Unorm, /*width=*/2);
  const uint32_t mip1_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, /*height=*/2);
  const size_t mip1_size = static_cast<size_t>(mip1_row_bytes) * static_cast<size_t>(mip1_rows);

  std::vector<uint8_t> sysmem(mip1_size);
  for (size_t i = 0; i < sysmem.size(); ++i) {
    sysmem[i] = static_cast<uint8_t>(0xA0u + (i & 0x3Fu));
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          subresource,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/mip1_row_bytes,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d UpdateSubresourceUP(mip+array) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d UpdateSubresourceUP(mip+array) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "CREATE_TEXTURE2D backing_alloc_id == 0 (host-owned)")) {
    return false;
  }
  if (!Check(create_cmd->mip_levels == kMipLevels, "CREATE_TEXTURE2D mip_levels matches")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == kArraySize, "CREATE_TEXTURE2D array_layers matches")) {
    return false;
  }

  // Validate the upload offset matches the expected mip-major layout within each array layer.
  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint32_t mip0_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, kHeight);
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * static_cast<uint64_t>(mip0_rows);

  const uint32_t mip1_row_pitch = DxgiTextureMinRowPitchBytes(kDxgiFormatB8G8R8A8Unorm, 2);
  const uint32_t mip1_rows2 = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, 2);
  const uint64_t mip1_size_u64 = static_cast<uint64_t>(mip1_row_pitch) * static_cast<uint64_t>(mip1_rows2);

  const uint32_t mip2_row_pitch = DxgiTextureMinRowPitchBytes(kDxgiFormatB8G8R8A8Unorm, 1);
  const uint32_t mip2_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, 1);
  const uint64_t mip2_size = static_cast<uint64_t>(mip2_row_pitch) * static_cast<uint64_t>(mip2_rows);

  const uint64_t layer_stride = mip0_size + mip1_size_u64 + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == expected_offset, "UPLOAD_RESOURCE offset_bytes matches subresource layout")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == mip1_size_u64, "UPLOAD_RESOURCE size_bytes matches subresource size")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(payload_size == sysmem.size(), "UPLOAD_RESOURCE payload size == sysmem size")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, sysmem.data(), sysmem.size()) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTextureDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> sysmem(static_cast<size_t>(bytes_per_row) * height);
  for (uint32_t i = 0; i < sysmem.size(); i++) {
    sysmem[i] = static_cast<uint8_t>(0x90u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }

  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes includes padding")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed tex2d submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(total_bytes, 0);
  for (uint32_t y = 0; y < height; y++) {
    std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                sysmem.data() + static_cast<size_t>(y) * bytes_per_row,
                bytes_per_row);
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPBcTextureUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc)")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t total_bytes = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> sysmem(total_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x40u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            /*pDstBox=*/nullptr,
                                            sysmem.data(),
                                            /*SysMemPitch=*/row_bytes,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(bc)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned bc tex2d UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned bc tex2d UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == row_bytes, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == sysmem.size(), "UPLOAD_RESOURCE size_bytes matches")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + sysmem.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, sysmem.data(), sysmem.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(bc) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc guest-backed)")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t sysmem_size = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> sysmem(sysmem_size);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x90u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            /*pDstBox=*/nullptr,
                                            sysmem.data(),
                                            /*SysMemPitch=*/row_bytes,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(bc guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes >= row_bytes, "CREATE_TEXTURE2D row_pitch_bytes >= row_bytes")) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const size_t total_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches BC bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (const auto& a : dev.harness.last_allocs) {
      if (a.handle == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed bc tex2d submit alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(total_bytes, 0);
    for (uint32_t y = 0; y < blocks_h; y++) {
      std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                  sysmem.data() + static_cast<size_t>(y) * row_bytes,
                  row_bytes);
    }
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBufferBoxUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box buffer host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t patch[8] = {0xDE, 0xC0, 0xAD, 0xDE, 0xBE, 0xEF, 0xCA, 0xFE};
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = 12;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          patch,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 4, "UPLOAD_RESOURCE offset_bytes matches box.left")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(patch), "UPLOAD_RESOURCE size_bytes matches box span")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(patch) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, patch, sizeof(patch)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(box) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPBufferBoxUnalignedPadsTo4() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP unaligned box buffer host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t patch[5] = {0xDE, 0xC0, 0xAD, 0xBE, 0xEF};
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 6;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          patch,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned UpdateSubresourceUP(unaligned box) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes aligned down to 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == 8, "UPLOAD_RESOURCE size_bytes aligned up to 8")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }

  uint8_t expected[8] = {};
  expected[0] = 0;
  std::memcpy(expected + 1, patch, sizeof(patch));
  expected[6] = 0;
  expected[7] = 0;
  if (!Check(std::memcmp(stream + payload_offset, expected, sizeof(expected)) == 0,
             "UPLOAD_RESOURCE payload padded/aligned bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPTextureBoxUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev,
                                    /*width=*/3,
                                    /*height=*/2,
                                    /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_READ,
                                    &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;

  // Seed the texture with non-zero data so the box update must preserve bytes
  // outside the box.
  std::vector<uint8_t> initial(bytes_per_row * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x10u + (i & 0x7Fu));
  }
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          initial.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(initial tex)")) {
    return false;
  }

  // Update only the second row.
  uint8_t row[12] = {};
  for (uint32_t i = 0; i < sizeof(row); i++) {
    row[i] = static_cast<uint8_t>(0xA0u + i);
  }

  AEROGPU_DDI_BOX box{};
  box.left = 0;
  box.right = width;
  box.top = 1;
  box.bottom = 2;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          row,
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == bytes_per_row, "UPLOAD_RESOURCE offset_bytes == RowPitch*top")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(row), "UPLOAD_RESOURCE size_bytes matches one row")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(row) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, row, sizeof(row)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d UpdateSubresourceUP(box) alloc list empty")) {
    return false;
  }

  // Validate CPU-visible storage (Map) matches initial data, with the second row
  // replaced by the box upload bytes.
  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               tex.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &mapped);
  if (!Check(hr == S_OK, "Map(READ) after UpdateSubresourceUP(box)")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch >= bytes_per_row, "Map(READ) RowPitch >= bytes_per_row")) {
    dev.device_funcs.pfnUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
    return false;
  }

  std::vector<uint8_t> expected = initial;
  std::memcpy(expected.data() + bytes_per_row, row, sizeof(row));

  const auto* mapped_bytes = static_cast<const uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    const size_t src_off = static_cast<size_t>(y) * mapped.RowPitch;
    const size_t exp_off = static_cast<size_t>(y) * bytes_per_row;
    if (!Check(std::memcmp(mapped_bytes + src_off, expected.data() + exp_off, bytes_per_row) == 0, "Mapped tex bytes")) {
      dev.device_funcs.pfnUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
      return false;
    }
    // Padding should remain deterministic (zero) for full-row updates.
    for (uint32_t x = bytes_per_row; x < mapped.RowPitch; x++) {
      if (!Check(mapped_bytes[src_off + x] == 0, "Mapped row padding is zero")) {
        dev.device_funcs.pfnUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
        return false;
      }
    }
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPBufferBoxDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box buffer guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t patch[8] = {0x11, 0x33, 0x55, 0x77, 0x99, 0xBB, 0xDD, 0xFF};
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = 12;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          patch,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == 16, "RESOURCE_DIRTY_RANGE size_bytes == full buffer")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= 16, "backing allocation large enough")) {
    return false;
  }

  uint8_t expected[16] = {};
  std::memcpy(expected + 4, patch, sizeof(patch));
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTextureBoxDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;

  // Seed the texture with non-zero data so the box update must preserve bytes
  // outside the box.
  std::vector<uint8_t> initial(bytes_per_row * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x80u + (i & 0x7Fu));
  }
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          initial.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(initial guest tex)")) {
    return false;
  }
  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream(initial guest tex)")) {
    return false;
  }
  const uint8_t* init_stream = dev.harness.last_stream.data();
  const size_t init_stream_len = StreamBytesUsed(init_stream, dev.harness.last_stream.size());
  CmdLoc create_loc = FindLastOpcode(init_stream, init_stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted (initial guest tex)")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(init_stream + create_loc.offset);
  const uint32_t backing_alloc_id = create_cmd->backing_alloc_id;
  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  if (!Check(backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0 (initial guest tex)")) {
    return false;
  }
  if (!Check(row_pitch != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;

  const uint8_t pixel[4] = {0x10, 0x20, 0x30, 0x40};
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 2;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          pixel,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes == full texture bytes")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(total_bytes, 0);
  for (uint32_t y = 0; y < height; y++) {
    const size_t src_off = static_cast<size_t>(y) * bytes_per_row;
    const size_t dst_off = static_cast<size_t>(y) * row_pitch;
    std::memcpy(expected.data() + dst_off, initial.data() + src_off, bytes_per_row);
  }
  const size_t dst_offset = 0u * row_pitch + 1u * 4u;
  std::memcpy(expected.data() + dst_offset, pixel, sizeof(pixel));
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPBcTextureBoxUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP box bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
  };

  // Upload the bottom-right 4x4 block (aligned left/top, edge-aligned right/bottom).
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = kWidth;
  box.top = 4;
  box.bottom = kHeight;
  box.front = 0;
  box.back = 1;

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc box)")) {
      return false;
    }

    std::vector<uint8_t> sysmem(c.block_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x55u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            &box,
                                            sysmem.data(),
                                            /*SysMemPitch=*/0,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(box bc)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned bc tex2d UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned bc tex2d UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const uint32_t expected_row_pitch = 2u * c.block_bytes;
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(row_pitch == expected_row_pitch, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == row_pitch, "UPLOAD_RESOURCE offset_bytes == row_pitch (second block row)")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == row_pitch, "UPLOAD_RESOURCE size_bytes == row_pitch (one block row)")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + static_cast<size_t>(row_pitch) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(row_pitch), 0);
    // block_left=1 => offset = block_bytes
    std::memcpy(expected.data() + c.block_bytes, sysmem.data(), sysmem.size());
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match expected for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(box bc) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureBoxDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP box bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
  };

  // Upload the bottom-right 4x4 block (aligned left/top, edge-aligned right/bottom).
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = kWidth;
  box.top = 4;
  box.bottom = kHeight;
  box.front = 0;
  box.back = 1;

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc guest-backed box)")) {
      return false;
    }

    std::vector<uint8_t> sysmem(c.block_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x99u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            &box,
                                            sysmem.data(),
                                            /*SysMemPitch=*/0,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(box bc guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const uint32_t blocks_h = 2;
    const size_t total_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes == full texture bytes")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(total_bytes, 0);
    const size_t dst_offset = 1u * static_cast<size_t>(row_pitch) + c.block_bytes;
    std::memcpy(expected.data() + dst_offset, sysmem.data(), sysmem.size());
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBcTextureBoxRejectsMisaligned() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP invalid box bc tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              /*width=*/5,
                                              /*height=*/5,
                                              kDxgiFormatBc7Unorm,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(BC7)")) {
    return false;
  }

  // Misaligned left (must be multiple of 4 for BC formats).
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 5;
  box.top = 0;
  box.bottom = 4;
  box.front = 0;
  box.back = 1;

  const uint8_t junk[16] = {};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          junk,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(invalid bc box)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D) == 1, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureBoxRejectsMisaligned() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP invalid box bc tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              /*width=*/5,
                                              /*height=*/5,
                                              kDxgiFormatBc7Unorm,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(BC7 guest-backed)")) {
    return false;
  }

  // Misaligned left (must be multiple of 4 for BC formats).
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 5;
  box.top = 0;
  box.bottom = 4;
  box.front = 0;
  box.back = 1;

  const uint8_t junk[16] = {};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          junk,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(invalid bc box guest-backed)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D) == 1, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedCreateBufferInitialDataUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(CreateResource initial buffer host-owned)")) {
    return false;
  }

  const uint8_t initial[16] = {0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
                               0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE};

  TestResource buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(initial),
                                         AEROGPU_D3D11_USAGE_DEFAULT,
                                         /*bind_flags=*/0,
                                         /*cpu_access_flags=*/0,
                                         initial,
                                         &buf),
             "CreateBufferWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned CreateResource(initial) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned CreateResource(initial) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(initial), "UPLOAD_RESOURCE size_bytes matches initial buffer")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(initial) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, initial, sizeof(initial)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCreateBufferInitialDataPadsTo4() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(CreateResource initial buffer host-owned padded)")) {
    return false;
  }

  const uint8_t initial[15] = {0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
                               0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC};
  uint8_t expected_payload[16] = {};
  std::memcpy(expected_payload, initial, sizeof(initial));
  expected_payload[15] = 0;

  TestResource buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(initial),
                                         AEROGPU_D3D11_USAGE_DEFAULT,
                                         /*bind_flags=*/0,
                                         /*cpu_access_flags=*/0,
                                         initial,
                                         &buf),
             "CreateBufferWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->size_bytes == 16, "CREATE_BUFFER size_bytes padded to 16")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == 16, "UPLOAD_RESOURCE size_bytes padded to 16")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(expected_payload) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected_payload, sizeof(expected_payload)) == 0,
             "UPLOAD_RESOURCE payload bytes padded")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateBufferInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(CreateResource initial buffer guest-backed)")) {
    return false;
  }

  const uint8_t initial[16] = {0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10,
                               0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01};

  TestResource buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(initial),
                                         AEROGPU_D3D11_USAGE_DEFAULT,
                                         /*bind_flags=*/0,
                                         /*cpu_access_flags=*/0,
                                         initial,
                                         &buf),
             "CreateBufferWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(initial) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(initial) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(initial), "RESOURCE_DIRTY_RANGE size_bytes matches initial buffer")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed CreateResource(initial) alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(initial), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), initial, sizeof(initial)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateBufferSrvUavBindsMarkStorageUsage() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(CreateResource buffer storage usage)")) {
    return false;
  }

  auto check_bind = [&](uint32_t bind_flags, const char* label) -> bool {
    TestResource buf{};
    if (!Check(CreateBuffer(&dev,
                            /*byte_width=*/16,
                            AEROGPU_D3D11_USAGE_DEFAULT,
                            bind_flags,
                            /*cpu_access_flags=*/0,
                            &buf),
               label)) {
      return false;
    }

    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(buffer)")) {
      return false;
    }
    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
    if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
    if (!Check((create_cmd->usage_flags & AEROGPU_RESOURCE_USAGE_STORAGE) != 0,
               "CREATE_BUFFER usage_flags includes STORAGE")) {
      return false;
    }
    if (!Check((create_cmd->usage_flags & AEROGPU_RESOURCE_USAGE_TEXTURE) == 0,
               "CREATE_BUFFER usage_flags does not include TEXTURE")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
    return true;
  };

  bool ok = true;
  ok &= check_bind(kD3D11BindShaderResource, "CreateBuffer(SRV bind flag)");
  ok &= check_bind(kD3D11BindUnorderedAccess, "CreateBuffer(UAV bind flag)");

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return ok;
}

bool TestHostOwnedCreateTextureInitialDataUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(CreateResource initial tex2d host-owned)")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> initial(static_cast<size_t>(bytes_per_row) * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x11u + i);
  }

  TestResource tex{};
  if (!Check(CreateTexture2DWithInitialData(&dev,
                                            width,
                                            height,
                                            AEROGPU_D3D11_USAGE_DEFAULT,
                                            /*bind_flags=*/0,
                                            /*cpu_access_flags=*/0,
                                            initial.data(),
                                            bytes_per_row,
                                            &tex),
             "CreateTexture2DWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned CreateResource(initial tex2d) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned CreateResource(initial tex2d) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == bytes_per_row, "CREATE_TEXTURE2D row_pitch_bytes tight")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == initial.size(), "UPLOAD_RESOURCE size_bytes matches initial tex2d")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + initial.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, initial.data(), initial.size()) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial tex2d) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateTextureInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(CreateResource initial tex2d guest-backed)")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> initial(static_cast<size_t>(bytes_per_row) * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x80u + i);
  }

  TestResource tex{};
  if (!Check(CreateTexture2DWithInitialData(&dev,
                                            width,
                                            height,
                                            AEROGPU_D3D11_USAGE_DEFAULT,
                                            /*bind_flags=*/0,
                                            /*cpu_access_flags=*/0,
                                            initial.data(),
                                            bytes_per_row,
                                            &tex),
             "CreateTexture2DWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(initial tex2d) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(initial tex2d) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }

  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  const size_t dirty_bytes = static_cast<size_t>(row_pitch) * height;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == dirty_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches initial tex2d bytes")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed CreateResource(initial tex2d) alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= dirty_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(dirty_bytes, 0);
  for (uint32_t y = 0; y < height; y++) {
    std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                initial.data() + static_cast<size_t>(y) * bytes_per_row,
                bytes_per_row);
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCreateBcTextureInitialDataUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(CreateResource initial BC tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t total_bytes = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> initial(total_bytes);
    for (size_t i = 0; i < initial.size(); i++) {
      initial[i] = static_cast<uint8_t>(0x11u + (i & 0x3Fu));
    }

    TestResource tex{};
    if (!Check(CreateTexture2DWithInitialData(&dev,
                                              kWidth,
                                              kHeight,
                                              AEROGPU_D3D11_USAGE_DEFAULT,
                                              /*bind_flags=*/0,
                                              /*cpu_access_flags=*/0,
                                              initial.data(),
                                              row_bytes,
                                              &tex,
                                              c.dxgi_format),
               "CreateTexture2DWithInitialData(BC)")) {
      return false;
    }

    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(initial BC tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned CreateResource(initial BC tex2d) should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned CreateResource(initial BC tex2d) should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == row_bytes, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == initial.size(), "UPLOAD_RESOURCE size_bytes matches initial BC tex2d")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + initial.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, initial.data(), initial.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial BC) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedCreateBcTextureInitialDataDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(CreateResource initial BC tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t initial_size = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> initial(initial_size);
    for (size_t i = 0; i < initial.size(); i++) {
      initial[i] = static_cast<uint8_t>(0x80u + (i & 0x3Fu));
    }

    TestResource tex{};
    if (!Check(CreateTexture2DWithInitialData(&dev,
                                              kWidth,
                                              kHeight,
                                              AEROGPU_D3D11_USAGE_DEFAULT,
                                              /*bind_flags=*/0,
                                              /*cpu_access_flags=*/0,
                                              initial.data(),
                                              row_bytes,
                                              &tex,
                                              c.dxgi_format),
               "CreateTexture2DWithInitialData(BC guest-backed)")) {
      return false;
    }

    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(initial BC tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed CreateResource(initial BC tex2d) should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed CreateResource(initial BC tex2d) should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const size_t dirty_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == dirty_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches BC tex2d bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (const auto& a : dev.harness.last_allocs) {
      if (a.handle == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed CreateResource(initial BC) alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= dirty_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(dirty_bytes, 0);
    for (uint32_t y = 0; y < blocks_h; y++) {
      std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                  initial.data() + static_cast<size_t>(y) * row_bytes,
                  row_bytes);
    }
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestSrgbTexture2DFormatPropagation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(srgb format propagation)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
  };

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_B8G8R8A8_UNORM_SRGB", kDxgiFormatB8G8R8A8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB
#else
       AEROGPU_FORMAT_B8G8R8A8_UNORM
#endif
      },
      {"DXGI_FORMAT_B8G8R8X8_UNORM_SRGB", kDxgiFormatB8G8R8X8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
#else
       AEROGPU_FORMAT_B8G8R8X8_UNORM
#endif
      },
      {"DXGI_FORMAT_R8G8B8A8_UNORM_SRGB", kDxgiFormatR8G8B8A8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB
#else
       AEROGPU_FORMAT_R8G8B8A8_UNORM
#endif
      },
  };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/4,
                                                /*height=*/4,
                                                c.dxgi_format,
                                                // Staging textures require CPU access flags in real D3D11; keep the
                                                // descriptor valid so this test doesn't start failing if stricter
                                                // CreateResource validation is added later.
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &tex),
               "CreateStagingTexture2DWithFormat(srgb)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(srgb tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrgbTexture2DFormatPropagationGuestBacked() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(srgb format propagation guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
  };

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_B8G8R8A8_UNORM_SRGB", kDxgiFormatB8G8R8A8UnormSrgb, AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB},
      {"DXGI_FORMAT_B8G8R8X8_UNORM_SRGB", kDxgiFormatB8G8R8X8UnormSrgb, AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB},
      {"DXGI_FORMAT_R8G8B8A8_UNORM_SRGB", kDxgiFormatR8G8B8A8UnormSrgb, AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB},
  };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/4,
                                                /*height=*/4,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &tex),
               "CreateStagingTexture2DWithFormat(srgb guest-backed)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(srgb tex2d guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }

    bool found = false;
    for (const auto& a : dev.harness.last_allocs) {
      if (a.handle == create_cmd->backing_alloc_id) {
        found = true;
        break;
      }
    }
    if (!Check(found, "submit alloc list contains guest-backed sRGB texture allocation")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTexture2DMipArrayCreateEncodesMipAndArray() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(mip+array create guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/0,
                                                     /*mip_levels=*/0, // full chain
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(mip+array)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(mip+array tex2d)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->width == 4, "CREATE_TEXTURE2D width == 4")) {
    return false;
  }
  if (!Check(create_cmd->height == 4, "CREATE_TEXTURE2D height == 4")) {
    return false;
  }
  if (!Check(create_cmd->mip_levels == 3, "CREATE_TEXTURE2D mip_levels full chain (4x4 => 3)")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == 2, "CREATE_TEXTURE2D array_layers == 2")) {
    return false;
  }
  const uint32_t expected_row_pitch = static_cast<uint32_t>(AlignUp(static_cast<size_t>(4u * 4u), 64));
  if (!Check(create_cmd->row_pitch_bytes == expected_row_pitch, "CREATE_TEXTURE2D row_pitch_bytes (mip0)")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  bool found_alloc = false;
  for (const auto& a : dev.harness.last_allocs) {
    if (a.handle == create_cmd->backing_alloc_id) {
      found_alloc = true;
      break;
    }
  }
  if (!Check(found_alloc, "submit alloc list contains mip+array backing allocation")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateTexture2DMipArrayInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(CreateResource initial mip+array tex2d guest-backed)")) {
    return false;
  }

  static constexpr uint32_t kWidth = 4;
  static constexpr uint32_t kHeight = 4;
  static constexpr uint32_t kMipLevels = 3;
  static constexpr uint32_t kArraySize = 2;

  struct SubInit {
    std::vector<uint8_t> bytes;
    uint32_t row_bytes = 0;
    uint32_t height = 0;
  };

  std::vector<SubInit> sub_inits;
  std::vector<AEROGPU_DDI_SUBRESOURCE_DATA> inits;
  sub_inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);
  inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);

  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      SubInit sub{};
      sub.row_bytes = level_w * 4u;
      sub.height = level_h;
      sub.bytes.resize(static_cast<size_t>(sub.row_bytes) * static_cast<size_t>(sub.height));
      const uint8_t seed = static_cast<uint8_t>(0x40u + layer * 0x20u + mip * 0x08u);
      for (size_t i = 0; i < sub.bytes.size(); ++i) {
        sub.bytes[i] = static_cast<uint8_t>(seed + (i & 0x7u));
      }
      sub_inits.push_back(std::move(sub));

      AEROGPU_DDI_SUBRESOURCE_DATA init = {};
      init.pSysMem = sub_inits.back().bytes.data();
      init.SysMemPitch = sub_inits.back().row_bytes;
      init.SysMemSlicePitch = 0;
      inits.push_back(init);

      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = kMipLevels;
  desc.ArraySize = kArraySize;
  desc.Format = kDxgiFormatB8G8R8A8Unorm;
  desc.pInitialData = inits.data();
  desc.InitialDataCount = static_cast<uint32_t>(inits.size());

  TestResource tex{};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateResourceSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }
  tex.storage.assign(static_cast<size_t>(size), 0);
  tex.hResource.pDrvPrivate = tex.storage.data();

  HRESULT hr = dev.device_funcs.pfnCreateResource(dev.hDevice, &desc, tex.hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d mip+array initial data)")) {
    return false;
  }

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(mip+array initial data)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(mip+array initial tex2d) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(mip+array initial tex2d) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == kMipLevels, "CREATE_TEXTURE2D mip_levels matches")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == kArraySize, "CREATE_TEXTURE2D array_layers matches")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * kHeight;
  const uint64_t mip1_size = static_cast<uint64_t>((kWidth / 2) * 4u) * (kHeight / 2);
  const uint64_t mip2_size = static_cast<uint64_t>(4u); // 1x1 RGBA8
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t total_bytes = layer_stride * kArraySize;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE covers full mip+array chain")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(static_cast<size_t>(total_bytes), 0);
  size_t init_index = 0;
  size_t dst_offset = 0;
  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      const uint32_t src_pitch = inits[init_index].SysMemPitch;
      const uint32_t dst_pitch = (mip == 0) ? row_pitch0 : src_pitch;
      const uint32_t row_bytes = src_pitch;
      const size_t sub_size = static_cast<size_t>(dst_pitch) * static_cast<size_t>(level_h);
      for (uint32_t y = 0; y < level_h; ++y) {
        std::memcpy(expected.data() + dst_offset + static_cast<size_t>(y) * dst_pitch,
                    static_cast<const uint8_t*>(inits[init_index].pSysMem) + static_cast<size_t>(y) * src_pitch,
                    row_bytes);
      }
      dst_offset += sub_size;
      init_index++;
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
             "backing allocation bytes match all subresource initial data")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCreateTexture2DMipArrayInitialDataUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(CreateResource initial mip+array tex2d host-owned)")) {
    return false;
  }

  static constexpr uint32_t kWidth = 4;
  static constexpr uint32_t kHeight = 4;
  static constexpr uint32_t kMipLevels = 3;
  static constexpr uint32_t kArraySize = 2;

  struct SubInit {
    std::vector<uint8_t> bytes;
    uint32_t row_bytes = 0;
    uint32_t height = 0;
  };

  std::vector<SubInit> sub_inits;
  std::vector<AEROGPU_DDI_SUBRESOURCE_DATA> inits;
  sub_inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);
  inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);

  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      SubInit sub{};
      sub.row_bytes = level_w * 4u;
      sub.height = level_h;
      sub.bytes.resize(static_cast<size_t>(sub.row_bytes) * static_cast<size_t>(sub.height));
      const uint8_t seed = static_cast<uint8_t>(0x10u + layer * 0x40u + mip * 0x08u);
      for (size_t i = 0; i < sub.bytes.size(); ++i) {
        sub.bytes[i] = static_cast<uint8_t>(seed + (i & 0x7u));
      }
      sub_inits.push_back(std::move(sub));

      AEROGPU_DDI_SUBRESOURCE_DATA init = {};
      init.pSysMem = sub_inits.back().bytes.data();
      init.SysMemPitch = sub_inits.back().row_bytes;
      init.SysMemSlicePitch = 0;
      inits.push_back(init);

      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = kMipLevels;
  desc.ArraySize = kArraySize;
  desc.Format = kDxgiFormatB8G8R8A8Unorm;
  desc.pInitialData = inits.data();
  desc.InitialDataCount = static_cast<uint32_t>(inits.size());

  TestResource tex{};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateResourceSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }
  tex.storage.assign(static_cast<size_t>(size), 0);
  tex.hResource.pDrvPrivate = tex.storage.data();

  HRESULT hr = dev.device_funcs.pfnCreateResource(dev.hDevice, &desc, tex.hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d mip+array initial data host-owned)")) {
    return false;
  }

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(mip+array initial data host-owned)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned CreateResource(mip+array initial tex2d) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned CreateResource(mip+array initial tex2d) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == kMipLevels, "CREATE_TEXTURE2D mip_levels matches")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == kArraySize, "CREATE_TEXTURE2D array_layers matches")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id == 0, "CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == kWidth * 4u, "CREATE_TEXTURE2D mip0 row_pitch_bytes is tight")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * kHeight;
  const uint64_t mip1_size = static_cast<uint64_t>((kWidth / 2) * 4u) * (kHeight / 2);
  const uint64_t mip2_size = static_cast<uint64_t>(4u); // 1x1 RGBA8
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t total_bytes = layer_stride * kArraySize;

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == total_bytes, "UPLOAD_RESOURCE covers full mip+array chain")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }

  std::vector<uint8_t> expected(static_cast<size_t>(total_bytes), 0);
  size_t init_index = 0;
  size_t dst_offset = 0;
  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      const uint32_t src_pitch = inits[init_index].SysMemPitch;
      const uint32_t dst_pitch = (mip == 0) ? row_pitch0 : src_pitch;
      const uint32_t row_bytes = src_pitch;
      const size_t sub_size = static_cast<size_t>(dst_pitch) * static_cast<size_t>(level_h);
      for (uint32_t y = 0; y < level_h; ++y) {
        std::memcpy(expected.data() + dst_offset + static_cast<size_t>(y) * dst_pitch,
                    static_cast<const uint8_t*>(inits[init_index].pSysMem) + static_cast<size_t>(y) * src_pitch,
                    row_bytes);
      }
      dst_offset += sub_size;
      init_index++;
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  if (!Check(std::memcmp(stream + payload_offset, expected.data(), expected.size()) == 0,
             "UPLOAD_RESOURCE payload bytes match all subresource initial data")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(mip+array) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedDynamicTexture2DMipArrayMapUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(dynamic mip+array map/unmap host-owned)")) {
    return false;
  }

  constexpr uint32_t kWidth = 4;
  constexpr uint32_t kHeight = 4;
  constexpr uint32_t kMipLevels = 3;
  constexpr uint32_t kArraySize = 2;

  TestResource tex{};
  if (!Check(CreateDynamicTexture2DWithFormatAndDesc(&dev,
                                                     kWidth,
                                                     kHeight,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     kMipLevels,
                                                     kArraySize,
                                                     &tex),
             "CreateDynamicTexture2DWithFormatAndDesc(mip+array)")) {
    return false;
  }

  const uint32_t subresource = 4; // mip1 layer1 when mip_levels=3.

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       tex.hResource,
                                       subresource,
                                       AEROGPU_DDI_MAP_WRITE_DISCARD,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE_DISCARD) host-owned dynamic mip+array")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }

  uint8_t expected[16] = {};
  for (size_t i = 0; i < sizeof(expected); ++i) {
    expected[i] = static_cast<uint8_t>(0xE0u + i);
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));

  dev.device_funcs.pfnUnmap(dev.hDevice, tex.hResource, subresource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap(dynamic mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned dynamic mip+array Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned dynamic mip+array Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == kMipLevels, "CREATE_TEXTURE2D mip_levels matches")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == kArraySize, "CREATE_TEXTURE2D array_layers matches")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id == 0, "CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }

  // subresource=4 corresponds to mip1 of array layer 1 when mip_levels=3 (mip-major within each layer).
  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint32_t mip0_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, kHeight);
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * static_cast<uint64_t>(mip0_rows);

  const uint32_t mip1_row_pitch = DxgiTextureMinRowPitchBytes(kDxgiFormatB8G8R8A8Unorm, 2);
  const uint32_t mip1_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, 2);
  const uint64_t mip1_size = static_cast<uint64_t>(mip1_row_pitch) * static_cast<uint64_t>(mip1_rows);

  const uint32_t mip2_row_pitch = DxgiTextureMinRowPitchBytes(kDxgiFormatB8G8R8A8Unorm, 1);
  const uint32_t mip2_rows = DxgiTextureNumRows(kDxgiFormatB8G8R8A8Unorm, 1);
  const uint64_t mip2_size = static_cast<uint64_t>(mip2_row_pitch) * static_cast<uint64_t>(mip2_rows);

  if (!Check(mapped.RowPitch == mip1_row_pitch, "Map RowPitch matches mip1 tight layout")) {
    return false;
  }
  if (!Check(mapped.DepthPitch == mip1_size, "Map DepthPitch == subresource size")) {
    return false;
  }

  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == expected_offset, "UPLOAD_RESOURCE offset matches subresource layout")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches subresource layout")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits in stream")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned dynamic mip+array submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTexture2DMipArrayMapUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(mip+array map/unmap guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(map/unmap mip+array)")) {
    return false;
  }

  const uint32_t subresource = 4; // mip1 layer1 when mip_levels=3.

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      subresource,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed mip+array")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 8, "Map RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped.DepthPitch == 16, "Map DepthPitch == RowPitch*height")) {
    return false;
  }

  uint8_t expected[16] = {};
  for (size_t i = 0; i < sizeof(expected); ++i) {
    expected[i] = static_cast<uint8_t>(0xD0u + i);
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  void* mapped_ptr = mapped.pData;

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, subresource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after StagingResourceUnmap(mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "mip+array Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == 3, "CREATE_TEXTURE2D mip_levels == 3")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == 2, "CREATE_TEXTURE2D array_layers == 2")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * 4u;
  const uint64_t mip1_size = static_cast<uint64_t>(8u) * 2u;
  const uint64_t mip2_size = 4u;
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == expected_offset, "RESOURCE_DIRTY_RANGE offset matches subresource layout")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches subresource layout")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected_offset + sizeof(expected), "backing allocation large enough")) {
    return false;
  }

  const uint8_t* alloc_base = alloc->bytes.data();
  if (!Check(static_cast<uint8_t*>(mapped_ptr) == alloc_base + expected_offset, "Map pData points at subresource offset")) {
    return false;
  }
  if (!Check(std::memcmp(alloc_base + expected_offset, expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTexture2DMipArrayDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/0,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(UpdateSubresourceUP mip+array)")) {
    return false;
  }

  const uint32_t dst_subresource = 4; // mip1 layer1 when mip_levels=3.
  std::vector<uint8_t> sysmem(16);
  for (size_t i = 0; i < sysmem.size(); ++i) {
    sysmem[i] = static_cast<uint8_t>(0x70u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          dst_subresource,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/8,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed mip+array UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed mip+array UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * 4u;
  const uint64_t mip1_size = static_cast<uint64_t>(8u) * 2u;
  const uint64_t mip2_size = 4u;
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == expected_offset, "RESOURCE_DIRTY_RANGE offset matches subresource layout")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sysmem.size(), "RESOURCE_DIRTY_RANGE size matches sysmem upload")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected_offset + sysmem.size(), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data() + expected_offset, sysmem.data(), sysmem.size()) == 0, "backing bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopySubresourceRegionTexture2DMipArrayReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy subresource mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &src),
             "CreateStagingTexture2DWithFormatAndDesc(src mip+array)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_READ,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &dst),
             "CreateStagingTexture2DWithFormatAndDesc(dst mip+array)")) {
    return false;
  }

  const uint32_t src_subresource = 1; // mip1 layer0 when mip_levels=3.
  const uint32_t dst_subresource = 4; // mip1 layer1 when mip_levels=3.

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_src = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      src_subresource,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped_src);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src mip+array")) {
    return false;
  }
  if (!Check(mapped_src.pData != nullptr, "Map src returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_src.RowPitch == 8, "Map src RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped_src.DepthPitch == 16, "Map src DepthPitch == RowPitch*height")) {
    return false;
  }

  std::vector<uint8_t> expected(static_cast<size_t>(mapped_src.DepthPitch));
  for (size_t i = 0; i < expected.size(); ++i) {
    expected[i] = static_cast<uint8_t>(0x30u + i);
  }
  std::memcpy(mapped_src.pData, expected.data(), expected.size());
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, src_subresource);

  hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                 dst.hResource,
                                                 dst_subresource,
                                                 /*dst_x=*/0,
                                                 /*dst_y=*/0,
                                                 /*dst_z=*/0,
                                                 src.hResource,
                                                 src_subresource,
                                                 /*pSrcBox=*/nullptr);
  if (!Check(hr == S_OK, "CopySubresourceRegion(mip+array) returns S_OK")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_dst = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              dst_subresource,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &mapped_dst);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst mip+array")) {
    return false;
  }
  if (!Check(mapped_dst.pData != nullptr, "Map dst returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_dst.RowPitch == 8, "Map dst RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped_dst.DepthPitch == 16, "Map dst DepthPitch == RowPitch*height")) {
    return false;
  }
  if (!Check(std::memcmp(mapped_dst.pData, expected.data(), expected.size()) == 0, "CopySubresourceRegion bytes")) {
    return false;
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, dst_subresource);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check(copy_cmd->dst_mip_level == 1, "COPY_TEXTURE2D dst_mip_level == 1")) {
    return false;
  }
  if (!Check(copy_cmd->dst_array_layer == 1, "COPY_TEXTURE2D dst_array_layer == 1")) {
    return false;
  }
  if (!Check(copy_cmd->src_mip_level == 1, "COPY_TEXTURE2D src_mip_level == 1")) {
    return false;
  }
  if (!Check(copy_cmd->src_array_layer == 0, "COPY_TEXTURE2D src_array_layer == 0")) {
    return false;
  }
  if (!Check(copy_cmd->width == 2 && copy_cmd->height == 2, "COPY_TEXTURE2D width/height match mip1 dims")) {
    return false;
  }
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceTexture2DMipArrayReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &src),
             "CreateStagingTexture2DWithFormatAndDesc(src mip+array)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_READ,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &dst),
             "CreateStagingTexture2DWithFormatAndDesc(dst mip+array)")) {
    return false;
  }

  // Fill each src subresource with a distinct byte pattern (pixel bytes only; padding stays zero).
  for (uint32_t subresource = 0; subresource < 6; ++subresource) {
    const uint32_t mip = subresource % 3;
    const uint32_t mip_w = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t mip_h = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t tight_row_bytes = mip_w * 4u;
    const uint8_t fill = static_cast<uint8_t>(0x10u + subresource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        subresource,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src subresource")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map src returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map src returned RowPitch")) {
      return false;
    }

    auto* bytes = static_cast<uint8_t*>(mapped.pData);
    const uint32_t row_pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < mip_h; ++y) {
      std::memset(bytes + static_cast<size_t>(y) * row_pitch, fill, tight_row_bytes);
      if (row_pitch > tight_row_bytes) {
        std::memset(bytes + static_cast<size_t>(y) * row_pitch + tight_row_bytes, 0, row_pitch - tight_row_bytes);
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, subresource);
  }

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  // Force submission so we can validate the COPY_TEXTURE2D count once.
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CopyResource(mip+array)")) {
    return false;
  }
  const std::vector<uint8_t> submitted_stream = dev.harness.last_stream;

  // Validate readback of each destination subresource.
  for (uint32_t subresource = 0; subresource < 6; ++subresource) {
    const uint32_t mip = subresource % 3;
    const uint32_t mip_w = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t mip_h = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t tight_row_bytes = mip_w * 4u;
    const uint8_t fill = static_cast<uint8_t>(0x10u + subresource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                subresource,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst subresource")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map dst returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map dst returned RowPitch")) {
      return false;
    }

    const auto* bytes = static_cast<const uint8_t*>(mapped.pData);
    const uint32_t row_pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < mip_h; ++y) {
      for (uint32_t x = 0; x < tight_row_bytes; ++x) {
        if (!Check(bytes[static_cast<size_t>(y) * row_pitch + x] == fill, "CopyResource subresource bytes")) {
          return false;
        }
      }
      for (uint32_t x = tight_row_bytes; x < row_pitch; ++x) {
        if (!Check(bytes[static_cast<size_t>(y) * row_pitch + x] == 0, "CopyResource subresource padding bytes")) {
          return false;
        }
      }
    }

    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, subresource);
  }

  if (!Check(ValidateStream(submitted_stream.data(), submitted_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = submitted_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, submitted_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 6, "COPY_TEXTURE2D emitted per subresource")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestBcTexture2DLayout() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(bc texture layout)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(bc tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    const uint32_t expected_row_pitch = div_round_up(kWidth, 4) * c.block_bytes;
    const uint32_t expected_rows = div_round_up(kHeight, 4);
    const uint32_t expected_depth_pitch = expected_row_pitch * expected_rows;

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == expected_row_pitch, msg)) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                tex.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_WRITE,
                                                /*map_flags=*/0,
                                                &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "Map RowPitch matches expected for %s", c.name);
    if (!Check(mapped.RowPitch == expected_row_pitch, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "Map DepthPitch matches expected for %s", c.name);
    if (!Check(mapped.DepthPitch == expected_depth_pitch, msg)) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestMapDoNotWaitRespectsFenceCompletion() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/true),
             "InitTestDevice(map do_not_wait async fences)")) {
    return false;
  }
  dev.callbacks.pfnWaitForFence = nullptr;
  dev.callbacks.pfnQueryCompletedFence = &Harness::QueryCompletedFence;

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_src = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped_src);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped_src.pData != nullptr, "Map src returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_src.RowPitch != 0, "Map src returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t src_pitch = mapped_src.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped_src.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * src_pitch + x] = static_cast<uint8_t>((y + 1) * 0x10u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_dst = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                                              &mapped_dst);
  if (!Check(hr == DXGI_ERROR_WAS_STILL_DRAWING, "Map(READ, DO_NOT_WAIT) returns still drawing")) {
    return false;
  }

  const uint64_t fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(fence != 0, "async submit produced a non-zero fence")) {
    return false;
  }

  dev.harness.completed_fence.store(fence, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &mapped_dst);
  if (!Check(hr == S_OK, "Map(READ) succeeds after fence completion")) {
    return false;
  }
  if (!Check(mapped_dst.pData != nullptr, "Map dst returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_dst.RowPitch == src_pitch, "Map dst RowPitch matches src")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(mapped_dst.pData);
  const uint32_t dst_pitch = mapped_dst.RowPitch;
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 0x10u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * dst_pitch + x] == expected, "Map dst bytes match")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestRasterizerStateWireframeDepthBiasEncodesCmd() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(rasterizer state)")) {
    return false;
  }

  AEROGPU_DDIARG_CREATERASTERIZERSTATE desc{};
  desc.fill_mode = AEROGPU_FILL_WIREFRAME;
  desc.cull_mode = AEROGPU_CULL_BACK;
  desc.front_ccw = 0;
  desc.depth_bias = 1337;
  desc.scissor_enable = 0;
  desc.depth_clip_enable = 0;

  const SIZE_T rs_size = dev.device_funcs.pfnCalcPrivateRasterizerStateSize(dev.hDevice, &desc);
  if (!Check(rs_size >= sizeof(uint32_t), "CalcPrivateRasterizerStateSize returned non-zero size")) {
    return false;
  }

  std::vector<uint8_t> rs_mem(static_cast<size_t>(rs_size), 0);
  D3D10DDI_HRASTERIZERSTATE rs{};
  rs.pDrvPrivate = rs_mem.data();

  HRESULT hr = dev.device_funcs.pfnCreateRasterizerState(dev.hDevice, &desc, rs);
  if (!Check(hr == S_OK, "CreateRasterizerState")) {
    return false;
  }

  dev.device_funcs.pfnSetRasterizerState(dev.hDevice, rs);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetRasterizerState")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());
  CmdLoc loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!Check(loc.hdr != nullptr, "SET_RASTERIZER_STATE emitted")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_rasterizer_state*>(stream + loc.offset);
  if (!Check(cmd->state.fill_mode == AEROGPU_FILL_WIREFRAME, "fill_mode is WIREFRAME")) {
    return false;
  }
  if (!Check(cmd->state.depth_bias == 1337, "depth_bias matches")) {
    return false;
  }
  if (!Check((cmd->state.flags & AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE) != 0,
             "DepthClipEnable=FALSE sets DEPTH_CLIP_DISABLE flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyRasterizerState(dev.hDevice, rs);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestRotateResourceIdentitiesRemapsMrtSlots() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(RotateResourceIdentities MRT)")) {
    return false;
  }

  TestResource a{};
  TestResource b{};
  TestResource c{};
  if (!Check(CreateTexture2D(&dev,
                             /*width=*/4,
                             /*height=*/4,
                             AEROGPU_D3D11_USAGE_DEFAULT,
                             kD3D11BindRenderTarget,
                             /*cpu_access_flags=*/0,
                             kDxgiFormatB8G8R8A8Unorm,
                             &a),
             "Create tex A")) {
    return false;
  }
  if (!Check(CreateTexture2D(&dev,
                             /*width=*/4,
                             /*height=*/4,
                             AEROGPU_D3D11_USAGE_DEFAULT,
                             kD3D11BindRenderTarget,
                             /*cpu_access_flags=*/0,
                             kDxgiFormatB8G8R8A8Unorm,
                             &b),
             "Create tex B")) {
    return false;
  }
  if (!Check(CreateTexture2D(&dev,
                             /*width=*/4,
                             /*height=*/4,
                             AEROGPU_D3D11_USAGE_DEFAULT,
                             kD3D11BindRenderTarget,
                             /*cpu_access_flags=*/0,
                             kDxgiFormatB8G8R8A8Unorm,
                             &c),
             "Create tex C")) {
    return false;
  }

  TestRenderTargetView rtv_a{};
  TestRenderTargetView rtv_b{};
  TestRenderTargetView rtv_c{};
  if (!Check(CreateRenderTargetView(&dev, &a, &rtv_a), "CreateRTV(A)")) {
    return false;
  }
  if (!Check(CreateRenderTargetView(&dev, &b, &rtv_b), "CreateRTV(B)")) {
    return false;
  }
  if (!Check(CreateRenderTargetView(&dev, &c, &rtv_c), "CreateRTV(C)")) {
    return false;
  }

  // Bind MRT: RTV0=A, RTV1=B.
  const D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv_a.hView, rtv_b.hView};
  D3D10DDI_HDEPTHSTENCILVIEW dsv{};
  dsv.pDrvPrivate = nullptr;
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, dsv);

  // Flush so we can capture the CREATE_TEXTURE2D handle identities before rotation.
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetRenderTargets")) {
    return false;
  }
  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream0 = dev.harness.last_stream.data();
  const size_t stream0_len = StreamBytesUsed(stream0, dev.harness.last_stream.size());

  // Collect the CREATE_TEXTURE2D handles in emission order so we don't assume any
  // specific handle allocation strategy.
  std::vector<aerogpu_handle_t> handles;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream0_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(stream0 + offset);
    if (hdr->opcode == AEROGPU_CMD_CREATE_TEXTURE2D) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream0 + offset);
      handles.push_back(cmd->texture_handle);
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream0_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  if (!Check(handles.size() >= 3, "captured >=3 CREATE_TEXTURE2D handles")) {
    return false;
  }
  const aerogpu_handle_t handle_a = handles[handles.size() - 3];
  const aerogpu_handle_t handle_b = handles[handles.size() - 2];
  const aerogpu_handle_t handle_c = handles[handles.size() - 1];
  (void)handle_a;

  D3D10DDI_HRESOURCE rotation[3] = {a.hResource, b.hResource, c.hResource};
  dev.device_funcs.pfnRotateResourceIdentities(dev.hDevice, rotation, 3);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after RotateResourceIdentities")) {
    return false;
  }
  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_SET_RENDER_TARGETS) == 1,
             "RotateResourceIdentities emitted SET_RENDER_TARGETS")) {
    return false;
  }

  CmdLoc loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS emitted")) {
    return false;
  }
  const auto* set_cmd = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(stream + loc.offset);
  if (!Check(set_cmd->color_count == 2, "color_count preserved")) {
    return false;
  }
  if (!Check(set_cmd->colors[0] == handle_b, "RTV0 remapped to B")) {
    return false;
  }
  if (!Check(set_cmd->colors[1] == handle_c, "RTV1 remapped to C")) {
    return false;
  }

  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv_c.hView);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv_b.hView);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv_a.hView);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, c.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, b.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, a.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestBlendStateValidationRtCountOneIgnoresRt1Mismatch() {
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
  rts[1].blend_enable = false; // mismatch, but should be ignored when rt_count==1.

  aerogpu::d3d10_11::AerogpuBlendStateBase out{};
  const HRESULT hr = aerogpu::d3d10_11::ValidateAndConvertBlendDesc(rts,
                                                                    /*rt_count=*/1,
                                                                    /*alpha_to_coverage_enable=*/false,
                                                                    &out);
  if (!Check(hr == S_OK, "ValidateAndConvertBlendDesc(rt_count=1) ignores RT1 mismatch")) {
    return false;
  }
  if (!Check(out.enable == 1u, "blend enable propagated from RT0")) {
    return false;
  }
  if (!Check(out.src_factor == AEROGPU_BLEND_SRC_ALPHA, "src factor mapped")) {
    return false;
  }
  if (!Check(out.dst_factor == AEROGPU_BLEND_INV_SRC_ALPHA, "dst factor mapped")) {
    return false;
  }
  return true;
}

bool TestSetBlendStateEncodesCmd() {
  TestDevice dev{};
  if (!InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false)) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.enable = 1;
  desc.src_factor = AEROGPU_BLEND_SRC_ALPHA;
  desc.dst_factor = AEROGPU_BLEND_INV_SRC_ALPHA;
  desc.blend_op = AEROGPU_BLEND_OP_ADD;
  desc.color_write_mask = 0xFu;
  desc.src_factor_alpha = AEROGPU_BLEND_ONE;
  desc.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  desc.blend_op_alpha = AEROGPU_BLEND_OP_ADD;

  const SIZE_T size = dev.device_funcs.pfnCalcPrivateBlendStateSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateBlendStateSize returned a non-trivial size")) {
    return false;
  }

  std::vector<uint8_t> storage(static_cast<size_t>(size), 0);
  D3D10DDI_HBLENDSTATE hState{};
  hState.pDrvPrivate = storage.data();

  HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == S_OK, "CreateBlendState(supported)")) {
    return false;
  }

  dev.device_funcs.pfnSetBlendState(dev.hDevice, hState, /*blend_factor=*/nullptr, /*sample_mask=*/0xFFFFFFFFu);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());
  CmdLoc loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(stream + loc.offset);
  if (!Check(cmd->state.enable == 1u, "blend enable propagated")) {
    return false;
  }
  if (!Check(cmd->state.src_factor == AEROGPU_BLEND_SRC_ALPHA, "src_factor mapped")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor == AEROGPU_BLEND_INV_SRC_ALPHA, "dst_factor mapped")) {
    return false;
  }
  if (!Check(cmd->state.blend_op == AEROGPU_BLEND_OP_ADD, "blend_op mapped")) {
    return false;
  }
  if (!Check(cmd->state.color_write_mask == 0xFu, "color_write_mask propagated")) {
    return false;
  }
  if (!Check(cmd->state.src_factor_alpha == AEROGPU_BLEND_ONE, "src_factor_alpha mapped")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor_alpha == AEROGPU_BLEND_ZERO, "dst_factor_alpha mapped")) {
    return false;
  }
  if (!Check(cmd->state.blend_op_alpha == AEROGPU_BLEND_OP_ADD, "blend_op_alpha mapped")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == 0x3F800000u, "blend constant defaulted to 1.0")) {
    return false;
  }
  if (!Check(cmd->state.sample_mask == 0xFFFFFFFFu, "sample mask defaulted to all 1s")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetBlendStateEncodesConstantFactor() {
  TestDevice dev{};
  if (!InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false)) {
    return false;
  }

  AEROGPU_DDIARG_CREATEBLENDSTATE desc = {};
  desc.enable = 1;
  desc.src_factor = AEROGPU_BLEND_CONSTANT;
  desc.dst_factor = AEROGPU_BLEND_INV_CONSTANT;
  desc.blend_op = AEROGPU_BLEND_OP_ADD;
  desc.color_write_mask = 0xFu;
  // Keep alpha in a supported config (doesn't matter much for this test).
  desc.src_factor_alpha = AEROGPU_BLEND_ONE;
  desc.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  desc.blend_op_alpha = AEROGPU_BLEND_OP_ADD;

  const SIZE_T size = dev.device_funcs.pfnCalcPrivateBlendStateSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateBlendStateSize returned a non-trivial size")) {
    return false;
  }

  std::vector<uint8_t> storage(static_cast<size_t>(size), 0);
  D3D10DDI_HBLENDSTATE hState{};
  hState.pDrvPrivate = storage.data();

  HRESULT hr = dev.device_funcs.pfnCreateBlendState(dev.hDevice, &desc, hState);
  if (!Check(hr == S_OK, "CreateBlendState(constant factor)")) {
    return false;
  }

  const float blend_factor[4] = {0.25f, 0.5f, 0.75f, 1.0f};
  const UINT sample_mask = 0x01234567u;
  dev.device_funcs.pfnSetBlendState(dev.hDevice, hState, blend_factor, sample_mask);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetBlendState(constant factor)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());
  CmdLoc loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_BLEND_STATE);
  if (!Check(loc.hdr != nullptr, "SET_BLEND_STATE emitted")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_blend_state*>(stream + loc.offset);
  if (!Check(cmd->state.enable == 1u, "blend enable propagated")) {
    return false;
  }
  if (!Check(cmd->state.src_factor == AEROGPU_BLEND_CONSTANT, "src_factor mapped to CONSTANT")) {
    return false;
  }
  if (!Check(cmd->state.dst_factor == AEROGPU_BLEND_INV_CONSTANT, "dst_factor mapped to INV_CONSTANT")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[0] == 0x3E800000u, "blend constant[0] encoded (0.25)")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[1] == 0x3F000000u, "blend constant[1] encoded (0.5)")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[2] == 0x3F400000u, "blend constant[2] encoded (0.75)")) {
    return false;
  }
  if (!Check(cmd->state.blend_constant_rgba_f32[3] == 0x3F800000u, "blend constant[3] encoded (1.0)")) {
    return false;
  }
  if (!Check(cmd->state.sample_mask == sample_mask, "sample mask propagated")) {
    return false;
  }

  dev.device_funcs.pfnDestroyBlendState(dev.hDevice, hState);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDrawInstancedEncodesInstanceFields() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(draw instanced)")) {
    return false;
  }

  dev.harness.last_stream.clear();
  dev.device_funcs.pfnDrawInstanced(dev.hDevice,
                                    /*vertex_count_per_instance=*/6,
                                    /*instance_count=*/4,
                                    /*start_vertex=*/2,
                                    /*start_instance=*/7);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DrawInstanced")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc draw_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_DRAW);
  if (!Check(draw_loc.hdr != nullptr, "DRAW emitted for DrawInstanced")) {
    return false;
  }
  const auto* draw = reinterpret_cast<const aerogpu_cmd_draw*>(stream + draw_loc.offset);
  if (!Check(draw->vertex_count == 6, "DrawInstanced vertex_count encoded")) {
    return false;
  }
  if (!Check(draw->instance_count == 4, "DrawInstanced instance_count encoded")) {
    return false;
  }
  if (!Check(draw->first_vertex == 2, "DrawInstanced first_vertex encoded")) {
    return false;
  }
  if (!Check(draw->first_instance == 7, "DrawInstanced first_instance encoded")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDrawIndexedInstancedEncodesInstanceFields() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(draw indexed instanced)")) {
    return false;
  }

  dev.harness.last_stream.clear();
  dev.device_funcs.pfnDrawIndexedInstanced(dev.hDevice,
                                           /*index_count_per_instance=*/12,
                                           /*instance_count=*/3,
                                           /*start_index=*/5,
                                           /*base_vertex=*/-2,
                                           /*start_instance=*/9);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DrawIndexedInstanced")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc draw_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_DRAW_INDEXED);
  if (!Check(draw_loc.hdr != nullptr, "DRAW_INDEXED emitted for DrawIndexedInstanced")) {
    return false;
  }
  const auto* draw = reinterpret_cast<const aerogpu_cmd_draw_indexed*>(stream + draw_loc.offset);
  if (!Check(draw->index_count == 12, "DrawIndexedInstanced index_count encoded")) {
    return false;
  }
  if (!Check(draw->instance_count == 3, "DrawIndexedInstanced instance_count encoded")) {
    return false;
  }
  if (!Check(draw->first_index == 5, "DrawIndexedInstanced first_index encoded")) {
    return false;
  }
  if (!Check(draw->base_vertex == -2, "DrawIndexedInstanced base_vertex encoded")) {
    return false;
  }
  if (!Check(draw->first_instance == 9, "DrawIndexedInstanced first_instance encoded")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDrawAutoEncodesNoopDraw() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(draw auto)")) {
    return false;
  }

  dev.harness.last_stream.clear();
  dev.device_funcs.pfnDrawAuto(dev.hDevice);
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DrawAuto")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc draw_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_DRAW);
  if (!Check(draw_loc.hdr != nullptr, "DRAW emitted for DrawAuto")) {
    return false;
  }
  const auto* draw = reinterpret_cast<const aerogpu_cmd_draw*>(stream + draw_loc.offset);
  if (!Check(draw->vertex_count == 0, "DrawAuto vertex_count encoded as 0")) {
    return false;
  }
  if (!Check(draw->instance_count == 1, "DrawAuto instance_count encoded as 1")) {
    return false;
  }
  if (!Check(draw->first_vertex == 0, "DrawAuto first_vertex encoded as 0")) {
    return false;
  }
  if (!Check(draw->first_instance == 0, "DrawAuto first_instance encoded as 0")) {
    return false;
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestInternalDxgiFormatCompatHelpers();
  ok &= TestViewportHelperCachesDimsOnlyWhenEnabledForD3D10StyleDevice();
  ok &= TestViewportScissorHelpersDontReportNotImplWhenCmdAppendFails();
  ok &= TestRenderTargetHelpersClearStaleDsvHandles();
  ok &= TestPrimitiveTopologyHelperEmitsAndCaches();
  ok &= TestSetTextureHelperEncodesPacket();
  ok &= TestSetSamplersHelperEncodesPacket();
  ok &= TestTrackWddmAllocForSubmitLockedHelper();
  ok &= TestDeviceFuncsTableNoNullEntriesHostOwned();
  ok &= TestDeviceFuncsTableNoNullEntriesGuestBacked();
  ok &= TestHostOwnedBufferUnmapUploads();
  ok &= TestHostOwnedTextureUnmapUploads();
  ok &= TestCreateTexture2dSrgbFormatEncodesSrgbAerogpuFormat();
  ok &= TestCreateTexture2DMipLevelsZeroAllocatesFullChain();
  ok &= TestB5Texture2DCreateMapUnmapEncodesAerogpuFormat();
  ok &= TestGuestBackedBufferUnmapDirtyRange();
  ok &= TestGuestBackedTextureUnmapDirtyRange();
  ok &= TestGuestBackedBcTextureUnmapDirtyRange();
  ok &= TestMapUsageValidation();
  ok &= TestMapCpuAccessValidation();
  ok &= TestMapFlagsValidation();
  ok &= TestStagingMapFlagsValidation();
  ok &= TestMapAlreadyMappedFails();
  ok &= TestMapSubresourceValidation();
  ok &= TestStagingMapTypeValidation();
  ok &= TestStagingReadWriteMapAllowed();
  ok &= TestMapDoNotWaitReportsStillDrawing();
  ok &= TestMapDoNotWaitIgnoresUnrelatedInFlightWork();
  ok &= TestMapBlockingWaitUsesInfiniteTimeout();
  ok &= TestInvalidUnmapReportsError();
  ok &= TestInvalidSpecializedUnmapReportsError();
  ok &= TestDynamicMapFlagsValidation();
  ok &= TestDynamicMapTypeValidation();
  ok &= TestMapDefaultImmutableRejected();
  ok &= TestHostOwnedDynamicIABufferUploads();
  ok &= TestGuestBackedDynamicIABufferDirtyRange();
  ok &= TestDynamicBufferUsageValidation();
  ok &= TestHostOwnedDynamicConstantBufferUploads();
  ok &= TestGuestBackedDynamicConstantBufferDirtyRange();
  ok &= TestSubmitAllocListTracksBoundConstantBuffer();
  ok &= TestSubmitAllocListTracksBoundShaderResource();
  ok &= TestSubmitAllocWriteFlagsForDraw();
  ok &= TestHostOwnedCopyResourceBufferReadback();
  ok &= TestHostOwnedCopyResourceBufferReadbackPadsSize();
  ok &= TestHostOwnedCopyResourceTextureReadback();
  ok &= TestHostOwnedCopyResourceBcTextureReadback();
  ok &= TestHostOwnedCopySubresourceRegionBcTextureReadback();
  ok &= TestGuestBackedCopyResourceBufferReadback();
  ok &= TestGuestBackedCopyResourceTextureReadback();
  ok &= TestClearRtvB5FormatsProduceCorrectReadback();
  ok &= TestGuestBackedCopyResourceBcTextureReadback();
  ok &= TestGuestBackedCopySubresourceRegionBcTextureReadback();
  ok &= TestHostOwnedUpdateSubresourceUPBufferUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBufferDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPTextureUploads();
  ok &= TestHostOwnedUpdateSubresourceUPTexture2DMipArrayUploads();
  ok &= TestGuestBackedUpdateSubresourceUPTextureDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBufferBoxUploads();
  ok &= TestHostOwnedUpdateSubresourceUPBufferBoxUnalignedPadsTo4();
  ok &= TestHostOwnedUpdateSubresourceUPTextureBoxUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBufferBoxDirtyRange();
  ok &= TestGuestBackedUpdateSubresourceUPTextureBoxDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureBoxUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureBoxDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureBoxRejectsMisaligned();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureBoxRejectsMisaligned();
  ok &= TestHostOwnedCreateBufferInitialDataUploads();
  ok &= TestHostOwnedCreateBufferInitialDataPadsTo4();
  ok &= TestGuestBackedCreateBufferInitialDataDirtyRange();
  ok &= TestCreateBufferSrvUavBindsMarkStorageUsage();
  ok &= TestHostOwnedCreateTextureInitialDataUploads();
  ok &= TestGuestBackedCreateTextureInitialDataDirtyRange();
  ok &= TestHostOwnedCreateBcTextureInitialDataUploads();
  ok &= TestGuestBackedCreateBcTextureInitialDataDirtyRange();
  ok &= TestSrgbTexture2DFormatPropagation();
  ok &= TestSrgbTexture2DFormatPropagationGuestBacked();
  ok &= TestGuestBackedTexture2DMipArrayCreateEncodesMipAndArray();
  ok &= TestGuestBackedCreateTexture2DMipArrayInitialDataDirtyRange();
  ok &= TestHostOwnedCreateTexture2DMipArrayInitialDataUploads();
  ok &= TestHostOwnedDynamicTexture2DMipArrayMapUnmapUploads();
  ok &= TestGuestBackedTexture2DMipArrayMapUnmapDirtyRange();
  ok &= TestGuestBackedUpdateSubresourceUPTexture2DMipArrayDirtyRange();
  ok &= TestGuestBackedCopySubresourceRegionTexture2DMipArrayReadback();
  ok &= TestGuestBackedCopyResourceTexture2DMipArrayReadback();
  ok &= TestBcTexture2DLayout();
  ok &= TestMapDoNotWaitRespectsFenceCompletion();
  ok &= TestRasterizerStateWireframeDepthBiasEncodesCmd();
  ok &= TestRotateResourceIdentitiesRemapsMrtSlots();
  ok &= TestBlendStateValidationRtCountOneIgnoresRt1Mismatch();
  ok &= TestSetBlendStateEncodesCmd();
  ok &= TestSetBlendStateEncodesConstantFactor();
  ok &= TestDrawInstancedEncodesInstanceFields();
  ok &= TestDrawIndexedInstancedEncodesInstanceFields();
  ok &= TestDrawAutoEncodesNoopDraw();

  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_map_unmap_tests\n");
  return 0;
}
