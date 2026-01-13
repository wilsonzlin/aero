#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "aerogpu_d3d10_11_umd.h"
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

const aerogpu_cmd_set_render_targets* FindLastSetRenderTargets(const uint8_t* buf, size_t len) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return nullptr;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  size_t stream_len = len;
  if (stream->size_bytes >= sizeof(aerogpu_cmd_stream_header) && stream->size_bytes <= len) {
    stream_len = static_cast<size_t>(stream->size_bytes);
  }

  const aerogpu_cmd_set_render_targets* last = nullptr;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || (hdr->size_bytes & 3u) != 0 ||
        hdr->size_bytes > stream_len - offset) {
      break;
    }
    if (hdr->opcode == AEROGPU_CMD_SET_RENDER_TARGETS) {
      if (hdr->size_bytes >= sizeof(aerogpu_cmd_set_render_targets)) {
        last = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(buf + offset);
      }
    }
    offset += hdr->size_bytes;
  }
  return last;
}

bool TestBindTwoRtvsEmitsTwoColorHandles() {
  Device dev{};

  Resource res0{};
  res0.handle = 1001;
  Resource res1{};
  res1.handle = 1002;

  RenderTargetView rtv0{};
  rtv0.texture = res0.handle;
  rtv0.resource = &res0;
  RenderTargetView rtv1{};
  rtv1.texture = res1.handle;
  rtv1.resource = &res1;

  const RenderTargetView* rtvs[2] = {&rtv0, &rtv1};
  SetRenderTargetsStateLocked(&dev, /*num_rtvs=*/2, rtvs, /*dsv=*/nullptr);
  if (!Check(EmitSetRenderTargetsCmdFromStateLocked(&dev), "EmitSetRenderTargetsCmdFromStateLocked")) {
    return false;
  }
  dev.cmd.finalize();

  const uint8_t* bytes = dev.cmd.data();
  const size_t len = dev.cmd.size();
  const auto* cmd = FindLastSetRenderTargets(bytes, len);
  if (!Check(cmd != nullptr, "SET_RENDER_TARGETS packet must exist")) {
    return false;
  }

  if (!Check(cmd->color_count == 2, "SET_RENDER_TARGETS color_count==2")) {
    return false;
  }
  if (!Check(cmd->colors[0] == res0.handle, "SET_RENDER_TARGETS colors[0]")) {
    return false;
  }
  if (!Check(cmd->colors[1] == res1.handle, "SET_RENDER_TARGETS colors[1]")) {
    return false;
  }
  return true;
}

bool TestGapNormalizationDropsLaterRtvs() {
  Device dev{};

  Resource res0{};
  res0.handle = 2001;
  Resource res1{};
  res1.handle = 2002;

  RenderTargetView rtv0{};
  rtv0.texture = res0.handle;
  rtv0.resource = &res0;
  RenderTargetView rtv1{};
  rtv1.texture = res1.handle;
  rtv1.resource = &res1;

  const RenderTargetView* rtvs[2] = {&rtv0, &rtv1};
  SetRenderTargetsStateLocked(&dev, /*num_rtvs=*/2, rtvs, /*dsv=*/nullptr);

  // Simulate SRV aliasing unbinding slot 0 while slot 1 is still bound, which
  // would produce an unsupported SET_RENDER_TARGETS gap without normalization.
  dev.current_rtvs[0] = 0;
  dev.current_rtv_resources[0] = nullptr;
  NormalizeRenderTargetsNoGapsLocked(&dev);

  if (!Check(EmitSetRenderTargetsCmdFromStateLocked(&dev), "EmitSetRenderTargetsCmdFromStateLocked(gap)")) {
    return false;
  }
  dev.cmd.finalize();

  const uint8_t* bytes = dev.cmd.data();
  const size_t len = dev.cmd.size();
  const auto* cmd = FindLastSetRenderTargets(bytes, len);
  if (!Check(cmd != nullptr, "SET_RENDER_TARGETS packet must exist (gap)")) {
    return false;
  }

  if (!Check(cmd->color_count == 0, "gap normalization should drop all RTVs (color_count==0)")) {
    return false;
  }
  if (!Check(cmd->colors[0] == 0 && cmd->colors[1] == 0, "gap normalization clears colors[]")) {
    return false;
  }
  return true;
}

} // namespace

int main() {
  if (!TestBindTwoRtvsEmitsTwoColorHandles()) {
    return 1;
  }
  if (!TestGapNormalizationDropsLaterRtvs()) {
    return 1;
  }
  return 0;
}
