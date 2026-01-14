#pragma once

#include <cstdint>

namespace aerogpu {

// Minimal built-in D3D9 SM2.0 shader token streams used by the AeroGPU D3D9 UMD
// fixed-function fallback path (bring-up FVF subset; see `drivers/aerogpu/umd/d3d9/README.md`).
//
// These are intentionally tiny and avoid declarations so they can be consumed by
// early bring-up shader translators (mov/add/mul subset).
namespace fixedfunc {

// -----------------------------------------------------------------------------
// Vertex shaders (vs_2_0)
// -----------------------------------------------------------------------------

// vs_2_0:
//   mov oPos, v0
//   mov oD0, v1       ; D3DCOLOR is BGRA in memory but is presented to shaders as RGBA
//   mov oT0, v0       ; Provide a stable t0 for stage0 texture sampling (minimal fixed-function fallback)
//   end
static constexpr uint32_t kVsPassthroughPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x02000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v2
//   end
static constexpr uint32_t kVsPassthroughPosColorTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x02000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   def c4, 1, 1, 1, 1
//   mov oPos, v0
//   mov oD0, c4
//   mov oT0, v1
//   end
static constexpr uint32_t kVsPassthroughPosWhiteTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x05000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x02000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   dp4 oPos.x, v0, c0
//   dp4 oPos.y, v0, c1
//   dp4 oPos.z, v0, c2
//   dp4 oPos.w, v0, c3
//   mov oD0, v1
//   mov oT0, v0
//   end
//
// The WVP matrix is provided in constants c0-c3 as column vectors (the transpose
// of a row-major matrix) so the shader computes `float4(pos, 1) * WVP` using the
// D3D9 row-vector convention.
static constexpr uint32_t kVsWvpPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x03000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E40000u, // c0.xyzw
    0x03000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x03000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E40002u, // c2.xyzw
    0x03000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E40003u, // c3.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   dp4 oPos.x, v0, c0
//   dp4 oPos.y, v0, c1
//   dp4 oPos.z, v0, c2
//   dp4 oPos.w, v0, c3
//   mov oD0, v1
//   mov oT0, v2
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1
// where the UMD uploads the *columns* of the row-major `world_view_proj` matrix
// into c0..c3 (i.e. transpose for `dp4(v, cN)` row-vector multiplication).
static constexpr uint32_t kVsWvpPosColorTex0[] = {
    0xFFFE0200u, // vs_2_0
    0x03000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E40000u, // c0.xyzw
    0x03000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x03000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E40002u, // c2.xyzw
    0x03000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E40003u, // c3.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   def c4, 1, 1, 1, 1
//   dp4 oPos.x, v0, c0
//   dp4 oPos.y, v0, c1
//   dp4 oPos.z, v0, c2
//   dp4 oPos.w, v0, c3
//   mov oD0, c4
//   mov oT0, v1
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_TEX1
// where the UMD uploads the *columns* of the row-major `world_view_proj` matrix
// into c0..c3 (i.e. transpose for `dp4(v, cN)` row-vector multiplication).
static constexpr uint32_t kVsTransformPosWhiteTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x05000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x03000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E40000u, // c0.xyzw
    0x03000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw

    0x03000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E40002u, // c2.xyzw

    0x03000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E40003u, // c3.xyzw

    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw

    0x0000FFFFu, // end
};

// -----------------------------------------------------------------------------
// Pixel shaders (ps_2_0)
// -----------------------------------------------------------------------------
// ps_2_0:
//   mov oC0, v0
//   end
static constexpr uint32_t kPsPassthroughColor[] = {
    0xFFFF0200u, // ps_2_0
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// PS_ColorOnly (ps_2_0):
//   mov oC0, v0
//   end
static constexpr uint32_t kPsColorOnly[] = {
    0xFFFF0200u, // ps_2_0
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// PS_TextureOnly (ps_2_0):
//   texld r0, t0, s0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsTextureOnly[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// PS_ModulateTexDiffuse (ps_2_0):
//   texld r0, t0, s0
//   mul r0, r0, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsModulateTexDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// Legacy name used by earlier bring-up code.
static constexpr uint32_t kPsTexturedModulateVertexColor[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// -----------------------------------------------------------------------------
// Minimal stage0 fixed-function fallback variants (ps_2_0)
// -----------------------------------------------------------------------------
// These variants are selected by the UMD based on stage0 texture stage state:
// - COLOROP/COLORARG1/COLORARG2
// - ALPHAOP/ALPHAARG1/ALPHAARG2
//
// Supported sources:
// - Diffuse (v0)
// - Texture0 (texld t0,s0)
// - Modulate (Texture0 * Diffuse)
//
// Note: the MODULATE/MODULATE case is implemented by `kPsTexturedModulateVertexColor`.

// ps_2_0:
//   texld r0, t0, s0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0TextureTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mov r0.xyz, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x02000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mov r0.xyz, v0
//   mul r0.w, r0, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x02000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x03000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mov r0.w, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0TextureDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x02000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mul r0.w, r0, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0TextureModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mul r0, r0, v0
//   mov r0.w, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0ModulateDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mul r0.xyz, r0, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0ModulateTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

} // namespace fixedfunc
} // namespace aerogpu
