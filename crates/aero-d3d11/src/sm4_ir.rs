//! Minimal SM4/SM5 intermediate representation for DXBC → WGSL translation.
//!
//! This IR is intentionally small: it is only meant to cover the handful of
//! instruction/resource features required for FL10_0 bring-up. The decoder that
//! produces this IR lives elsewhere (see Task 454).

use crate::sm4::{ShaderModel, ShaderStage};

/// A decoded SM4/SM5 module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sm4Module {
    /// Shader stage declared by the DXBC version token.
    pub stage: ShaderStage,
    /// Shader model declared by the DXBC version token.
    pub model: ShaderModel,
    /// Non-executable declarations and metadata.
    ///
    /// This includes traditional SM4/SM5 declarations that typically appear before the
    /// instruction stream, as well as non-executable `customdata` blocks (comments, debug data,
    /// immediate constant buffers) which may legally appear both before and within the
    /// instruction stream.
    pub decls: Vec<Sm4Decl>,
    /// Linear instruction stream in execution order.
    pub instructions: Vec<Sm4Inst>,
}

/// A single SM4/SM5 declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4Decl {
    Input {
        reg: u32,
        mask: WriteMask,
    },
    InputSiv {
        reg: u32,
        mask: WriteMask,
        sys_value: u32,
    },
    Output {
        reg: u32,
        mask: WriteMask,
    },
    OutputSiv {
        reg: u32,
        mask: WriteMask,
        sys_value: u32,
    },
    ConstantBuffer {
        slot: u32,
        reg_count: u32,
    },
    Sampler {
        slot: u32,
    },
    ResourceTexture2D {
        slot: u32,
    },
    /// `t#` buffer SRV declaration (raw or structured).
    ///
    /// `stride` is in bytes and is meaningful for [`BufferKind::Structured`]. For
    /// raw buffers it is typically 0.
    ResourceBuffer {
        slot: u32,
        stride: u32,
        kind: BufferKind,
    },
    /// `u#` buffer UAV declaration (raw or structured).
    ///
    /// `stride` is in bytes and is meaningful for [`BufferKind::Structured`]. For
    /// raw buffers it is typically 0.
    UavBuffer {
        slot: u32,
        stride: u32,
        kind: BufferKind,
    },
    /// Compute shader thread group size declared via `dcl_thread_group`.
    ThreadGroupSize {
        x: u32,
        y: u32,
        z: u32,
    },
    /// Non-executable `customdata` block.
    ///
    /// This is emitted by the SM4/SM5 encoder for comments, debug data, immediate constant
    /// buffers, etc. The decoder currently treats all custom data blocks as non-executable and
    /// does not attempt to parse the payload.
    CustomData {
        class: u32,
        /// Total block length in DWORDs (including opcode + class DWORDs).
        len_dwords: u32,
    },
    Unknown {
        opcode: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferKind {
    Raw,
    Structured,
}

/// A single SM4/SM5 instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4Inst {
    Mov {
        dst: DstOperand,
        src: SrcOperand,
    },
    Add {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Mul {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Mad {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
        c: SrcOperand,
    },
    Dp3 {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Dp4 {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Min {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Max {
        dst: DstOperand,
        a: SrcOperand,
        b: SrcOperand,
    },
    Rcp {
        dst: DstOperand,
        src: SrcOperand,
    },
    Rsq {
        dst: DstOperand,
        src: SrcOperand,
    },
    /// `sample dest, coord, t#, s#`
    Sample {
        dst: DstOperand,
        coord: SrcOperand,
        texture: TextureRef,
        sampler: SamplerRef,
    },
    /// `sample_l dest, coord, t#, s#, lod`
    SampleL {
        dst: DstOperand,
        coord: SrcOperand,
        texture: TextureRef,
        sampler: SamplerRef,
        lod: SrcOperand,
    },
    /// `ld dest, coord, t#` (e.g. `Texture2D.Load`).
    ///
    /// Note: `coord` and `lod` are integer-typed in SM4/SM5.
    ///
    /// The current translator still models temporaries as `vec4<f32>`. Integer values can
    /// therefore be represented in two ways:
    ///
    /// - Numeric float values (e.g. some builtins are expanded via `f32(...)`).
    /// - Raw integer bit patterns (common for real DXBC, which writes integer values into the
    ///   untyped register file).
    ///
    /// When emitting WGSL `textureLoad`, the backend picks between `i32(f32)` and
    /// `bitcast<i32>(f32)` per lane to recover an `i32` coordinate/LOD.
    Ld {
        dst: DstOperand,
        /// Texel coordinate (x/y in `.xy`).
        coord: SrcOperand,
        texture: TextureRef,
        /// Mip level. For common `Texture2D.Load(int3(x,y,mip))` forms this is derived
        /// from the third component of `coord`.
        lod: SrcOperand,
    },
    /// `ld_raw dst, addr, t#`
    ///
    /// `addr` is a byte offset into the raw buffer.
    LdRaw {
        dst: DstOperand,
        addr: SrcOperand,
        buffer: BufferRef,
    },
    /// `store_raw u#, addr, value` (mask comes from the `u#` operand write mask).
    StoreRaw {
        uav: UavRef,
        addr: SrcOperand,
        value: SrcOperand,
        mask: WriteMask,
    },
    /// `ld_structured dst, index, offset, t#`
    ///
    /// `index` is the structured element index and `offset` is the byte offset
    /// within the element. Stride comes from the corresponding declaration.
    LdStructured {
        dst: DstOperand,
        index: SrcOperand,
        offset: SrcOperand,
        buffer: BufferRef,
    },
    /// `store_structured u#, index, offset, value`
    ///
    /// `index` is the structured element index and `offset` is the byte offset
    /// within the element. Stride comes from the corresponding declaration.
    StoreStructured {
        uav: UavRef,
        index: SrcOperand,
        offset: SrcOperand,
        value: SrcOperand,
        mask: WriteMask,
    },
    /// A decoded instruction that the IR producer does not model yet.
    ///
    /// This allows the WGSL backend to fail with a precise opcode + instruction
    /// index, instead of the decoder having to reject the entire shader up
    /// front.
    Unknown {
        opcode: u32,
    },
    Ret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegFile {
    Temp,
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegisterRef {
    pub file: RegFile,
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMask(pub u8);

impl WriteMask {
    pub const XYZW: Self = Self(0b1111);
    pub const X: Self = Self(0b0001);
    pub const Y: Self = Self(0b0010);
    pub const Z: Self = Self(0b0100);
    pub const W: Self = Self(0b1000);

    pub fn contains(self, component: u8) -> bool {
        (self.0 & component) != 0
    }
}

/// 4-component swizzle.
///
/// Each lane is 0..=3 for x/y/z/w.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swizzle(pub [u8; 4]);

impl Swizzle {
    pub const XYZW: Self = Self([0, 1, 2, 3]);
    pub const XXXX: Self = Self([0, 0, 0, 0]);
    pub const YYYY: Self = Self([1, 1, 1, 1]);
    pub const ZZZZ: Self = Self([2, 2, 2, 2]);
    pub const WWWW: Self = Self([3, 3, 3, 3]);

    pub fn is_identity(self) -> bool {
        self == Self::XYZW
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandModifier {
    None,
    Neg,
    Abs,
    AbsNeg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstOperand {
    pub reg: RegisterRef,
    pub mask: WriteMask,
    pub saturate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcOperand {
    pub kind: SrcKind,
    pub swizzle: Swizzle,
    pub modifier: OperandModifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrcKind {
    Register(RegisterRef),
    ConstantBuffer {
        slot: u32,
        reg: u32,
    },
    /// Immediate 32-bit floats (IEEE bits).
    ImmediateF32([u32; 4]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureRef {
    pub slot: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerRef {
    pub slot: u32,
}

/// A `t#` shader resource bound as a buffer (e.g. `ByteAddressBuffer` / SRV buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferRef {
    pub slot: u32,
}

/// A `u#` unordered access view (UAV) bound to the shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UavRef {
    pub slot: u32,
}
