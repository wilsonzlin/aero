// Fullscreen blit shader used by presenters.
//
// The important part here is that presentation policy is *explicit*:
// - optional sRGB encoding (only when the render target is NOT already sRGB)
// - optional premultiplication (only when the canvas is configured for premultiplied alpha)
// - optional "opaque" forcing (to match Windows swapchain semantics)
//
// This prevents the classic "double gamma" and "wrong alpha mode" bugs that cause
// dark output or haloing around translucent UI when composited by the browser.

struct ViewportTransform {
    // Clip-space transform (scale, offset). The host can use this to implement
    // letterboxing/pillarboxing when presenting.
    scale: vec2<f32>,
    offset: vec2<f32>,
}

struct VsOut {
    @builtin(position) position: vec4<f32>,
    // UV origin is TOP-LEFT (0,0) to match D3D/Windows conventions.
    @location(0) uv: vec2<f32>,
}

struct BlitParams {
    // Bitmask of FLAG_* values below.
    flags: u32,
}

const FLAG_APPLY_SRGB_ENCODE: u32 = 1u;
const FLAG_PREMULTIPLY_ALPHA: u32 = 2u;
const FLAG_FORCE_OPAQUE_ALPHA: u32 = 4u;
const FLAG_FLIP_Y: u32 = 8u;

@group(0) @binding(0) var<uniform> viewport: ViewportTransform;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var input_sampler: sampler;
@group(0) @binding(3) var<uniform> params: BlitParams;

fn srgb_encode_channel(x: f32) -> f32 {
    let v = clamp(x, 0.0, 1.0);
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_encode(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_encode_channel(rgb.r),
        srgb_encode_channel(rgb.g),
        srgb_encode_channel(rgb.b),
    );
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Fullscreen quad (two triangles). This is used instead of the classic fullscreen
    // triangle so that viewport scale/offset can letterbox/pillarbox correctly.
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );

    // TOP-LEFT UV origin:
    // (-1,+1) => (0,0)
    // (+1,-1) => (1,1)
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );

    let base_pos = positions[vid];
    let xy = base_pos * viewport.scale + viewport.offset;

    var out: VsOut;
    out.position = vec4<f32>(xy, 0.0, 1.0);
    out.uv = uvs[vid];
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    var uv = input.uv;
    if ((params.flags & FLAG_FLIP_Y) != 0u) {
        uv.y = 1.0 - uv.y;
    }

    var color = textureSample(input_tex, input_sampler, uv);

    // Alpha policy.
    if ((params.flags & FLAG_PREMULTIPLY_ALPHA) != 0u) {
        color = vec4<f32>(color.rgb * color.a, color.a);
    }
    if ((params.flags & FLAG_FORCE_OPAQUE_ALPHA) != 0u) {
        color.a = 1.0;
    }

    // Color policy: apply sRGB encoding only when the output view is *not* already sRGB.
    if ((params.flags & FLAG_APPLY_SRGB_ENCODE) != 0u) {
        color = vec4<f32>(srgb_encode(color.rgb), color.a);
    }

    return color;
}

