#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace {

// Portable D3D9 shader stage IDs (from d3d9types.h / D3D9 DDI).
constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

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

void AssertHasDrawOpcode(const aerogpu::Device& dev) {
  assert(CountOpcode(dev.cmd, AEROGPU_CMD_DRAW) != 0 || CountOpcode(dev.cmd, AEROGPU_CMD_DRAW_INDEXED) != 0);
}

} // namespace

int main() {
  // ---------------------------------------------------------------------------
  // Case 1: Unsupported fixed-function FVF with no user shaders must fail draws
  // cleanly (D3DERR_INVALIDCALL) without emitting draw packets.
  // ---------------------------------------------------------------------------
  {
    aerogpu::Adapter adapter{};
    aerogpu::Device dev(&adapter);
    D3DDDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = &dev;

    const uint32_t kUnsupportedFvf = 0x2u; // D3DFVF_XYZ (no diffuse)
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
  }

  // ---------------------------------------------------------------------------
  // Case 2: Unsupported fixed-function vertex declaration patterns (SetVertexDecl
  // without SetFVF) must also fail draws cleanly without emitting draw packets.
  // ---------------------------------------------------------------------------
  {
    aerogpu::Adapter adapter{};
    aerogpu::Device dev(&adapter);
    D3DDDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = &dev;

    // Reset any FVF state explicitly.
    HRESULT hr = aerogpu::device_set_fvf(hDevice, 0u);
    assert(hr == S_OK);

    // Minimal vertex declaration with only POSITIONT (no DIFFUSE), which is not
    // a supported fixed-function fallback pattern.
    struct VertexElem {
      uint16_t Stream;
      uint16_t Offset;
      uint8_t Type;
      uint8_t Method;
      uint8_t Usage;
      uint8_t UsageIndex;
    };
    static_assert(sizeof(VertexElem) == 8, "vertex element must match D3DVERTEXELEMENT9 layout");
    constexpr uint8_t kD3dDeclTypeFloat4 = 3;
    constexpr uint8_t kD3dDeclTypeUnused = 17;
    constexpr uint8_t kD3dDeclMethodDefault = 0;
    constexpr uint8_t kD3dDeclUsagePositionT = 9;

    const VertexElem decl[] = {
        {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
    };

    D3D9DDI_HVERTEXDECL hDecl{};
    hr = aerogpu::device_create_vertex_decl(hDevice, decl, static_cast<uint32_t>(sizeof(decl)), &hDecl);
    assert(hr == S_OK);
    assert(hDecl.pDrvPrivate != nullptr);

    hr = aerogpu::device_set_vertex_decl(hDevice, hDecl);
    assert(hr == S_OK);

    const size_t baseline_size = dev.cmd.size();
    AssertNoDrawOpcodes(dev, baseline_size);

    hr = aerogpu::device_draw_primitive(hDevice, D3DDDIPT_TRIANGLELIST, 0, 1);
    assert(hr == D3DERR_INVALIDCALL);
    AssertNoDrawOpcodes(dev, baseline_size);

    hr = aerogpu::device_destroy_vertex_decl(hDevice, hDecl);
    assert(hr == S_OK);
  }

  // ---------------------------------------------------------------------------
  // Case 3: If an app sets an unsupported FVF but *does* bind explicit shaders,
  // draws should proceed (do not treat it as unsupported fixed-function).
  // ---------------------------------------------------------------------------
  {
    aerogpu::Adapter adapter{};
    aerogpu::Device dev(&adapter);
    D3DDDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = &dev;

    const uint32_t kUnsupportedFvf = 0x2u; // D3DFVF_XYZ
    HRESULT hr = aerogpu::device_set_fvf(hDevice, kUnsupportedFvf);
    assert(hr == S_OK);

    D3D9DDI_HSHADER hVs{};
    hr = aerogpu::device_create_shader(
        hDevice,
        kD3d9ShaderStageVs,
        aerogpu::fixedfunc::kVsPassthroughPosColor,
        static_cast<uint32_t>(sizeof(aerogpu::fixedfunc::kVsPassthroughPosColor)),
        &hVs);
    assert(hr == S_OK);
    assert(hVs.pDrvPrivate != nullptr);

    D3D9DDI_HSHADER hPs{};
    hr = aerogpu::device_create_shader(
        hDevice,
        kD3d9ShaderStagePs,
        aerogpu::fixedfunc::kPsPassthroughColor,
        static_cast<uint32_t>(sizeof(aerogpu::fixedfunc::kPsPassthroughColor)),
        &hPs);
    assert(hr == S_OK);
    assert(hPs.pDrvPrivate != nullptr);

    hr = aerogpu::device_set_shader(hDevice, kD3d9ShaderStageVs, hVs);
    assert(hr == S_OK);
    hr = aerogpu::device_set_shader(hDevice, kD3d9ShaderStagePs, hPs);
    assert(hr == S_OK);

    const size_t baseline_size = dev.cmd.size();
    AssertNoDrawOpcodes(dev, baseline_size);

    hr = aerogpu::device_draw_primitive(hDevice, D3DDDIPT_TRIANGLELIST, 0, 1);
    assert(hr == S_OK);
    assert(dev.cmd.size() > baseline_size);
    AssertHasDrawOpcode(dev);

    hr = aerogpu::device_destroy_shader(hDevice, hVs);
    assert(hr == S_OK);
    hr = aerogpu::device_destroy_shader(hDevice, hPs);
    assert(hr == S_OK);
  }

  return 0;
}
