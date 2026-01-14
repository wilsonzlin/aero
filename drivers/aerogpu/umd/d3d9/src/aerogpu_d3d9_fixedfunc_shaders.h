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
//   mov oT0, v0       ; Provide a stable t0 for fixed-function texture sampling (minimal fixed-function fallback)
//   end
static constexpr uint32_t kVsPassthroughPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant):
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v0
//   rcp r0, v0.w
//   mul r0, v0, r0
//   mov oT0.z, r0     ; Pack post-projection depth (clip_z / clip_w) into TEXCOORD0.z for fixed-function fog
//   end
static constexpr uint32_t kVsPassthroughPosColorFog[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000006u, // rcp (2 operands)
    0x000F0000u, // r0.xyzw
    0x10FF0000u, // v0.wwww
    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v2
//   end
static constexpr uint32_t kVsPassthroughPosColorTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant):
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v2
//   rcp r0, v0.w
//   mul r0, v0, r0
//   mov oT0.z, r0     ; Pack post-projection depth (clip_z / clip_w) into TEXCOORD0.z for fixed-function fog
//   end
static constexpr uint32_t kVsPassthroughPosColorTex1Fog[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x03000006u, // rcp (2 operands)
    0x000F0000u, // r0.xyzw
    0x10FF0000u, // v0.wwww
    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw
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
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant):
//   def c4, 1, 1, 1, 1
//   mov oPos, v0
//   mov oD0, c4
//   mov oT0, v1
//   rcp r0, v0.w
//   mul r0, v0, r0
//   mov oT0.z, r0     ; Pack post-projection depth (clip_z / clip_w) into TEXCOORD0.z for fixed-function fog
//   end
static constexpr uint32_t kVsPassthroughPosWhiteTex1Fog[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x03000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000006u, // rcp (2 operands)
    0x000F0000u, // r0.xyzw
    0x10FF0000u, // v0.wwww
    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, v1
//   mov oT0, v2
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1
//
// This shader expects the UMD to upload the *columns* of the row-major
// WORLD*VIEW*PROJECTION matrix into a reserved high VS constant range
// (c240..c243; i.e. transpose for `dp4(v, cN)` row-vector multiplication).
static constexpr uint32_t kVsWvpPosColorTex0[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosColorTex0 but also packs post-
// projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosColorTex0Fog[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0:
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, v1
//   mov oT0, v0
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_DIFFUSE
//
// Notes:
// - The input declaration supplies POSITION as float3; D3D9 expands it to float4
//   in the shader input register (v0.w = 1).
// - The UMD uploads the *columns* of the row-major WORLD*VIEW*PROJECTION matrix
//   into a reserved high VS constant range (c240..c243).
// - Like `kVsPassthroughPosColor`, we also write oT0 to provide a stable TEXCOORD0
//   stream for fixed-function texture sampling (stages 0..3).
static constexpr uint32_t kVsWvpPosColor[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosColor but also packs post-projection
// depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosColorFog[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0:
//   def c4, 1, 1, 1, 1
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, c4
//   mov oT0, v1
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_TEX1
// where the UMD uploads the *columns* of the row-major `world_view_proj` matrix
// into a reserved high VS constant range (c240..c243; i.e. transpose for
// `dp4(v, cN)` row-vector multiplication).
static constexpr uint32_t kVsTransformPosWhiteTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw
    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw
    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsTransformPosWhiteTex1 but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsTransformPosWhiteTex1Fog[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4 (3 operands)
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40001u, // v1.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0:
//   def c4, 1, 1, 1, 1
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, c4
//   mov oT0, v0
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_NORMAL
//
// Notes:
// - Expects the UMD to upload the *columns* of the row-major WVP matrix into
//   `c240..c243` (see `ensure_fixedfunc_wvp_constants_locked()`).
// - Writes a constant white diffuse color (lighting disabled path).
static constexpr uint32_t kVsWvpPosNormalWhite[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosNormalWhite but also packs post-
// projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosNormalWhiteFog[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def (5 operands)
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0:
//   def c4, 1, 1, 1, 1
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, c4
//   mov oT0, v2
//   end
//
// This shader is used by fixed-function emulation for:
//   D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_TEX1
//
// Lighting-disabled path; see also `kVsWvpLitPosNormalTex1`.
static constexpr uint32_t kVsWvpPosNormalWhiteTex0[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosNormalWhiteTex0 but also packs post-
// projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosNormalWhiteTex0Fog[] = {
    0xFFFE0200u, // vs_2_0
    0x06000051u, // def
    0x200F0004u, // c4.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw
    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw
    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw
    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x20E40004u, // c4.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (lit; XYZ|NORMAL):
//
// Bounded fixed-function lighting subset; see `kVsWvpLitPosNormalDiffuse` for the
// full algorithm and constant layout. This variant assumes the vertex diffuse
// color is {1,1,1,1} (FVF does not include D3DFVF_DIFFUSE).
static constexpr uint32_t kVsWvpLitPosNormal[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz (normal)
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // -------------------------------------------------------------------------
    // Directional lights (up to 4)
    // -------------------------------------------------------------------------

    // Light 0: c211=dir, c212=diffuse, c213=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Light 1: c214=dir, c215=diffuse, c216=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Light 2: c217=dir, c218=diffuse, c219=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Light 3: c220=dir, c221=diffuse, c222=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // -------------------------------------------------------------------------
    // Point lights (up to 2)
    // -------------------------------------------------------------------------

    // Point 0: c223=pos, c224=diffuse, c225=ambient, c226=inv_att0, c227=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // attenuation = inv_att0 * max(1 - dist2*inv_range2, 0)
    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    // diffuseSum += lightDiffuse * ndotl * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    // ambientSum += lightAmbient * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point 1: c228=pos, c229=diffuse, c230=ambient, c231=inv_att0, c232=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // -------------------------------------------------------------------------
    // Apply material + global ambient and output final lit color
    // -------------------------------------------------------------------------

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw (mat diffuse)

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw (global ambient)

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw (mat ambient)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw (emissive)

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x0000FFFFu, // end
};


// vs_2_0 (lit + fog; XYZ|NORMAL): like `kVsWvpLitPosNormal` but also packs post-
// projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpLitPosNormalFog[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz (normal)
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // -------------------------------------------------------------------------
    // Directional lights (up to 4)
    // -------------------------------------------------------------------------

    // Light 0: c211=dir, c212=diffuse, c213=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Light 1: c214=dir, c215=diffuse, c216=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Light 2: c217=dir, c218=diffuse, c219=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Light 3: c220=dir, c221=diffuse, c222=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // -------------------------------------------------------------------------
    // Point lights (up to 2)
    // -------------------------------------------------------------------------

    // Point 0: c223=pos, c224=diffuse, c225=ambient, c226=inv_att0, c227=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // attenuation = inv_att0 * max(1 - dist2*inv_range2, 0)
    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    // diffuseSum += lightDiffuse * ndotl * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    // ambientSum += lightAmbient * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point 1: c228=pos, c229=diffuse, c230=ambient, c231=inv_att0, c232=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // -------------------------------------------------------------------------
    // Apply material + global ambient and output final lit color
    // -------------------------------------------------------------------------

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw (mat diffuse)

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw (global ambient)

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw (mat ambient)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw (emissive)

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// Like `kVsWvpLitPosNormal`, but passes TEXCOORD0 through v2 (XYZ|NORMAL|TEX1).
static constexpr uint32_t kVsWvpLitPosNormalTex1[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw

    // Directional light 0
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Directional light 1
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Directional light 2
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Directional light 3
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // Point light 0
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point light 1
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Apply material + global ambient
    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw (texcoord0)

    0x0000FFFFu, // end
};


// vs_2_0 (lit + fog; XYZ|NORMAL|TEX1): like `kVsWvpLitPosNormalTex1` but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpLitPosNormalTex1Fog[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw

    // Directional light 0
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Directional light 1
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Directional light 2
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Directional light 3
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // Point light 0
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point light 1
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Apply material + global ambient
    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw (texcoord0)

    

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// -----------------------------------------------------------------------------
// Vertex shaders: FVF with normals (minimal fixed-function lighting bring-up)
// -----------------------------------------------------------------------------

// vs_2_0 (unlit; XYZ|NORMAL|DIFFUSE):
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, v2
//   mov oT0, v0
//   end
static constexpr uint32_t kVsWvpPosNormalDiffuse[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40002u, // v2.xyzw (diffuse)

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw (stable t0)
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosNormalDiffuse but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosNormalDiffuseFog[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40002u, // v2.xyzw (diffuse)

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw (stable t0)

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (unlit; XYZ|NORMAL|DIFFUSE|TEX1):
//   dp4 oPos.x, v0, c240
//   dp4 oPos.y, v0, c241
//   dp4 oPos.z, v0, c242
//   dp4 oPos.w, v0, c243
//   mov oD0, v2
//   mov oT0, v3
//   end
static constexpr uint32_t kVsWvpPosNormalDiffuseTex1[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40002u, // v2.xyzw (diffuse)

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40003u, // v3.xyzw (texcoord0)
    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpPosNormalDiffuseTex1 but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpPosNormalDiffuseTex1Fog[] = {
    0xFFFE0200u, // vs_2_0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40002u, // v2.xyzw (diffuse)

    0x03000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40003u, // v3.xyzw (texcoord0)

    0x04000009u, // dp4 (3 operands)
    0x000F0000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40000u, // r0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (lit; XYZ|NORMAL|DIFFUSE):
//
// Bounded fixed-function lighting subset (D3D9 bring-up):
// - global ambient (D3DRS_AMBIENT)
// - material diffuse/ambient/emissive
// - up to 4 directional lights (packed)
// - up to 2 point/spot lights (packed) with:
//   - constant attenuation (1/att0)
//   - range clamp based on dist^2 (max(1 - dist^2/range^2, 0))
//
// The constant register layout is described by `kFixedfuncLightingStartRegister`
// in `aerogpu_d3d9_driver.cpp`.
static constexpr uint32_t kVsWvpLitPosNormalDiffuse[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz (normal)
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // -------------------------------------------------------------------------
    // Directional lights (up to 4)
    // -------------------------------------------------------------------------

    // Light 0: c211=dir, c212=diffuse, c213=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Light 1: c214=dir, c215=diffuse, c216=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Light 2: c217=dir, c218=diffuse, c219=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Light 3: c220=dir, c221=diffuse, c222=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // -------------------------------------------------------------------------
    // Point lights (up to 2)
    // -------------------------------------------------------------------------

    // Point 0: c223=pos, c224=diffuse, c225=ambient, c226=inv_att0, c227=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // attenuation = inv_att0 * max(1 - dist2*inv_range2, 0)
    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    // diffuseSum += lightDiffuse * ndotl * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    // ambientSum += lightAmbient * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point 1: c228=pos, c229=diffuse, c230=ambient, c231=inv_att0, c232=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // -------------------------------------------------------------------------
    // Apply material + global ambient and output final lit color
    // -------------------------------------------------------------------------

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw (mat diffuse)

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw (global ambient)

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw (mat ambient)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x10E40002u, // v2.xyzw (vertex diffuse)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw (emissive)

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpLitPosNormalDiffuse but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpLitPosNormalDiffuseFog[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz (normal)
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq (2 operands)
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // -------------------------------------------------------------------------
    // Directional lights (up to 4)
    // -------------------------------------------------------------------------

    // Light 0: c211=dir, c212=diffuse, c213=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Light 1: c214=dir, c215=diffuse, c216=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Light 2: c217=dir, c218=diffuse, c219=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Light 3: c220=dir, c221=diffuse, c222=ambient
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // -------------------------------------------------------------------------
    // Point lights (up to 2)
    // -------------------------------------------------------------------------

    // Point 0: c223=pos, c224=diffuse, c225=ambient, c226=inv_att0, c227=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw (0)

    // attenuation = inv_att0 * max(1 - dist2*inv_range2, 0)
    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw (1)

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw (0)

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    // diffuseSum += lightDiffuse * ndotl * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    // ambientSum += lightAmbient * attenuation
    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point 1: c228=pos, c229=diffuse, c230=ambient, c231=inv_att0, c232=inv_range2
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw (-1)

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // -------------------------------------------------------------------------
    // Apply material + global ambient and output final lit color
    // -------------------------------------------------------------------------

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw (mat diffuse)

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw (global ambient)

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw (mat ambient)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x10E40002u, // v2.xyzw (vertex diffuse)

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw (emissive)

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw


    0x04000009u, // dp4 (3 operands)
    0x000F0006u, // r6.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0007u, // r7.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0006u, // r6.xyzw
    0x00E40006u, // r6.xyzw
    0x00E40007u, // r7.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40006u, // r6.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (lit; XYZ|NORMAL|DIFFUSE|TEX1): identical to kVsWvpLitPosNormalDiffuse
// but passes TEXCOORD0 through v3.
static constexpr uint32_t kVsWvpLitPosNormalDiffuseTex1[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw

    // Directional light 0
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Directional light 1
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Directional light 2
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Directional light 3
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // Point light 0
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point light 1
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Apply material + global ambient
    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x10E40002u, // v2.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40003u, // v3.xyzw

    0x0000FFFFu, // end
};

// vs_2_0 (fog variant): identical to kVsWvpLitPosNormalDiffuseTex1 but also packs
// post-projection depth (clip_z / clip_w) into TEXCOORD0.z.
static constexpr uint32_t kVsWvpLitPosNormalDiffuseTex1Fog[] = {
    0xFFFE0200u, // vs_2_0

    0x06000051u, // def (5 operands)
    0x200F00FDu, // c253.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0

    0x06000051u, // def
    0x200F00FEu, // c254.xyzw
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0
    0x00000000u, // 0.0

    0x06000051u, // def
    0x200F00FFu, // c255.xyzw
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0
    0x3F800000u, // 1.0

    0x04000009u, // dp4 (3 operands)
    0x40010000u, // oPos.x
    0x10E40000u, // v0.xyzw
    0x20E400F0u, // c240.xyzw

    0x04000009u, // dp4
    0x40020000u, // oPos.y
    0x10E40000u, // v0.xyzw
    0x20E400F1u, // c241.xyzw

    0x04000009u, // dp4
    0x40040000u, // oPos.z
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4
    0x40080000u, // oPos.w
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x04000008u, // dp3
    0x00010000u, // r0.x
    0x10E40001u, // v1.xyz
    0x20E400D0u, // c208.xyz

    0x04000008u, // dp3
    0x00020000u, // r0.y
    0x10E40001u, // v1.xyz
    0x20E400D1u, // c209.xyz

    0x04000008u, // dp3
    0x00040000u, // r0.z
    0x10E40001u, // v1.xyz
    0x20E400D2u, // c210.xyz

    0x04000008u, // dp3
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw

    0x03000007u, // rsq
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw

    0x04000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw

    // View-space position (r6.xyz): dp4(v0, world*view colN)
    0x04000009u, // dp4
    0x00010006u, // r6.x
    0x10E40000u, // v0.xyzw
    0x20E400D0u, // c208.xyzw

    0x04000009u, // dp4
    0x00020006u, // r6.y
    0x10E40000u, // v0.xyzw
    0x20E400D1u, // c209.xyzw

    0x04000009u, // dp4
    0x00040006u, // r6.z
    0x10E40000u, // v0.xyzw
    0x20E400D2u, // c210.xyzw

    // Accumulators: r2 = diffuseSum, r3 = ambientSum
    0x03000001u, // mov
    0x000F0002u, // r2.xyzw
    0x20E400FEu, // c254.xyzw

    0x03000001u, // mov
    0x000F0003u, // r3.xyzw
    0x20E400FEu, // c254.xyzw

    // Directional light 0
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D3u, // c211.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D4u, // c212.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D5u, // c213.xyzw

    // Directional light 1
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D6u, // c214.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400D7u, // c215.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400D8u, // c216.xyzw

    // Directional light 2
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400D9u, // c217.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DAu, // c218.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DBu, // c219.xyzw

    // Directional light 3
    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400DCu, // c220.xyz

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400DDu, // c221.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400DEu, // c222.xyzw

    // Point light 0
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400DFu, // c223.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E3u, // c227.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E2u, // c226.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E0u, // c224.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E1u, // c225.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Point light 1
    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40006u, // r6.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0004u, // r4.xyzw
    0x20E400E4u, // c228.xyzw
    0x00E40004u, // r4.xyzw

    0x04000008u, // dp3
    0x000F0005u, // r5.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40004u, // r4.xyzw

    0x03000007u, // rsq
    0x000F0007u, // r7.xyzw
    0x00E40005u, // r5.xyzw

    0x04000005u, // mul
    0x000F0004u, // r4.xyzw
    0x00E40004u, // r4.xyzw
    0x00E40007u, // r7.xyzw

    0x04000008u, // dp3
    0x000F0007u, // r7.xyzw
    0x00E40000u, // r0.xyzw
    0x00E40004u, // r4.xyzw

    0x0400000Bu, // max
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E8u, // c232.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FDu, // c253.xyzw

    0x04000002u, // add
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FFu, // c255.xyzw

    0x0400000Bu, // max
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400FEu, // c254.xyzw

    0x04000005u, // mul
    0x000F0005u, // r5.xyzw
    0x00E40005u, // r5.xyzw
    0x20E400E7u, // c231.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E5u, // c229.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x00E40008u, // r8.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40008u, // r8.xyzw

    0x04000005u, // mul
    0x000F0008u, // r8.xyzw
    0x20E400E6u, // c230.xyzw
    0x00E40005u, // r5.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x00E40008u, // r8.xyzw

    // Apply material + global ambient
    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400E9u, // c233.xyzw

    0x04000002u, // add
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400ECu, // c236.xyzw

    0x04000005u, // mul
    0x000F0003u, // r3.xyzw
    0x00E40003u, // r3.xyzw
    0x20E400EAu, // c234.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x00E40003u, // r3.xyzw

    0x04000005u, // mul
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x10E40002u, // v2.xyzw

    0x04000002u, // add
    0x000F0002u, // r2.xyzw
    0x00E40002u, // r2.xyzw
    0x20E400EBu, // c235.xyzw

    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x00E40002u, // r2.xyzw

    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40003u, // v3.xyzw


    0x04000009u, // dp4 (3 operands)
    0x000F0006u, // r6.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F2u, // c242.xyzw

    0x04000009u, // dp4 (3 operands)
    0x000F0007u, // r7.xyzw
    0x10E40000u, // v0.xyzw
    0x20E400F3u, // c243.xyzw

    0x03000006u, // rcp (2 operands)
    0x000F0007u, // r7.xyzw
    0x00E40007u, // r7.xyzw

    0x04000005u, // mul (3 operands)
    0x000F0006u, // r6.xyzw
    0x00E40006u, // r6.xyzw
    0x00E40007u, // r7.xyzw

    0x03000001u, // mov (2 operands)
    0x60040000u, // oT0.z
    0x00E40006u, // r6.xyzw

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
    0x03000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// PS_ColorOnly (ps_2_0):
//   mov oC0, v0
//   end
static constexpr uint32_t kPsColorOnly[] = {
    0xFFFF0200u, // ps_2_0
    0x03000001u, // mov (2 operands)
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
    0x04000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov (2 operands)
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
    0x04000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// Legacy name used by earlier bring-up code.
static constexpr uint32_t kPsTexturedModulateVertexColor[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// -----------------------------------------------------------------------------
// Legacy stage0 fixed-function fallback variants (ps_2_0)
// -----------------------------------------------------------------------------
// These were used by the initial bring-up implementation, which hard-selected
// from a tiny set of pre-baked pixel shader token streams.
//
// Texture stage emulation has since been expanded: the D3D9 UMD now synthesizes a
// `ps_2_0` token stream at runtime based on the supported subset of texture stage
// state across stages 0..3 (see `fixedfunc_ps20` in `aerogpu_d3d9_driver.cpp`).
//
// The tables below are kept as a reference for the expected instruction
// encodings and as a convenient source of minimal token streams.

// ps_2_0:
//   texld r0, t0, s0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsStage0TextureTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov (2 operands)
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
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
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// -----------------------------------------------------------------------------
// Extended stage0 fixed-function fallback variants (ps_2_0)
// -----------------------------------------------------------------------------
// These variants extend the bring-up stage0 texture combiner subset with a few
// additional D3DTOP_* operations and the D3DTA_TFACTOR source (provided via PS
// constant c255 by the UMD).
//
// Notes:
// - These shaders intentionally avoid declarations and stick to a small set of
//   ALU ops (mov/add/mul + def) so they remain compatible with minimal SM2.0
//   translators.
// - "Alpha = <X>" means the output alpha component is sourced independently
//   from RGB (matching the UMD's stage0 COLOROP vs ALPHAOP handling).

// ps_2_0 (stage0): COLOR = TEXTURE + DIFFUSE, ALPHA = TEXTURE
//   texld r0, t0, s0
//   add  r1, r0, v0
//   mov  r1.w, r0
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0AddTextureDiffuseAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000002u, // add
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x00080001u, // r1.w
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE + DIFFUSE, ALPHA = DIFFUSE
//   texld r0, t0, s0
//   add  r0.xyz, r0, v0
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0AddTextureDiffuseAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE + DIFFUSE, ALPHA = TEXTURE * DIFFUSE
//   texld r0, t0, s0
//   add  r0.xyz, r0, v0
//   mul  r0.w, r0, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0AddTextureDiffuseAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 2, ALPHA = TEXTURE
//   texld r0, t0, s0
//   mul  r1, r0, v0
//   add  r1, r1, r1
//   mov  r1.w, r0
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0Modulate2xTextureDiffuseAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x00080001u, // r1.w
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 2, ALPHA = DIFFUSE
//   texld r0, t0, s0
//   mul  r0.xyz, r0, v0
//   add  r0.xyz, r0, r0
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0Modulate2xTextureDiffuseAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 2, ALPHA = TEXTURE * DIFFUSE
//   texld r0, t0, s0
//   mul  r1, r0, v0
//   add  r1.xyz, r1, r1
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0Modulate2xTextureDiffuseAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00070001u, // r1.xyz
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 4, ALPHA = TEXTURE
//   texld r0, t0, s0
//   mul  r1, r0, v0
//   add  r1, r1, r1
//   add  r1, r1, r1
//   mov  r1.w, r0
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0Modulate4xTextureDiffuseAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x04000002u, // add
    0x000F0001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x00080001u, // r1.w
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 4, ALPHA = DIFFUSE
//   texld r0, t0, s0
//   mul  r0.xyz, r0, v0
//   add  r0.xyz, r0, r0
//   add  r0.xyz, r0, r0
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0Modulate4xTextureDiffuseAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * DIFFUSE * 4, ALPHA = TEXTURE * DIFFUSE
//   texld r0, t0, s0
//   mul  r1, r0, v0
//   add  r1.xyz, r1, r1
//   add  r1.xyz, r1, r1
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0Modulate4xTextureDiffuseAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00070001u, // r1.xyz
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x04000002u, // add
    0x00070001u, // r1.xyz
    0x00E40001u, // r1.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE - DIFFUSE, ALPHA = TEXTURE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, v0, c1
//   add  r0.xyz, r0, r1
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractTextureDiffuseAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE - DIFFUSE, ALPHA = DIFFUSE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, v0, c1
//   add  r0.xyz, r0, r1
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractTextureDiffuseAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE - DIFFUSE, ALPHA = TEXTURE * DIFFUSE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, v0, c1
//   add  r0.xyz, r0, r1
//   mul  r0.w, r0, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractTextureDiffuseAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE - TEXTURE, ALPHA = TEXTURE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, r0, c1
//   add  r0.xyz, v0, r1
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractDiffuseTextureAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE - TEXTURE, ALPHA = DIFFUSE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, r0, c1
//   add  r0.xyz, v0, r1
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractDiffuseTextureAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE - TEXTURE, ALPHA = TEXTURE * DIFFUSE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mul  r1, r0, c1
//   add  r0.xyz, v0, r1
//   mul  r0.w, r0, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0SubtractDiffuseTextureAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x00E40001u, // r1.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * TFACTOR (RGB), ALPHA = TEXTURE
//   texld r0, t0, s0
//   mul  r1, r0, c255      ; c255 is TFACTOR
//   mov  r1.w, r0
//   mov  oC0, r1
//   end
static constexpr uint32_t kPsStage0ModulateTextureTFactorAlphaTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x20E400FFu, // c255.xyzw
    0x03000001u, // mov
    0x00080001u, // r1.w
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40001u, // r1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * TFACTOR (RGB), ALPHA = DIFFUSE
//   texld r0, t0, s0
//   mul  r0.xyz, r0, c255
//   mov  r0.w, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0ModulateTextureTFactorAlphaDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x20E400FFu, // c255.xyzw
    0x03000001u, // mov
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TEXTURE * TFACTOR (RGB), ALPHA = TEXTURE * DIFFUSE
//   texld r0, t0, s0
//   mul  r0.xyz, r0, c255
//   mul  r0.w, r0, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0ModulateTextureTFactorAlphaModulate[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x04000005u, // mul
    0x00070000u, // r0.xyz
    0x00E40000u, // r0.xyzw
    0x20E400FFu, // c255.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE, ALPHA = TEXTURE + DIFFUSE
//   texld r0, t0, s0
//   mov  r0.xyz, v0
//   add  r0.w, r0, v0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseAlphaAddTextureDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE, ALPHA = TEXTURE - DIFFUSE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mov  r0.xyz, v0
//   mul  r1, v0, c1
//   add  r0.w, r0, r1
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseAlphaSubtractTextureDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x10E40000u, // v0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE, ALPHA = DIFFUSE - TEXTURE
//   def  c1, -1, -1, -1, -1
//   texld r0, t0, s0
//   mov  r0.xyz, v0
//   mul  r1, r0, c1
//   add  r0.w, v0, r1
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseAlphaSubtractDiffuseTexture[] = {
    0xFFFF0200u, // ps_2_0
    0x06000051u, // def
    0x200F0001u, // c1.xyzw
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0xBF800000u, // -1.0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x000F0001u, // r1.xyzw
    0x00E40000u, // r0.xyzw
    0x20E40001u, // c1.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x10E40000u, // v0.xyzw
    0x00E40001u, // r1.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE, ALPHA = (TEXTURE * DIFFUSE) * 2
//   texld r0, t0, s0
//   mov  r0.xyz, v0
//   mul  r0.w, r0, v0
//   add  r0.w, r0, r0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseAlphaModulate2xTextureDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = DIFFUSE, ALPHA = (TEXTURE * DIFFUSE) * 4
//   texld r0, t0, s0
//   mov  r0.xyz, v0
//   mul  r0.w, r0, v0
//   add  r0.w, r0, r0
//   add  r0.w, r0, r0
//   mov  oC0, r0
//   end
static constexpr uint32_t kPsStage0DiffuseAlphaModulate4xTextureDiffuse[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, // texld
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000001u, // mov
    0x00070000u, // r0.xyz
    0x10E40000u, // v0.xyzw
    0x04000005u, // mul
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x04000002u, // add
    0x00080000u, // r0.w
    0x00E40000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

// ps_2_0 (stage0): COLOR = TFACTOR, ALPHA = TFACTOR
//   mov oC0, c255
//   end
static constexpr uint32_t kPsStage0TextureFactor[] = {
    0xFFFF0200u, // ps_2_0
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x20E400FFu, // c255.xyzw
    0x0000FFFFu, // end
};

} // namespace fixedfunc
} // namespace aerogpu
