#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureArg {
    Current,
    Diffuse,
    Texture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureOp {
    Disable,
    SelectArg1,
    SelectArg2,
    Modulate,
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureStageState {
    pub color_op: TextureOp,
    pub color_arg1: TextureArg,
    pub color_arg2: TextureArg,
    pub alpha_op: TextureOp,
    pub alpha_arg1: TextureArg,
    pub alpha_arg2: TextureArg,
}

impl Default for TextureStageState {
    fn default() -> Self {
        Self {
            color_op: TextureOp::Disable,
            color_arg1: TextureArg::Current,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::Disable,
            alpha_arg1: TextureArg::Current,
            alpha_arg2: TextureArg::Current,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FogState {
    pub enabled: bool,
}

impl Default for FogState {
    fn default() -> Self {
        Self { enabled: false }
    }
}
