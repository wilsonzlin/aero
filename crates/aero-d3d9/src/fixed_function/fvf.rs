use wgpu::{VertexAttribute, VertexFormat};

const POSITION_LOCATION: u32 = 0;
const NORMAL_LOCATION: u32 = 1;
const DIFFUSE_LOCATION: u32 = 3;
const SPECULAR_LOCATION: u32 = 4;
const TEXCOORD_BASE_LOCATION: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fvf(pub u32);

impl Fvf {
    pub const XYZ: u32 = 0x0000_0002;
    pub const XYZRHW: u32 = 0x0000_0004;
    pub const NORMAL: u32 = 0x0000_0010;
    pub const DIFFUSE: u32 = 0x0000_0040;
    pub const SPECULAR: u32 = 0x0000_0080;

    pub const TEXCOUNT_MASK: u32 = 0x0000_0F00;
    pub const TEXCOUNT_SHIFT: u32 = 8;

    pub fn has_flag(self, flag: u32) -> bool {
        (self.0 & flag) == flag
    }

    pub fn position_type(self) -> Result<PositionType, FvfError> {
        let has_xyz = self.has_flag(Self::XYZ);
        let has_rhw = self.has_flag(Self::XYZRHW);
        match (has_xyz, has_rhw) {
            (true, false) => Ok(PositionType::Xyz),
            (false, true) => Ok(PositionType::XyzRhw),
            (false, false) => Err(FvfError::MissingPosition),
            (true, true) => Err(FvfError::ConflictingPositionFlags),
        }
    }

    pub fn texcoord_count(self) -> usize {
        ((self.0 & Self::TEXCOUNT_MASK) >> Self::TEXCOUNT_SHIFT) as usize
    }

    pub fn texcoord_size(self, index: usize) -> TexCoordSize {
        // D3D9 encodes texcoord sizes as 2-bit fields starting at bit 16:
        // 00=2, 01=3, 10=4, 11=1 components.
        let bits = (self.0 >> (16 + index * 2)) & 0b11;
        match bits {
            0 => TexCoordSize::Two,
            1 => TexCoordSize::Three,
            2 => TexCoordSize::Four,
            _ => TexCoordSize::One,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PositionType {
    Xyz,
    XyzRhw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexCoordSize {
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
}

impl TexCoordSize {
    pub fn components(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug)]
pub struct FvfLayout {
    pub fvf: Fvf,
    pub position: PositionType,
    pub has_normal: bool,
    pub has_diffuse: bool,
    pub has_specular: bool,
    pub texcoords: Vec<TexCoordSize>,
    pub vertex_stride: u64,
    pub vertex_attributes: Vec<VertexAttribute>,
}

impl FvfLayout {
    pub fn new(fvf: Fvf) -> Result<Self, FvfError> {
        let position = fvf.position_type()?;
        let has_normal = fvf.has_flag(Fvf::NORMAL);
        let has_diffuse = fvf.has_flag(Fvf::DIFFUSE);
        let has_specular = fvf.has_flag(Fvf::SPECULAR);

        let tex_count = fvf.texcoord_count();
        if tex_count > 4 {
            return Err(FvfError::UnsupportedTexcoordCount { count: tex_count });
        }

        let mut texcoords = Vec::with_capacity(tex_count);
        for i in 0..tex_count {
            texcoords.push(fvf.texcoord_size(i));
        }

        let mut vertex_attributes = Vec::new();
        let mut offset: u64 = 0;

        let (pos_format, pos_size) = match position {
            PositionType::Xyz => (VertexFormat::Float32x3, 12),
            PositionType::XyzRhw => (VertexFormat::Float32x4, 16),
        };
        vertex_attributes.push(VertexAttribute {
            format: pos_format,
            offset,
            shader_location: POSITION_LOCATION,
        });
        offset += pos_size;

        if has_normal {
            vertex_attributes.push(VertexAttribute {
                format: VertexFormat::Float32x3,
                offset,
                shader_location: NORMAL_LOCATION,
            });
            offset += 12;
        }

        if has_diffuse {
            vertex_attributes.push(VertexAttribute {
                format: VertexFormat::Unorm8x4,
                offset,
                shader_location: DIFFUSE_LOCATION,
            });
            offset += 4;
        }

        if has_specular {
            vertex_attributes.push(VertexAttribute {
                format: VertexFormat::Unorm8x4,
                offset,
                shader_location: SPECULAR_LOCATION,
            });
            offset += 4;
        }

        for (i, size) in texcoords.iter().enumerate() {
            let (format, byte_len) = match size.components() {
                1 => (VertexFormat::Float32, 4),
                2 => (VertexFormat::Float32x2, 8),
                3 => (VertexFormat::Float32x3, 12),
                4 => (VertexFormat::Float32x4, 16),
                _ => unreachable!("TexCoordSize components must be 1..=4"),
            };

            vertex_attributes.push(VertexAttribute {
                format,
                offset,
                shader_location: TEXCOORD_BASE_LOCATION + (i as u32),
            });
            offset += byte_len;
        }

        Ok(Self {
            fvf,
            position,
            has_normal,
            has_diffuse,
            has_specular,
            texcoords,
            vertex_stride: offset,
            vertex_attributes,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FvfError {
    MissingPosition,
    ConflictingPositionFlags,
    UnsupportedTexcoordCount { count: usize },
}
