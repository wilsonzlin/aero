use bitflags::bitflags;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureArgSource {
    Current,
    Diffuse,
    Specular,
    Texture,
    /// D3D9 `D3DTA_TFACTOR` (`D3DRS_TEXTUREFACTOR`).
    TextureFactor,
    /// D3D9 `D3DTA_CONSTANT` (`D3DTSS_CONSTANT`).
    Factor,
    /// D3D9 `D3DTA_TEMP` (requires `D3DTSS_RESULTARG` support).
    Temp,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct TextureArgFlags: u8 {
        const COMPLEMENT = 0b0000_0001;
        const ALPHA_REPLICATE = 0b0000_0010;
    }
}

/// A texture-stage argument with optional D3D9 modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureArg {
    pub source: TextureArgSource,
    pub flags: TextureArgFlags,
}

impl TextureArg {
    // These are intentionally PascalCase to mirror the `D3DTA_*` names and to keep
    // call sites compact (`TextureArg::Texture`, etc.).
    #[allow(non_upper_case_globals)]
    pub const Current: Self = Self {
        source: TextureArgSource::Current,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const Diffuse: Self = Self {
        source: TextureArgSource::Diffuse,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const Specular: Self = Self {
        source: TextureArgSource::Specular,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const Texture: Self = Self {
        source: TextureArgSource::Texture,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const TextureFactor: Self = Self {
        source: TextureArgSource::TextureFactor,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const Factor: Self = Self {
        source: TextureArgSource::Factor,
        flags: TextureArgFlags::empty(),
    };
    #[allow(non_upper_case_globals)]
    pub const Temp: Self = Self {
        source: TextureArgSource::Temp,
        flags: TextureArgFlags::empty(),
    };

    pub fn complement(mut self) -> Self {
        self.flags |= TextureArgFlags::COMPLEMENT;
        self
    }

    pub fn alpha_replicate(mut self) -> Self {
        self.flags |= TextureArgFlags::ALPHA_REPLICATE;
        self
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureResultTarget {
    /// Store stage output back into `CURRENT` (default D3D9 behavior).
    Current,
    /// Store stage output into `TEMP` (`D3DTSS_RESULTARG = D3DTA_TEMP`).
    Temp,
}

impl Default for TextureResultTarget {
    fn default() -> Self {
        Self::Current
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureOp {
    Disable,
    SelectArg1,
    SelectArg2,
    Modulate,
    Modulate2x,
    Modulate4x,
    Add,
    AddSigned,
    AddSigned2x,
    Subtract,
    AddSmooth,
    BlendDiffuseAlpha,
    BlendTextureAlpha,
    BlendFactorAlpha,
    BlendTextureAlphaPm,
    BlendCurrentAlpha,
    MultiplyAdd,
    Lerp,
    DotProduct3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureStageState {
    pub color_op: TextureOp,
    pub color_arg0: TextureArg,
    pub color_arg1: TextureArg,
    pub color_arg2: TextureArg,
    pub alpha_op: TextureOp,
    pub alpha_arg0: TextureArg,
    pub alpha_arg1: TextureArg,
    pub alpha_arg2: TextureArg,
    pub result_target: TextureResultTarget,
}

impl Default for TextureStageState {
    fn default() -> Self {
        Self {
            color_op: TextureOp::Disable,
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Current,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::Disable,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Current,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareFunc {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AlphaTestState {
    pub enabled: bool,
    pub func: CompareFunc,
}

impl Default for AlphaTestState {
    fn default() -> Self {
        Self {
            enabled: false,
            func: CompareFunc::Always,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct FogState {
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LightingState {
    pub enabled: bool,
}
