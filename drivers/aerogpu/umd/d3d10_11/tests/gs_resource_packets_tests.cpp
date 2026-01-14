#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <iterator>

#include "aerogpu_cmd.h"
#include "aerogpu_cmd_writer.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

size_t AlignUp(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

bool TestGeometryStageResourceBindingPackets() {
  aerogpu::CmdWriter w;
  w.set_vector();

  // SET_TEXTURE (GS)
  constexpr aerogpu_handle_t kTex = 0xAABBCCDDu;
  auto* set_tex = w.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  if (!Check(set_tex != nullptr, "append SET_TEXTURE")) {
    return false;
  }
  // Prefer the direct GEOMETRY stage encoding for GS bindings:
  // - shader_stage = GEOMETRY
  // - reserved0 = 0
  //
  // (The `stage_ex` encoding exists for compatibility and for non-legacy stages like HS/DS.)
  set_tex->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
  set_tex->slot = 3;
  set_tex->texture = kTex;
  set_tex->reserved0 = 0;

  // SET_SAMPLERS (GS)
  constexpr aerogpu_handle_t kSamplers[] = {0x1111u, 0x2222u, 0x3333u};
  auto* set_samplers = w.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, kSamplers, sizeof(kSamplers));
  if (!Check(set_samplers != nullptr, "append SET_SAMPLERS")) {
    return false;
  }
  set_samplers->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
  set_samplers->start_slot = 1;
  set_samplers->sampler_count = static_cast<uint32_t>(std::size(kSamplers));
  set_samplers->reserved0 = 0;

  // SET_CONSTANT_BUFFERS (GS)
  constexpr aerogpu_constant_buffer_binding kCbs[] = {
      {0x44556677u, 16u, 64u, 0u},
  };
  auto* set_cbs = w.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, kCbs, sizeof(kCbs));
  if (!Check(set_cbs != nullptr, "append SET_CONSTANT_BUFFERS")) {
    return false;
  }
  set_cbs->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
  set_cbs->start_slot = 2;
  set_cbs->buffer_count = static_cast<uint32_t>(std::size(kCbs));
  set_cbs->reserved0 = 0;

  // SET_SHADER_RESOURCE_BUFFERS (GS)
  constexpr aerogpu_shader_resource_buffer_binding kSrvBufs[] = {
      {0xCAFEBABEu, 0u, 128u, 0u},
  };
  auto* set_srv_bufs = w.append_with_payload<aerogpu_cmd_set_shader_resource_buffers>(
      AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS, kSrvBufs, sizeof(kSrvBufs));
  if (!Check(set_srv_bufs != nullptr, "append SET_SHADER_RESOURCE_BUFFERS")) {
    return false;
  }
  set_srv_bufs->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
  set_srv_bufs->start_slot = 4;
  set_srv_bufs->buffer_count = static_cast<uint32_t>(std::size(kSrvBufs));
  set_srv_bufs->reserved0 = 0;

  w.finalize();
  if (!Check(w.error() == aerogpu::CmdStreamError::kOk, "writer error == kOk")) {
    return false;
  }

  const uint8_t* buf = w.data();
  const size_t len = w.bytes_used();
  if (!Check(len >= sizeof(aerogpu_cmd_stream_header), "stream contains header")) {
    return false;
  }
 
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "stream magic")) {
    return false;
  }
  if (!Check(stream->size_bytes == len, "stream size_bytes matches writer bytes_used")) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);

  // SET_TEXTURE
  {
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "SET_TEXTURE header in-bounds")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->opcode == AEROGPU_CMD_SET_TEXTURE, "SET_TEXTURE opcode")) {
      return false;
    }
    if (!Check(hdr->size_bytes == sizeof(aerogpu_cmd_set_texture), "SET_TEXTURE size_bytes")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (!Check(cmd->shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY, "SET_TEXTURE shader_stage==GEOMETRY")) {
      return false;
    }
    if (!Check(cmd->slot == 3, "SET_TEXTURE slot==3")) {
      return false;
    }
    if (!Check(cmd->texture == kTex, "SET_TEXTURE texture")) {
      return false;
    }
    if (!Check(cmd->reserved0 == 0, "SET_TEXTURE reserved0==0")) {
      return false;
    }
    offset += hdr->size_bytes;
  }

  // SET_SAMPLERS
  {
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "SET_SAMPLERS header in-bounds")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->opcode == AEROGPU_CMD_SET_SAMPLERS, "SET_SAMPLERS opcode")) {
      return false;
    }
    const size_t expected_size = AlignUp(sizeof(aerogpu_cmd_set_samplers) + sizeof(kSamplers), 4);
    if (!Check(hdr->size_bytes == expected_size, "SET_SAMPLERS size_bytes")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_samplers*>(hdr);
    if (!Check(cmd->shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY, "SET_SAMPLERS shader_stage==GEOMETRY")) {
      return false;
    }
    if (!Check(cmd->start_slot == 1, "SET_SAMPLERS start_slot==1")) {
      return false;
    }
    if (!Check(cmd->sampler_count == std::size(kSamplers), "SET_SAMPLERS sampler_count")) {
      return false;
    }
    if (!Check(cmd->reserved0 == 0, "SET_SAMPLERS reserved0==0")) {
      return false;
    }
    const size_t payload_off = offset + sizeof(aerogpu_cmd_set_samplers);
    if (!Check(payload_off + sizeof(kSamplers) <= len, "SET_SAMPLERS payload in-bounds")) {
      return false;
    }
    if (!Check(std::memcmp(buf + payload_off, kSamplers, sizeof(kSamplers)) == 0, "SET_SAMPLERS payload handles")) {
      return false;
    }
    offset += hdr->size_bytes;
  }

  // SET_CONSTANT_BUFFERS
  {
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "SET_CONSTANT_BUFFERS header in-bounds")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->opcode == AEROGPU_CMD_SET_CONSTANT_BUFFERS, "SET_CONSTANT_BUFFERS opcode")) {
      return false;
    }
    const size_t expected_size = AlignUp(sizeof(aerogpu_cmd_set_constant_buffers) + sizeof(kCbs), 4);
    if (!Check(hdr->size_bytes == expected_size, "SET_CONSTANT_BUFFERS size_bytes")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_constant_buffers*>(hdr);
    if (!Check(cmd->shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY,
               "SET_CONSTANT_BUFFERS shader_stage==GEOMETRY")) {
      return false;
    }
    if (!Check(cmd->start_slot == 2, "SET_CONSTANT_BUFFERS start_slot==2")) {
      return false;
    }
    if (!Check(cmd->buffer_count == std::size(kCbs), "SET_CONSTANT_BUFFERS buffer_count")) {
      return false;
    }
    if (!Check(cmd->reserved0 == 0, "SET_CONSTANT_BUFFERS reserved0==0")) {
      return false;
    }
    const size_t payload_off = offset + sizeof(aerogpu_cmd_set_constant_buffers);
    if (!Check(payload_off + sizeof(kCbs) <= len, "SET_CONSTANT_BUFFERS payload in-bounds")) {
      return false;
    }
    if (!Check(std::memcmp(buf + payload_off, kCbs, sizeof(kCbs)) == 0, "SET_CONSTANT_BUFFERS payload bindings")) {
      return false;
    }
    offset += hdr->size_bytes;
  }

  // SET_SHADER_RESOURCE_BUFFERS
  {
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "SET_SHADER_RESOURCE_BUFFERS header in-bounds")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->opcode == AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS, "SET_SHADER_RESOURCE_BUFFERS opcode")) {
      return false;
    }
    const size_t expected_size =
        AlignUp(sizeof(aerogpu_cmd_set_shader_resource_buffers) + sizeof(kSrvBufs), 4);
    if (!Check(hdr->size_bytes == expected_size, "SET_SHADER_RESOURCE_BUFFERS size_bytes")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_shader_resource_buffers*>(hdr);
    if (!Check(cmd->shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY,
               "SET_SHADER_RESOURCE_BUFFERS shader_stage==GEOMETRY")) {
      return false;
    }
    if (!Check(cmd->start_slot == 4, "SET_SHADER_RESOURCE_BUFFERS start_slot==4")) {
      return false;
    }
    if (!Check(cmd->buffer_count == std::size(kSrvBufs), "SET_SHADER_RESOURCE_BUFFERS buffer_count")) {
      return false;
    }
    if (!Check(cmd->reserved0 == 0, "SET_SHADER_RESOURCE_BUFFERS reserved0==0")) {
      return false;
    }
    const size_t payload_off = offset + sizeof(aerogpu_cmd_set_shader_resource_buffers);
    if (!Check(payload_off + sizeof(kSrvBufs) <= len, "SET_SHADER_RESOURCE_BUFFERS payload in-bounds")) {
      return false;
    }
    if (!Check(std::memcmp(buf + payload_off, kSrvBufs, sizeof(kSrvBufs)) == 0, "SET_SHADER_RESOURCE_BUFFERS payload bindings")) {
      return false;
    }
    offset += hdr->size_bytes;
  }

  return Check(offset == len, "stream ends after SET_SHADER_RESOURCE_BUFFERS");
}

}  // namespace

int main() {
  bool ok = true;
  ok &= TestGeometryStageResourceBindingPackets();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_gs_resource_packets_tests\n");
  return 0;
}
