#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BuiltinShader {
    Blit,
    SolidColor,
    DebugGrid,
}

impl BuiltinShader {
    pub const ALL: [BuiltinShader; 3] = [
        BuiltinShader::Blit,
        BuiltinShader::SolidColor,
        BuiltinShader::DebugGrid,
    ];
}

pub fn wgsl(shader: BuiltinShader) -> &'static str {
    match shader {
        BuiltinShader::Blit => include_str!("../shaders/blit.wgsl"),
        BuiltinShader::SolidColor => include_str!("../shaders/solid_color.wgsl"),
        BuiltinShader::DebugGrid => include_str!("../shaders/debug_grid.wgsl"),
    }
}

pub fn hash(shader: BuiltinShader) -> u64 {
    fn fnv1a64(bytes: &[u8]) -> u64 {
        const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET_BASIS;
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fnv1a64(wgsl(shader).as_bytes())
}
