use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc([u8; 4]);

impl FourCc {
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    pub fn from_str(s: &str) -> Self {
        let mut out = [0u8; 4];
        for (dst, src) in out.iter_mut().zip(s.as_bytes().iter().copied()) {
            *dst = src;
        }
        Self(out)
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let printable = self
            .0
            .iter()
            .copied()
            .all(|b| b.is_ascii_graphic() || b == b' ');
        if printable {
            let s = std::str::from_utf8(&self.0).unwrap_or("????");
            write!(f, "{s}")
        } else {
            write!(
                f,
                "0x{:02x}{:02x}{:02x}{:02x}",
                self.0[0], self.0[1], self.0[2], self.0[3]
            )
        }
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, Clone)]
pub struct DxbcChunk<'a> {
    pub fourcc: FourCc,
    pub offset: u32,
    pub size: u32,
    pub data: &'a [u8],
}
