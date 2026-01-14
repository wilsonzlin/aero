#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

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

bool TestGeometryShaderCreateAndBindPackets() {
  aerogpu::CmdWriter w;
  w.set_vector();

  constexpr aerogpu_handle_t kGsHandle = 0xCAFE1234u;
  constexpr uint8_t kDxbc[] = {
      0x44, 0x58, 0x42, 0x43,  // "DXBC"
      0x01, 0x02, 0x03,        // payload bytes (intentionally not 4-byte aligned)
  };

  auto* create = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, kDxbc, sizeof(kDxbc));
  if (!Check(create != nullptr, "append CREATE_SHADER_DXBC")) {
    return false;
  }
  create->shader_handle = kGsHandle;
  // Prefer the direct GEOMETRY stage encoding for GS shaders:
  // - stage = GEOMETRY
  // - reserved0 = 0
  //
  // (The `stage_ex` encoding exists for compatibility and for non-legacy stages like HS/DS.)
  create->stage = AEROGPU_SHADER_STAGE_GEOMETRY;
  create->dxbc_size_bytes = static_cast<uint32_t>(sizeof(kDxbc));
  create->reserved0 = 0;
  
  auto* bind = w.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  if (!Check(bind != nullptr, "append BIND_SHADERS")) {
    return false;
  }
  bind->vs = 0;
  bind->ps = 0;
  bind->cs = 0;
  bind->reserved0 = kGsHandle;

  auto* destroy = w.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
  if (!Check(destroy != nullptr, "append DESTROY_SHADER")) {
    return false;
  }
  destroy->shader_handle = kGsHandle;
  destroy->reserved0 = 0;

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

  // CREATE_SHADER_DXBC
  if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "CREATE_SHADER_DXBC header in-bounds")) {
    return false;
  }
  const auto* create_hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
  if (!Check(create_hdr->opcode == AEROGPU_CMD_CREATE_SHADER_DXBC, "CREATE_SHADER_DXBC opcode")) {
    return false;
  }
  const size_t expected_create_size =
      AlignUp(sizeof(aerogpu_cmd_create_shader_dxbc) + sizeof(kDxbc), 4);
  if (!Check(create_hdr->size_bytes == expected_create_size, "CREATE_SHADER_DXBC size_bytes")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(create_hdr);
  if (!Check(create_cmd->stage == AEROGPU_SHADER_STAGE_GEOMETRY,
             "CREATE_SHADER_DXBC stage==GEOMETRY")) {
    return false;
  }
  if (!Check(create_cmd->reserved0 == 0, "CREATE_SHADER_DXBC reserved0==0")) {
    return false;
  }
  if (!Check(create_cmd->shader_handle == kGsHandle, "CREATE_SHADER_DXBC shader_handle")) {
    return false;
  }
  if (!Check(create_cmd->dxbc_size_bytes == sizeof(kDxbc), "CREATE_SHADER_DXBC dxbc_size_bytes")) {
    return false;
  }
  if (!Check(std::memcmp(buf + offset + sizeof(aerogpu_cmd_create_shader_dxbc), kDxbc, sizeof(kDxbc)) == 0,
             "CREATE_SHADER_DXBC payload bytes")) {
    return false;
  }
  offset += create_hdr->size_bytes;

  // BIND_SHADERS
  if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "BIND_SHADERS header in-bounds")) {
    return false;
  }
  const auto* bind_hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
  if (!Check(bind_hdr->opcode == AEROGPU_CMD_BIND_SHADERS, "BIND_SHADERS opcode")) {
    return false;
  }
  if (!Check(bind_hdr->size_bytes == sizeof(aerogpu_cmd_bind_shaders), "BIND_SHADERS size_bytes")) {
    return false;
  }
  const auto* bind_cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(bind_hdr);
  if (!Check(bind_cmd->reserved0 == kGsHandle, "BIND_SHADERS reserved0==GS handle")) {
    return false;
  }
  offset += bind_hdr->size_bytes;

  // DESTROY_SHADER
  if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= len, "DESTROY_SHADER header in-bounds")) {
    return false;
  }
  const auto* destroy_hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
  if (!Check(destroy_hdr->opcode == AEROGPU_CMD_DESTROY_SHADER, "DESTROY_SHADER opcode")) {
    return false;
  }
  if (!Check(destroy_hdr->size_bytes == sizeof(aerogpu_cmd_destroy_shader), "DESTROY_SHADER size_bytes")) {
    return false;
  }
  const auto* destroy_cmd = reinterpret_cast<const aerogpu_cmd_destroy_shader*>(destroy_hdr);
  if (!Check(destroy_cmd->shader_handle == kGsHandle, "DESTROY_SHADER shader_handle")) {
    return false;
  }
  offset += destroy_hdr->size_bytes;

  return Check(offset == len, "stream ends after DESTROY_SHADER");
}

}  // namespace

int main() {
  bool ok = true;
  ok &= TestGeometryShaderCreateAndBindPackets();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_gs_shader_packets_tests\n");
  return 0;
}
