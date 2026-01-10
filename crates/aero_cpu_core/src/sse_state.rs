use crate::FxStateError;

/// Mask of MXCSR bits supported by the emulated CPU.
///
/// This value is written to the `MXCSR_MASK` field of the FXSAVE area. Real
/// hardware treats this as a CPU capability (not per-thread state) and ignores
/// the memory image's value on `FXRSTOR`.
pub const MXCSR_MASK: u32 = 0x0000_FFFF;

/// SSE architectural state sufficient for `FXSAVE`/`FXRSTOR`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseState {
    pub xmm: [u128; 16],
    pub mxcsr: u32,
}

impl Default for SseState {
    fn default() -> Self {
        Self {
            xmm: [0u128; 16],
            mxcsr: 0x1F80,
        }
    }
}

impl SseState {
    pub fn set_mxcsr(&mut self, value: u32) -> Result<(), FxStateError> {
        let reserved = value & !MXCSR_MASK;
        if reserved != 0 {
            return Err(FxStateError::MxcsrReservedBits {
                value,
                mask: MXCSR_MASK,
            });
        }
        self.mxcsr = value;
        Ok(())
    }
}
