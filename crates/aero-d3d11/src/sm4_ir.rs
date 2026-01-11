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
    /// Declarations that appear before the instruction stream.
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
    Unknown {
        opcode: u32,
    },
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
