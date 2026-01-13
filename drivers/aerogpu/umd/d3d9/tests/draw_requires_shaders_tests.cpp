#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// Forward declarations for the draw entrypoints under test.
HRESULT AEROGPU_D3D9_CALL device_set_fvf(D3DDDI_HDEVICE hDevice, uint32_t fvf);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    int32_t base_vertex,
    uint32_t min_index,
    uint32_t num_vertices,
    uint32_t start_index,
    uint32_t primitive_count);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t primitive_count,
    const void* pVertexData,
    uint32_t stride_bytes);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t min_vertex_index,
    uint32_t num_vertices,
    uint32_t primitive_count,
    const void* pIndexData,
    D3DDDIFORMAT index_data_format,
    const void* pVertexData,
    uint32_t stride_bytes);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWPRIMITIVE2* pDraw);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWINDEXEDPRIMITIVE2* pDraw);

} // namespace aerogpu

namespace {

size_t CountOpcode(const aerogpu::CmdWriter& cmd, uint32_t opcode) {
  const uint8_t* buf = cmd.data();
  const size_t len = cmd.size();
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

void AssertNoDrawOpcodes(const aerogpu::Device& dev, size_t expected_stream_size) {
  assert(dev.cmd.size() == expected_stream_size);
  assert(CountOpcode(dev.cmd, AEROGPU_CMD_DRAW) == 0);
  assert(CountOpcode(dev.cmd, AEROGPU_CMD_DRAW_INDEXED) == 0);
}

} // namespace

int main() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  // Configure an unsupported fixed-function/FVF state, with no user shaders
  // bound. Fixed-function fallback is only implemented for a narrow subset of
  // FVFs; unsupported values must fail draws cleanly.
  const uint32_t kUnsupportedFvf = 0x2u; // D3DFVF_XYZ
  HRESULT hr = aerogpu::device_set_fvf(hDevice, kUnsupportedFvf);
  assert(hr == S_OK);

  const size_t baseline_size = dev.cmd.size();
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawPrimitive
  hr = aerogpu::device_draw_primitive(hDevice, D3DDDIPT_TRIANGLELIST, 0, 1);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawIndexedPrimitive
  hr = aerogpu::device_draw_indexed_primitive(hDevice, D3DDDIPT_TRIANGLELIST, 0, 0, 0, 0, 1);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawPrimitiveUP
  uint8_t vertices[3 * 16] = {};
  hr = aerogpu::device_draw_primitive_up(hDevice, D3DDDIPT_TRIANGLELIST, 1, vertices, 16);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawIndexedPrimitiveUP
  uint16_t indices[3] = {0, 1, 2};
  hr = aerogpu::device_draw_indexed_primitive_up(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      0,
      3,
      1,
      indices,
      static_cast<D3DDDIFORMAT>(101), // D3DFMT_INDEX16
      vertices,
      16);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawPrimitive2
  D3DDDIARG_DRAWPRIMITIVE2 draw2{};
  draw2.PrimitiveType = D3DDDIPT_TRIANGLELIST;
  draw2.PrimitiveCount = 1;
  draw2.pVertexStreamZeroData = vertices;
  draw2.VertexStreamZeroStride = 16;
  hr = aerogpu::device_draw_primitive2(hDevice, &draw2);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  // DrawIndexedPrimitive2
  D3DDDIARG_DRAWINDEXEDPRIMITIVE2 drawi2{};
  drawi2.PrimitiveType = D3DDDIPT_TRIANGLELIST;
  drawi2.PrimitiveCount = 1;
  drawi2.MinIndex = 0;
  drawi2.NumVertices = 3;
  drawi2.pIndexData = indices;
  drawi2.IndexDataFormat = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  drawi2.pVertexStreamZeroData = vertices;
  drawi2.VertexStreamZeroStride = 16;
  hr = aerogpu::device_draw_indexed_primitive2(hDevice, &drawi2);
  assert(hr == D3DERR_INVALIDCALL);
  AssertNoDrawOpcodes(dev, baseline_size);

  return 0;
}

