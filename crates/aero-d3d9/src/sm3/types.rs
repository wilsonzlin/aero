#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Pixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderVersion {
    pub stage: ShaderStage,
    pub major: u8,
    pub minor: u8,
}

impl ShaderVersion {
    pub fn is_sm2(&self) -> bool {
        self.major == 2
    }

    pub fn is_sm3(&self) -> bool {
        self.major == 3
    }
}
