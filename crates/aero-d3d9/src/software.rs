//! Minimal software implementation of the D3D9 programmable pipeline.
//!
//! This exists to make the shader translation and state mapping testable without
//! requiring a GPU / WebGPU implementation in CI.

use std::collections::HashMap;

use crate::{
    shader::{Dst, Op, RegisterFile, ShaderIr, Src, Swizzle, WriteMask},
    state::{
        AddressMode, BlendFactor, BlendOp, BlendState, FilterMode, SamplerState, VertexDecl,
        VertexElementType,
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vec4 {
    pub const ZERO: Vec4 = Vec4 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 0.0,
    };

    pub fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    pub fn splat(v: f32) -> Self {
        Self::new(v, v, v, v)
    }

    pub fn add(self, rhs: Self) -> Self {
        Self::new(
            self.x + rhs.x,
            self.y + rhs.y,
            self.z + rhs.z,
            self.w + rhs.w,
        )
    }

    pub fn sub(self, rhs: Self) -> Self {
        Self::new(
            self.x - rhs.x,
            self.y - rhs.y,
            self.z - rhs.z,
            self.w - rhs.w,
        )
    }

    pub fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.x * rhs.x,
            self.y * rhs.y,
            self.z * rhs.z,
            self.w * rhs.w,
        )
    }

    pub fn mul_scalar(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs, self.w * rhs)
    }

    pub fn clamp01(self) -> Self {
        Self::new(
            self.x.clamp(0.0, 1.0),
            self.y.clamp(0.0, 1.0),
            self.z.clamp(0.0, 1.0),
            self.w.clamp(0.0, 1.0),
        )
    }

    pub fn dot3(self, rhs: Self) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    pub fn dot4(self, rhs: Self) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z + self.w * rhs.w
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        let c = self.clamp01();
        [
            (c.x * 255.0).round() as u8,
            (c.y * 255.0).round() as u8,
            (c.z * 255.0).round() as u8,
            (c.w * 255.0).round() as u8,
        ]
    }
}

fn swizzle(v: Vec4, swz: Swizzle) -> Vec4 {
    let a = [v.x, v.y, v.z, v.w];
    let idx = |i: u8| a[i as usize];
    Vec4::new(idx(swz.0[0]), idx(swz.0[1]), idx(swz.0[2]), idx(swz.0[3]))
}

fn apply_write_mask(dst: &mut Vec4, mask: WriteMask, value: Vec4) {
    if mask.0 & 0b0001 != 0 {
        dst.x = value.x;
    }
    if mask.0 & 0b0010 != 0 {
        dst.y = value.y;
    }
    if mask.0 & 0b0100 != 0 {
        dst.z = value.z;
    }
    if mask.0 & 0b1000 != 0 {
        dst.w = value.w;
    }
}

#[derive(Debug, Clone)]
pub struct Texture2D {
    pub width: u32,
    pub height: u32,
    /// Row-major, top-left origin, RGBA in 0..1.
    pub texels: Vec<Vec4>,
}

impl Texture2D {
    pub fn from_rgba8(width: u32, height: u32, bytes: &[u8]) -> Self {
        assert_eq!(bytes.len(), (width * height * 4) as usize);
        let mut texels = Vec::with_capacity((width * height) as usize);
        for px in bytes.chunks_exact(4) {
            texels.push(Vec4::new(
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
                px[3] as f32 / 255.0,
            ));
        }
        Self {
            width,
            height,
            texels,
        }
    }

    fn get(&self, x: u32, y: u32) -> Vec4 {
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        self.texels[(y * self.width + x) as usize]
    }

    pub fn sample(&self, sampler: SamplerState, uv: (f32, f32)) -> Vec4 {
        let mut u = uv.0;
        let mut v = uv.1;

        let apply_addr = |coord: &mut f32, mode: AddressMode| match mode {
            AddressMode::Clamp => {
                *coord = coord.clamp(0.0, 1.0);
            }
            AddressMode::Wrap => {
                *coord = coord.fract();
                if *coord < 0.0 {
                    *coord += 1.0;
                }
            }
        };
        apply_addr(&mut u, sampler.address_u);
        apply_addr(&mut v, sampler.address_v);

        // Map [0..1] to texel centers [0..(size-1)].
        let fx = u * (self.width as f32 - 1.0);
        let fy = v * (self.height as f32 - 1.0);

        match (sampler.min_filter, sampler.mag_filter) {
            (FilterMode::Point, FilterMode::Point) => {
                let x = (fx + 0.5).floor() as u32;
                let y = (fy + 0.5).floor() as u32;
                self.get(x, y)
            }
            _ => {
                // Bilinear.
                let x0 = fx.floor().clamp(0.0, (self.width - 1) as f32) as u32;
                let y0 = fy.floor().clamp(0.0, (self.height - 1) as f32) as u32;
                let x1 = (x0 + 1).min(self.width - 1);
                let y1 = (y0 + 1).min(self.height - 1);
                let tx = fx - fx.floor();
                let ty = fy - fy.floor();

                let c00 = self.get(x0, y0);
                let c10 = self.get(x1, y0);
                let c01 = self.get(x0, y1);
                let c11 = self.get(x1, y1);

                let lerp = |a: Vec4, b: Vec4, t: f32| a.mul_scalar(1.0 - t).add(b.mul_scalar(t));
                let cx0 = lerp(c00, c10, tx);
                let cx1 = lerp(c01, c11, tx);
                lerp(cx0, cx1, ty)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderTarget {
    pub width: u32,
    pub height: u32,
    pub color: Vec<Vec4>,
}

impl RenderTarget {
    pub fn new(width: u32, height: u32, clear: Vec4) -> Self {
        Self {
            width,
            height,
            color: vec![clear; (width * height) as usize],
        }
    }

    pub fn clear(&mut self, clear: Vec4) {
        self.color.fill(clear);
    }

    pub fn get(&self, x: u32, y: u32) -> Vec4 {
        self.color[(y * self.width + x) as usize]
    }

    pub fn set(&mut self, x: u32, y: u32, c: Vec4) {
        self.color[(y * self.width + x) as usize] = c;
    }

    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        for c in &self.color {
            out.extend_from_slice(&c.to_rgba8());
        }
        out
    }
}

fn blend_factor(factor: BlendFactor, src: Vec4, dst: Vec4) -> Vec4 {
    match factor {
        BlendFactor::Zero => Vec4::splat(0.0),
        BlendFactor::One => Vec4::splat(1.0),
        BlendFactor::SrcColor => src,
        BlendFactor::OneMinusSrcColor => Vec4::splat(1.0).sub(src),
        BlendFactor::SrcAlpha => Vec4::splat(src.w),
        BlendFactor::OneMinusSrcAlpha => Vec4::splat(1.0 - src.w),
        BlendFactor::DstColor => dst,
        BlendFactor::OneMinusDstColor => Vec4::splat(1.0).sub(dst),
        BlendFactor::DstAlpha => Vec4::splat(dst.w),
        BlendFactor::OneMinusDstAlpha => Vec4::splat(1.0 - dst.w),
    }
}

fn blend(state: BlendState, src: Vec4, dst: Vec4) -> Vec4 {
    if !state.enabled {
        return src;
    }
    let sf = blend_factor(state.src_factor, src, dst);
    let df = blend_factor(state.dst_factor, src, dst);
    let s = src.mul(sf);
    let d = dst.mul(df);
    match state.op {
        BlendOp::Add => s.add(d),
        BlendOp::Subtract => s.sub(d),
        BlendOp::ReverseSubtract => d.sub(s),
    }
}

fn read_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_vertex_element(bytes: &[u8], ty: VertexElementType) -> Vec4 {
    match ty {
        VertexElementType::Float1 => Vec4::new(read_f32(&bytes[0..4]), 0.0, 0.0, 1.0),
        VertexElementType::Float2 => {
            Vec4::new(read_f32(&bytes[0..4]), read_f32(&bytes[4..8]), 0.0, 1.0)
        }
        VertexElementType::Float3 => Vec4::new(
            read_f32(&bytes[0..4]),
            read_f32(&bytes[4..8]),
            read_f32(&bytes[8..12]),
            1.0,
        ),
        VertexElementType::Float4 => Vec4::new(
            read_f32(&bytes[0..4]),
            read_f32(&bytes[4..8]),
            read_f32(&bytes[8..12]),
            read_f32(&bytes[12..16]),
        ),
        VertexElementType::Color => {
            // D3DCOLOR is BGRA8.
            let b = bytes[0] as f32 / 255.0;
            let g = bytes[1] as f32 / 255.0;
            let r = bytes[2] as f32 / 255.0;
            let a = bytes[3] as f32 / 255.0;
            Vec4::new(r, g, b, a)
        }
    }
}

#[derive(Debug, Clone)]
struct VsOut {
    clip_pos: Vec4,
    attr: HashMap<u16, Vec4>,
    tex: HashMap<u16, Vec4>,
}

fn exec_src(
    src: Src,
    temps: &[Vec4],
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &[Vec4; 256],
) -> Vec4 {
    let v = match src.reg.file {
        RegisterFile::Temp => temps
            .get(src.reg.index as usize)
            .copied()
            .unwrap_or(Vec4::ZERO),
        RegisterFile::Input => inputs_v.get(&src.reg.index).copied().unwrap_or(Vec4::ZERO),
        RegisterFile::Texture => inputs_t.get(&src.reg.index).copied().unwrap_or(Vec4::ZERO),
        RegisterFile::Const => constants[src.reg.index as usize],
        _ => Vec4::ZERO,
    };
    swizzle(v, src.swizzle)
}

fn exec_dst(
    dst: Dst,
    temps: &mut [Vec4],
    o_pos: &mut Vec4,
    o_attr: &mut HashMap<u16, Vec4>,
    o_tex: &mut HashMap<u16, Vec4>,
    o_color: &mut Vec4,
    value: Vec4,
) {
    match dst.reg.file {
        RegisterFile::Temp => {
            if let Some(v) = temps.get_mut(dst.reg.index as usize) {
                apply_write_mask(v, dst.mask, value);
            }
        }
        RegisterFile::RastOut => {
            apply_write_mask(o_pos, dst.mask, value);
        }
        RegisterFile::AttrOut => {
            let v = o_attr.entry(dst.reg.index).or_insert(Vec4::ZERO);
            apply_write_mask(v, dst.mask, value);
        }
        RegisterFile::TexCoordOut => {
            let v = o_tex.entry(dst.reg.index).or_insert(Vec4::ZERO);
            apply_write_mask(v, dst.mask, value);
        }
        RegisterFile::ColorOut => {
            apply_write_mask(o_color, dst.mask, value);
        }
        _ => {}
    }
}

fn run_vertex_shader(ir: &ShaderIr, inputs: &HashMap<u16, Vec4>, constants: &[Vec4; 256]) -> VsOut {
    let mut temps = vec![Vec4::ZERO; ir.temp_count as usize];
    let mut o_pos = Vec4::ZERO;
    let mut o_attr = HashMap::<u16, Vec4>::new();
    let mut o_tex = HashMap::<u16, Vec4>::new();
    let mut dummy_color = Vec4::ZERO;

    let empty_t = HashMap::new();
    for inst in &ir.ops {
        match inst.op {
            Op::Nop => {}
            Op::End => break,
            Op::Mov => {
                let dst = inst.dst.unwrap();
                let v = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Add => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    a.add(b),
                );
            }
            Op::Mul => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    a.mul(b),
                );
            }
            Op::Min => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let v = Vec4::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z), a.w.min(b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Max => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let v = Vec4::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z), a.w.max(b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Mad => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let c = exec_src(inst.src[2], &temps, inputs, &empty_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    a.mul(b).add(c),
                );
            }
            Op::Dp3 => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let d = Vec4::splat(a.dot3(b));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    d,
                );
            }
            Op::Dp4 => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let d = Vec4::splat(a.dot4(b));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    d,
                );
            }
            Op::Rcp => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let v = Vec4::new(1.0 / a.x, 1.0 / a.y, 1.0 / a.z, 1.0 / a.w);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Rsq => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let inv_sqrt = |v: f32| 1.0 / v.sqrt();
                let v = Vec4::new(inv_sqrt(a.x), inv_sqrt(a.y), inv_sqrt(a.z), inv_sqrt(a.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Frc => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let fract = |v: f32| v - v.floor();
                let v = Vec4::new(fract(a.x), fract(a.y), fract(a.z), fract(a.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Cmp => {
                let dst = inst.dst.unwrap();
                let cond = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let a = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[2], &temps, inputs, &empty_t, constants);
                let pick = |cond: f32, a: f32, b: f32| if cond >= 0.0 { a } else { b };
                let v = Vec4::new(
                    pick(cond.x, a.x, b.x),
                    pick(cond.y, a.y, b.y),
                    pick(cond.z, a.z, b.z),
                    pick(cond.w, a.w, b.w),
                );
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Slt | Op::Sge => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs, &empty_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs, &empty_t, constants);
                let cmp = |a: f32, b: f32| {
                    if inst.op == Op::Slt {
                        (a < b) as u8 as f32
                    } else {
                        (a >= b) as u8 as f32
                    }
                };
                let v = Vec4::new(cmp(a.x, b.x), cmp(a.y, b.y), cmp(a.z, b.z), cmp(a.w, b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut o_pos,
                    &mut o_attr,
                    &mut o_tex,
                    &mut dummy_color,
                    v,
                );
            }
            Op::Texld => {
                // Unused in vs for our supported subset.
            }
        }
    }

    VsOut {
        clip_pos: o_pos,
        attr: o_attr,
        tex: o_tex,
    }
}

fn run_pixel_shader(
    ir: &ShaderIr,
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &[Vec4; 256],
    textures: &HashMap<u16, Texture2D>,
    sampler_states: &HashMap<u16, SamplerState>,
) -> Vec4 {
    let mut temps = vec![Vec4::ZERO; ir.temp_count as usize];
    let mut o_color = Vec4::ZERO;
    let mut dummy_pos = Vec4::ZERO;
    let mut dummy_attr = HashMap::<u16, Vec4>::new();
    let mut dummy_tex = HashMap::<u16, Vec4>::new();

    for inst in &ir.ops {
        match inst.op {
            Op::Nop => {}
            Op::End => break,
            Op::Mov => {
                let dst = inst.dst.unwrap();
                let v = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Add => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    a.add(b),
                );
            }
            Op::Mul => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    a.mul(b),
                );
            }
            Op::Min => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let v = Vec4::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z), a.w.min(b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Max => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let v = Vec4::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z), a.w.max(b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Mad => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let c = exec_src(inst.src[2], &temps, inputs_v, inputs_t, constants);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    a.mul(b).add(c),
                );
            }
            Op::Dp3 => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let d = Vec4::splat(a.dot3(b));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    d,
                );
            }
            Op::Dp4 => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let d = Vec4::splat(a.dot4(b));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    d,
                );
            }
            Op::Rcp => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let v = Vec4::new(1.0 / a.x, 1.0 / a.y, 1.0 / a.z, 1.0 / a.w);
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Rsq => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let inv_sqrt = |v: f32| 1.0 / v.sqrt();
                let v = Vec4::new(inv_sqrt(a.x), inv_sqrt(a.y), inv_sqrt(a.z), inv_sqrt(a.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Frc => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let fract = |v: f32| v - v.floor();
                let v = Vec4::new(fract(a.x), fract(a.y), fract(a.z), fract(a.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Cmp => {
                let dst = inst.dst.unwrap();
                let cond = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let a = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[2], &temps, inputs_v, inputs_t, constants);
                let pick = |cond: f32, a: f32, b: f32| if cond >= 0.0 { a } else { b };
                let v = Vec4::new(
                    pick(cond.x, a.x, b.x),
                    pick(cond.y, a.y, b.y),
                    pick(cond.z, a.z, b.z),
                    pick(cond.w, a.w, b.w),
                );
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Slt | Op::Sge => {
                let dst = inst.dst.unwrap();
                let a = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let b = exec_src(inst.src[1], &temps, inputs_v, inputs_t, constants);
                let cmp = |a: f32, b: f32| {
                    if inst.op == Op::Slt {
                        (a < b) as u8 as f32
                    } else {
                        (a >= b) as u8 as f32
                    }
                };
                let v = Vec4::new(cmp(a.x, b.x), cmp(a.y, b.y), cmp(a.z, b.z), cmp(a.w, b.w));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    v,
                );
            }
            Op::Texld => {
                let dst = inst.dst.unwrap();
                let coord = exec_src(inst.src[0], &temps, inputs_v, inputs_t, constants);
                let s = inst.sampler.expect("texld requires sampler index");
                let tex = textures.get(&s).expect("missing bound texture");
                let samp = sampler_states.get(&s).copied().unwrap_or_default();
                let sampled = tex.sample(samp, (coord.x, coord.y));
                exec_dst(
                    dst,
                    &mut temps,
                    &mut dummy_pos,
                    &mut dummy_attr,
                    &mut dummy_tex,
                    &mut o_color,
                    sampled,
                );
            }
        }
    }

    o_color
}

#[derive(Debug, Clone)]
struct ScreenVertex {
    x: f32,
    y: f32,
    inv_w: f32,
    attr: HashMap<u16, Vec4>,
    tex: HashMap<u16, Vec4>,
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

/// Draw a triangle list.
pub fn draw(
    target: &mut RenderTarget,
    vs: &ShaderIr,
    ps: &ShaderIr,
    vertex_decl: &VertexDecl,
    vertex_buffer: &[u8],
    indices: Option<&[u16]>,
    constants: &[Vec4; 256],
    textures: &HashMap<u16, Texture2D>,
    sampler_states: &HashMap<u16, SamplerState>,
    blend_state: BlendState,
) {
    let fetch_vertex = |vertex_index: u32| -> HashMap<u16, Vec4> {
        let base = vertex_index as usize * vertex_decl.stride as usize;
        let mut inputs = HashMap::<u16, Vec4>::new();
        for (slot, element) in vertex_decl.elements.iter().enumerate() {
            let off = base + element.offset as usize;
            let bytes = &vertex_buffer[off..off + element.ty.byte_size()];
            inputs.insert(slot as u16, read_vertex_element(bytes, element.ty));
        }
        inputs
    };

    let mut verts = Vec::<ScreenVertex>::new();
    let mut emit_vertex = |vertex_index: u32| {
        let inputs = fetch_vertex(vertex_index);
        let out = run_vertex_shader(vs, &inputs, constants);
        let cp = out.clip_pos;
        let inv_w = 1.0 / cp.w;
        let ndc_x = cp.x * inv_w;
        let ndc_y = cp.y * inv_w;
        let sx = (ndc_x * 0.5 + 0.5) * target.width as f32;
        let sy = (-ndc_y * 0.5 + 0.5) * target.height as f32;
        verts.push(ScreenVertex {
            x: sx,
            y: sy,
            inv_w,
            attr: out.attr,
            tex: out.tex,
        });
    };

    // Process vertices. For simplicity we process all unique vertices referenced by indices or by draw order.
    match indices {
        Some(idx) => {
            let max = idx.iter().copied().max().unwrap_or(0) as u32;
            for i in 0..=max {
                emit_vertex(i);
            }

            for tri in idx.chunks_exact(3) {
                let a = &verts[tri[0] as usize];
                let b = &verts[tri[1] as usize];
                let c = &verts[tri[2] as usize];
                rasterize_triangle(
                    target,
                    ps,
                    a,
                    b,
                    c,
                    constants,
                    textures,
                    sampler_states,
                    blend_state,
                );
            }
        }
        None => {
            let vertex_count = (vertex_buffer.len() / vertex_decl.stride as usize) as u32;
            for i in 0..vertex_count {
                emit_vertex(i);
            }
            for tri in (0..vertex_count).collect::<Vec<_>>().chunks_exact(3) {
                let a = &verts[tri[0] as usize];
                let b = &verts[tri[1] as usize];
                let c = &verts[tri[2] as usize];
                rasterize_triangle(
                    target,
                    ps,
                    a,
                    b,
                    c,
                    constants,
                    textures,
                    sampler_states,
                    blend_state,
                );
            }
        }
    }
}

fn rasterize_triangle(
    target: &mut RenderTarget,
    ps: &ShaderIr,
    a: &ScreenVertex,
    b: &ScreenVertex,
    c: &ScreenVertex,
    constants: &[Vec4; 256],
    textures: &HashMap<u16, Texture2D>,
    sampler_states: &HashMap<u16, SamplerState>,
    blend_state: BlendState,
) {
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
    let max_x = a.x.max(b.x).max(c.x).ceil().min(target.width as f32 - 1.0) as i32;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
    let max_y = a.y.max(b.y).max(c.y).ceil().min(target.height as f32 - 1.0) as i32;

    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area.abs() < f32::EPSILON {
        return;
    }

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let w0 = edge(b.x, b.y, c.x, c.y, px, py);
            let w1 = edge(c.x, c.y, a.x, a.y, px, py);
            let w2 = edge(a.x, a.y, b.x, b.y, px, py);

            // Inside test with consistent winding.
            if (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0) {
                let b0 = w0 / area;
                let b1 = w1 / area;
                let b2 = w2 / area;

                // Perspective-correct interpolation for varyings.
                let inv_w = a.inv_w * b0 + b.inv_w * b1 + c.inv_w * b2;
                let inv_w = inv_w.max(f32::EPSILON);
                let w = 1.0 / inv_w;

                let interp_map = |map_a: &HashMap<u16, Vec4>,
                                  map_b: &HashMap<u16, Vec4>,
                                  map_c: &HashMap<u16, Vec4>| {
                    let mut keys = map_a.keys().copied().collect::<Vec<_>>();
                    keys.extend(map_b.keys().copied());
                    keys.extend(map_c.keys().copied());
                    keys.sort_unstable();
                    keys.dedup();

                    let mut out = HashMap::<u16, Vec4>::new();
                    for k in keys {
                        let va = map_a
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(a.inv_w);
                        let vb = map_b
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(b.inv_w);
                        let vc = map_c
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(c.inv_w);
                        let v = va
                            .mul_scalar(b0)
                            .add(vb.mul_scalar(b1))
                            .add(vc.mul_scalar(b2))
                            .mul_scalar(w);
                        out.insert(k, v);
                    }
                    out
                };

                let attr = interp_map(&a.attr, &b.attr, &c.attr);
                let tex = interp_map(&a.tex, &b.tex, &c.tex);

                let color = run_pixel_shader(ps, &attr, &tex, constants, textures, sampler_states);
                let dst = target.get(x as u32, y as u32);
                let out = blend(blend_state, color, dst).clamp01();
                target.set(x as u32, y as u32, out);
            }
        }
    }
}
