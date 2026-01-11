use thiserror::Error;

pub const XMM_REG_COUNT: usize = 16;
pub const XMM_BYTES: usize = 16;

/// Default power-on value for MXCSR as specified by Intel SDM.
///
/// WebAssembly does not expose the x86 rounding mode / flush-to-zero controls, so the SIMD JIT
/// currently traps when MXCSR differs from this default to avoid silent miscompilation.
pub const MXCSR_DEFAULT: u32 = 0x1F80;

pub const STATE_SIZE_BYTES: usize = XMM_REG_COUNT * XMM_BYTES + 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseState {
    pub xmm: [u128; XMM_REG_COUNT],
    pub mxcsr: u32,
}

impl Default for SseState {
    fn default() -> Self {
        Self {
            xmm: [0; XMM_REG_COUNT],
            mxcsr: MXCSR_DEFAULT,
        }
    }
}

impl SseState {
    pub fn write_to_bytes(&self, dst: &mut [u8]) -> Result<(), StateError> {
        if dst.len() < STATE_SIZE_BYTES {
            return Err(StateError::BufferTooSmall {
                need: STATE_SIZE_BYTES,
                got: dst.len(),
            });
        }

        for (i, reg) in self.xmm.iter().enumerate() {
            let start = i * XMM_BYTES;
            dst[start..start + XMM_BYTES].copy_from_slice(&reg.to_le_bytes());
        }

        let mxcsr_off = XMM_REG_COUNT * XMM_BYTES;
        dst[mxcsr_off..mxcsr_off + 4].copy_from_slice(&self.mxcsr.to_le_bytes());
        Ok(())
    }

    pub fn read_from_bytes(&mut self, src: &[u8]) -> Result<(), StateError> {
        if src.len() < STATE_SIZE_BYTES {
            return Err(StateError::BufferTooSmall {
                need: STATE_SIZE_BYTES,
                got: src.len(),
            });
        }

        for i in 0..XMM_REG_COUNT {
            let start = i * XMM_BYTES;
            let bytes: [u8; XMM_BYTES] = src[start..start + XMM_BYTES]
                .try_into()
                .expect("slice len matches");
            self.xmm[i] = u128::from_le_bytes(bytes);
        }

        let mxcsr_off = XMM_REG_COUNT * XMM_BYTES;
        self.mxcsr = u32::from_le_bytes(
            src[mxcsr_off..mxcsr_off + 4]
                .try_into()
                .expect("slice len matches"),
        );
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("buffer too small: need {need} bytes, got {got}")]
    BufferTooSmall { need: usize, got: usize },
}
