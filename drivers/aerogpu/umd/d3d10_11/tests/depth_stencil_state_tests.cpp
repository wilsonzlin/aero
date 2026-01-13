#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "aerogpu_d3d10_11_umd.h"

#include "aerogpu_cmd.h"
#include "aerogpu_d3d10_11_internal.h"

namespace {

using namespace aerogpu::d3d10_11;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool TestStencilMasksPropagateIntoCmdPacket() {
  Device dev{};
  dev.cmd.reset();

  DepthStencilState dss{};
  dss.depth_enable = 1u;
  dss.depth_write_mask = 1u; // D3D11_DEPTH_WRITE_MASK_ALL
  dss.depth_func = 2u; // D3D11_COMPARISON_LESS
  dss.stencil_enable = 1u;
  dss.stencil_read_mask = 0x0Fu;
  dss.stencil_write_mask = 0xF0u;

  if (!Check(EmitDepthStencilStateCmdLocked(&dev, &dss), "EmitDepthStencilStateCmdLocked")) {
    return false;
  }
  dev.cmd.finalize();

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();

  if (!Check(stream_len >= sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_set_depth_stencil_state),
             "stream contains header + depth-stencil packet")) {
    return false;
  }

  const auto* hdr = reinterpret_cast<const aerogpu_cmd_stream_header*>(stream);
  if (!Check(hdr->magic == AEROGPU_CMD_STREAM_MAGIC, "stream header magic")) {
    return false;
  }
  if (!Check(hdr->abi_version == AEROGPU_ABI_VERSION_U32, "stream header abi_version")) {
    return false;
  }
  if (!Check(static_cast<size_t>(hdr->size_bytes) == stream_len, "stream header size_bytes matches buffer")) {
    return false;
  }

  const size_t pkt_off = sizeof(aerogpu_cmd_stream_header);
  const auto* pkt = reinterpret_cast<const aerogpu_cmd_set_depth_stencil_state*>(stream + pkt_off);
  if (!Check(pkt->hdr.opcode == AEROGPU_CMD_SET_DEPTH_STENCIL_STATE, "packet opcode")) {
    return false;
  }
  if (!Check(pkt->hdr.size_bytes == sizeof(aerogpu_cmd_set_depth_stencil_state), "packet size_bytes")) {
    return false;
  }
  if (!Check(pkt->state.stencil_read_mask == 0x0F, "stencil_read_mask propagated")) {
    return false;
  }
  if (!Check(pkt->state.stencil_write_mask == 0xF0, "stencil_write_mask propagated")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestStencilMasksPropagateIntoCmdPacket();

  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_depth_stencil_state_tests\n");
  return 0;
}

