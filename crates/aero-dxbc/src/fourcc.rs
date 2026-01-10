use core::fmt;
use std::borrow::Cow;

/// A 4-byte ASCII identifier used throughout `DXBC` containers.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FourCC(
    /// The raw four bytes.
    pub [u8; 4],
);

impl FourCC {
    /// Creates a [`FourCC`] from a 4 byte string.
    ///
    /// Returns `None` if `s` is not exactly 4 bytes long.
    pub fn from_str(s: &str) -> Option<Self> {
        let bytes = s.as_bytes();
        let &[a, b, c, d] = bytes else {
            return None;
        };
        Some(Self([a, b, c, d]))
    }

    /// Interprets this fourcc as UTF-8, replacing invalid bytes with U+FFFD.
    pub fn as_str_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

impl fmt::Debug for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FourCC").field(&self.as_str_lossy()).finish()
    }
}

impl fmt::Display for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str_lossy())
    }
}
