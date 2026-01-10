//! Minimal x87 FPU model for interpreter use.
//!
//! This implementation focuses on functional legacy compatibility and uses `f64` for
//! register contents. Real x87 registers are 80-bit extended precision; the reduced
//! precision means some results (especially integer conversions and intermediate
//! rounding) will differ from hardware.

use core::fmt;

use crate::fpu::FpuState;

/// #MF (x87 floating-point error) would be raised for an unmasked exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    MathFault,
}

pub type Result<T> = core::result::Result<T, Fault>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Tag {
    Valid = 0b00,
    Zero = 0b01,
    Special = 0b10,
    Empty = 0b11,
}

impl Tag {
    fn from_f64(v: f64) -> Self {
        if v.is_nan() || v.is_infinite() || v.is_subnormal() {
            Tag::Special
        } else if v == 0.0 {
            Tag::Zero
        } else {
            Tag::Valid
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Eflags {
    pub cf: bool,
    pub pf: bool,
    pub zf: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundingControl {
    NearestEven,
    Down,
    Up,
    TowardZero,
}

impl RoundingControl {
    fn from_fcw(fcw: u16) -> Self {
        match (fcw >> 10) & 0b11 {
            0b00 => RoundingControl::NearestEven,
            0b01 => RoundingControl::Down,
            0b10 => RoundingControl::Up,
            0b11 => RoundingControl::TowardZero,
            _ => unreachable!(),
        }
    }

    fn round(self, v: f64) -> f64 {
        match self {
            RoundingControl::NearestEven => v.round_ties_even(),
            RoundingControl::Down => v.floor(),
            RoundingControl::Up => v.ceil(),
            RoundingControl::TowardZero => v.trunc(),
        }
    }
}

const FCW_DEFAULT: u16 = 0x037F;

const FCW_EXCEPTION_MASK: u16 = 0b11_1111;

const FSW_IE: u16 = 1 << 0;
#[allow(dead_code)]
const FSW_DE: u16 = 1 << 1;
const FSW_ZE: u16 = 1 << 2;
#[allow(dead_code)]
const FSW_OE: u16 = 1 << 3;
#[allow(dead_code)]
const FSW_UE: u16 = 1 << 4;
#[allow(dead_code)]
const FSW_PE: u16 = 1 << 5;
const FSW_SF: u16 = 1 << 6;
const FSW_ES: u16 = 1 << 7;
const FSW_C0: u16 = 1 << 8;
const FSW_C1: u16 = 1 << 9;
const FSW_C2: u16 = 1 << 10;
const FSW_TOP_MASK: u16 = 0b111 << 11;
const FSW_C3: u16 = 1 << 14;

/// Simplified x87 state: 8-register stack, TOP pointer, control/status/tag words.
#[derive(Clone)]
pub struct X87 {
    regs: [f64; 8],
    tags: [Tag; 8],
    top: u8,
    fcw: u16,
    fsw: u16,
}

impl fmt::Debug for X87 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X87")
            .field("top", &self.top())
            .field("fcw", &format_args!("{:#06x}", self.fcw))
            .field("fsw", &format_args!("{:#06x}", self.status_word()))
            .field("tag", &format_args!("{:#06x}", self.tag_word()))
            .finish()
    }
}

impl Default for X87 {
    fn default() -> Self {
        let mut s = Self {
            regs: [0.0; 8],
            tags: [Tag::Empty; 8],
            top: 0,
            fcw: FCW_DEFAULT,
            fsw: 0,
        };
        s.sync_top();
        s
    }
}

impl X87 {
    /// Populate this runtime x87 state from an `FXSAVE`/`FXRSTOR` compatible image.
    ///
    /// Note: the `FpuState` register image stores 80-bit values. This model decodes
    /// them into `f64`, so it cannot preserve full x87 precision.
    pub fn load_from_fpu_state(&mut self, state: &FpuState) {
        self.fcw = state.fcw;
        self.top = state.top & 7;

        // Preserve all status flags, but make sure TOP matches `state.top`.
        self.fsw = (state.fsw & !FSW_TOP_MASK) | ((self.top as u16) << 11);

        for i in 0..8 {
            if (state.ftw & (1 << i)) == 0 {
                self.tags[i] = Tag::Empty;
                self.regs[i] = 0.0;
            } else {
                let v = f64_from_ext80(state.st[i]);
                self.regs[i] = v;
                self.tags[i] = Tag::from_f64(v);
            }
        }

        self.sync_top();
        self.sync_es();
    }

    /// Store this runtime x87 state back into an `FXSAVE`/`FXRSTOR` compatible image.
    ///
    /// This updates the control/status/tag words and the 80-bit register image. The
    /// instruction/data pointers are left untouched.
    pub fn store_to_fpu_state(&self, state: &mut FpuState) {
        state.fcw = self.fcw;
        state.top = self.top & 7;
        state.fsw = self.fsw & !FSW_TOP_MASK;

        let mut ftw: u8 = 0;
        for i in 0..8 {
            if !matches!(self.tags[i], Tag::Empty) {
                ftw |= 1 << i;
                state.st[i] = ext80_from_f64(self.regs[i]);
            } else {
                state.st[i] = 0;
            }
        }
        state.ftw = ftw;
    }

    pub fn fninit(&mut self) {
        *self = Self::default();
    }

    /// Implements `FNCLEX` / `FCLEX`.
    ///
    /// Clears the exception status flags and the exception summary bit.
    pub fn fnclex(&mut self) {
        self.fsw &= !(FSW_IE | FSW_DE | FSW_ZE | FSW_OE | FSW_UE | FSW_PE | FSW_SF | FSW_ES);
        self.sync_es();
    }

    pub fn control_word(&self) -> u16 {
        self.fcw
    }

    pub fn status_word(&self) -> u16 {
        self.fsw
    }

    pub fn tag_word(&self) -> u16 {
        let mut tw = 0u16;
        for (i, tag) in self.tags.iter().copied().enumerate() {
            tw |= (tag as u16) << (i * 2);
        }
        tw
    }

    pub fn top(&self) -> u8 {
        self.top
    }

    pub fn st(&self, i: usize) -> Option<f64> {
        let phys = self.phys_index(i)?;
        match self.tags[phys] {
            Tag::Empty => None,
            _ => Some(self.regs[phys]),
        }
    }

    pub fn st_tag(&self, i: usize) -> Option<Tag> {
        let phys = self.phys_index(i)?;
        Some(self.tags[phys])
    }

    pub fn fldcw(&mut self, cw: u16) {
        self.fcw = cw;
        self.sync_es();
    }

    pub fn fnstcw(&self) -> u16 {
        self.fcw
    }

    pub fn fnstsw(&self) -> u16 {
        self.status_word()
    }

    pub fn fld_f32(&mut self, v: f32) -> Result<()> {
        self.push(v as f64)
    }

    pub fn fld_f64(&mut self, v: f64) -> Result<()> {
        self.push(v)
    }

    pub fn fld_st(&mut self, i: usize) -> Result<()> {
        let v = self.read_st(i)?;
        self.push(v)
    }

    pub fn fld1(&mut self) -> Result<()> {
        self.push(1.0)
    }

    pub fn fldz(&mut self) -> Result<()> {
        self.push(0.0)
    }

    pub fn fst_f32(&mut self) -> Result<f32> {
        Ok(self.read_st(0)? as f32)
    }

    pub fn fst_f64(&mut self) -> Result<f64> {
        self.read_st(0)
    }

    pub fn fstp_f32(&mut self) -> Result<f32> {
        let v = self.read_st(0)? as f32;
        self.pop()?;
        Ok(v)
    }

    pub fn fstp_f64(&mut self) -> Result<f64> {
        let v = self.read_st(0)?;
        self.pop()?;
        Ok(v)
    }

    pub fn fst_st(&mut self, i: usize) -> Result<()> {
        let v = self.read_st(0)?;
        self.write_st(i, v)
    }

    pub fn fstp_st(&mut self, i: usize) -> Result<()> {
        let v = self.read_st(0)?;
        self.write_st(i, v)?;
        self.pop()
    }

    pub fn fxch_sti(&mut self, i: usize) -> Result<()> {
        if i == 0 {
            return Ok(());
        }
        let st0 = self.phys_index(0).ok_or(Fault::MathFault)?;
        let sti = self.phys_index(i).ok_or(Fault::MathFault)?;
        if matches!(self.tags[st0], Tag::Empty) || matches!(self.tags[sti], Tag::Empty) {
            // Swap involving an empty register is a stack underflow.
            self.stack_underflow()?;
        }
        self.regs.swap(st0, sti);
        self.tags.swap(st0, sti);
        Ok(())
    }

    pub fn ffree_sti(&mut self, i: usize) -> Result<()> {
        let phys = self.phys_index(i).ok_or(Fault::MathFault)?;
        self.tags[phys] = Tag::Empty;
        self.regs[phys] = 0.0;
        Ok(())
    }

    pub fn ffreep_sti(&mut self, i: usize) -> Result<()> {
        let st0 = self.phys_index(0).ok_or(Fault::MathFault)?;
        if matches!(self.tags[st0], Tag::Empty) {
            self.stack_underflow()?;
        }
        self.ffree_sti(i)?;
        // Pop regardless of whether the freed register was ST0.
        self.tags[st0] = Tag::Empty;
        self.regs[st0] = 0.0;
        self.top = (self.top + 1) & 7;
        self.sync_top();
        Ok(())
    }

    /// Implements `FINCSTP` (increment TOP).
    pub fn fincstp(&mut self) {
        self.top = (self.top + 1) & 7;
        // SDM: C1 cleared for FINCSTP/FDECSTP.
        self.fsw &= !FSW_C1;
        self.sync_top();
    }

    /// Implements `FDECSTP` (decrement TOP).
    pub fn fdecstp(&mut self) {
        self.top = (self.top + 7) & 7;
        self.fsw &= !FSW_C1;
        self.sync_top();
    }

    pub fn fild_i16(&mut self, v: i16) -> Result<()> {
        self.push(v as f64)
    }

    pub fn fild_i32(&mut self, v: i32) -> Result<()> {
        self.push(v as f64)
    }

    pub fn fild_i64(&mut self, v: i64) -> Result<()> {
        self.push(v as f64)
    }

    pub fn fistp_i16(&mut self) -> Result<i16> {
        let v = self.read_st(0)?;
        let rc = RoundingControl::from_fcw(self.fcw);
        let rounded = rc.round(v);
        let out = if !rounded.is_finite() || rounded < i16::MIN as f64 || rounded > i16::MAX as f64
        {
            self.signal_invalid(false)?;
            i16::MIN
        } else {
            rounded as i16
        };
        self.pop()?;
        Ok(out)
    }

    pub fn fistp_i32(&mut self) -> Result<i32> {
        let v = self.read_st(0)?;
        let rc = RoundingControl::from_fcw(self.fcw);
        let rounded = rc.round(v);
        let out = if !rounded.is_finite() || rounded < i32::MIN as f64 || rounded > i32::MAX as f64
        {
            self.signal_invalid(false)?;
            i32::MIN
        } else {
            rounded as i32
        };
        self.pop()?;
        Ok(out)
    }

    pub fn fistp_i64(&mut self) -> Result<i64> {
        let v = self.read_st(0)?;
        let rc = RoundingControl::from_fcw(self.fcw);
        let rounded = rc.round(v);
        let out = if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded > i64::MAX as f64
        {
            self.signal_invalid(false)?;
            i64::MIN
        } else {
            rounded as i64
        };
        self.pop()?;
        Ok(out)
    }

    pub fn fadd_m32(&mut self, v: f32) -> Result<()> {
        self.fadd_m64(v as f64)
    }

    pub fn fadd_m64(&mut self, v: f64) -> Result<()> {
        let st0 = self.read_st(0)?;
        self.write_st(0, st0 + v)
    }

    pub fn fsub_m32(&mut self, v: f32) -> Result<()> {
        self.fsub_m64(v as f64)
    }

    pub fn fsub_m64(&mut self, v: f64) -> Result<()> {
        let st0 = self.read_st(0)?;
        self.write_st(0, st0 - v)
    }

    pub fn fsubr_m32(&mut self, v: f32) -> Result<()> {
        self.fsubr_m64(v as f64)
    }

    pub fn fsubr_m64(&mut self, v: f64) -> Result<()> {
        let st0 = self.read_st(0)?;
        self.write_st(0, v - st0)
    }

    pub fn fmul_m32(&mut self, v: f32) -> Result<()> {
        self.fmul_m64(v as f64)
    }

    pub fn fmul_m64(&mut self, v: f64) -> Result<()> {
        let st0 = self.read_st(0)?;
        self.write_st(0, st0 * v)
    }

    pub fn fdiv_m32(&mut self, v: f32) -> Result<()> {
        self.fdiv_m64(v as f64)
    }

    pub fn fdiv_m64(&mut self, v: f64) -> Result<()> {
        if v == 0.0 {
            self.signal_zero_divide()?;
        }
        let st0 = self.read_st(0)?;
        self.write_st(0, st0 / v)
    }

    pub fn fdivr_m32(&mut self, v: f32) -> Result<()> {
        self.fdivr_m64(v as f64)
    }

    pub fn fdivr_m64(&mut self, v: f64) -> Result<()> {
        let denom = self.read_st(0)?;
        if denom == 0.0 {
            self.signal_zero_divide()?;
        }
        self.write_st(0, v / denom)
    }

    pub fn fadd_st0_sti(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(0, a + b)
    }

    pub fn fadd_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(i)?;
        let b = self.read_st(0)?;
        self.write_st(i, a + b)
    }

    pub fn fsub_st0_sti(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(0, a - b)
    }

    pub fn fsubr_st0_sti(&mut self, i: usize) -> Result<()> {
        let st0 = self.read_st(0)?;
        let sti = self.read_st(i)?;
        self.write_st(0, sti - st0)
    }

    pub fn fsub_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(i)?;
        let b = self.read_st(0)?;
        self.write_st(i, a - b)
    }

    pub fn fsubr_sti_st0(&mut self, i: usize) -> Result<()> {
        let sti = self.read_st(i)?;
        let st0 = self.read_st(0)?;
        self.write_st(i, st0 - sti)
    }

    pub fn fmul_st0_sti(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(0, a * b)
    }

    pub fn fmul_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(i)?;
        let b = self.read_st(0)?;
        self.write_st(i, a * b)
    }

    pub fn fdiv_st0_sti(&mut self, i: usize) -> Result<()> {
        let b = self.read_st(i)?;
        if b == 0.0 {
            self.signal_zero_divide()?;
        }
        let a = self.read_st(0)?;
        self.write_st(0, a / b)
    }

    pub fn fdivr_st0_sti(&mut self, i: usize) -> Result<()> {
        let denom = self.read_st(0)?;
        if denom == 0.0 {
            self.signal_zero_divide()?;
        }
        let num = self.read_st(i)?;
        self.write_st(0, num / denom)
    }

    pub fn fdiv_sti_st0(&mut self, i: usize) -> Result<()> {
        let divisor = self.read_st(0)?;
        if divisor == 0.0 {
            self.signal_zero_divide()?;
        }
        let dividend = self.read_st(i)?;
        self.write_st(i, dividend / divisor)
    }

    pub fn fdivr_sti_st0(&mut self, i: usize) -> Result<()> {
        let denom = self.read_st(i)?;
        if denom == 0.0 {
            self.signal_zero_divide()?;
        }
        let num = self.read_st(0)?;
        self.write_st(i, num / denom)
    }

    pub fn faddp_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(i, a + b)?;
        self.pop()
    }

    pub fn fsubp_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(i, b - a)?;
        self.pop()
    }

    pub fn fsubrp_sti_st0(&mut self, i: usize) -> Result<()> {
        let st0 = self.read_st(0)?;
        let sti = self.read_st(i)?;
        self.write_st(i, st0 - sti)?;
        self.pop()
    }

    pub fn fmulp_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.write_st(i, a * b)?;
        self.pop()
    }

    pub fn fdivp_sti_st0(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        if a == 0.0 {
            self.signal_zero_divide()?;
        }
        let b = self.read_st(i)?;
        self.write_st(i, b / a)?;
        self.pop()
    }

    pub fn fdivrp_sti_st0(&mut self, i: usize) -> Result<()> {
        let denom = self.read_st(i)?;
        if denom == 0.0 {
            self.signal_zero_divide()?;
        }
        let num = self.read_st(0)?;
        self.write_st(i, num / denom)?;
        self.pop()
    }

    pub fn fchs(&mut self) -> Result<()> {
        let v = self.read_st(0)?;
        self.write_st(0, -v)
    }

    pub fn fabs(&mut self) -> Result<()> {
        let v = self.read_st(0)?;
        self.write_st(0, v.abs())
    }

    pub fn fcom_sti(&mut self, i: usize) -> Result<()> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        self.set_condition_codes_from_cmp(a, b)?;
        Ok(())
    }

    pub fn fcom_m32(&mut self, v: f32) -> Result<()> {
        self.fcom_m64(v as f64)
    }

    pub fn fcom_m64(&mut self, v: f64) -> Result<()> {
        let a = self.read_st(0)?;
        self.set_condition_codes_from_cmp(a, v)?;
        Ok(())
    }

    pub fn fcomp_sti(&mut self, i: usize) -> Result<()> {
        self.fcom_sti(i)?;
        self.pop()
    }

    pub fn fcomp_m32(&mut self, v: f32) -> Result<()> {
        self.fcomp_m64(v as f64)
    }

    pub fn fcomp_m64(&mut self, v: f64) -> Result<()> {
        self.fcom_m64(v)?;
        self.pop()
    }

    pub fn fcompp(&mut self) -> Result<()> {
        self.fcom_sti(1)?;
        self.pop()?;
        self.pop()
    }

    pub fn fucomi_sti(&mut self, i: usize) -> Result<Eflags> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        if a.is_nan() || b.is_nan() {
            self.signal_invalid(true)?;
        }
        Ok(eflags_from_cmp(a, b))
    }

    pub fn fucomip_sti(&mut self, i: usize) -> Result<Eflags> {
        let flags = self.fucomi_sti(i)?;
        self.pop()?;
        Ok(flags)
    }

    pub fn fcomi_sti(&mut self, i: usize) -> Result<Eflags> {
        let a = self.read_st(0)?;
        let b = self.read_st(i)?;
        if a.is_nan() || b.is_nan() {
            self.signal_invalid(true)?;
        }
        Ok(eflags_from_cmp(a, b))
    }

    pub fn fcomip_sti(&mut self, i: usize) -> Result<Eflags> {
        let flags = self.fcomi_sti(i)?;
        self.pop()?;
        Ok(flags)
    }

    fn phys_index(&self, st: usize) -> Option<usize> {
        if st < 8 {
            Some((self.top as usize + st) & 7)
        } else {
            None
        }
    }

    fn read_st(&mut self, st: usize) -> Result<f64> {
        let phys = self.phys_index(st).ok_or(Fault::MathFault)?;
        if matches!(self.tags[phys], Tag::Empty) {
            self.stack_underflow_value()
        } else {
            Ok(self.regs[phys])
        }
    }

    fn write_st(&mut self, st: usize, v: f64) -> Result<()> {
        let phys = self.phys_index(st).ok_or(Fault::MathFault)?;
        self.regs[phys] = v;
        self.tags[phys] = Tag::from_f64(v);
        Ok(())
    }

    fn push(&mut self, v: f64) -> Result<()> {
        let new_top = (self.top + 7) & 7;
        let phys = new_top as usize;
        if !matches!(self.tags[phys], Tag::Empty) {
            self.stack_overflow()?;
            self.top = new_top;
            self.sync_top();
            self.regs[phys] = f64::NAN;
            self.tags[phys] = Tag::Special;
            return Ok(());
        }
        self.top = new_top;
        self.sync_top();
        self.regs[phys] = v;
        self.tags[phys] = Tag::from_f64(v);
        Ok(())
    }

    fn pop(&mut self) -> Result<()> {
        let phys = self.top as usize;
        if matches!(self.tags[phys], Tag::Empty) {
            self.stack_underflow()
        } else {
            self.tags[phys] = Tag::Empty;
            self.regs[phys] = 0.0;
            self.top = (self.top + 1) & 7;
            self.sync_top();
            Ok(())
        }
    }

    fn sync_top(&mut self) {
        self.fsw = (self.fsw & !FSW_TOP_MASK) | ((self.top as u16) << 11);
    }

    fn sync_es(&mut self) {
        let flags = self.fsw & FCW_EXCEPTION_MASK;
        let masks = self.fcw & FCW_EXCEPTION_MASK;
        if flags & !masks != 0 {
            self.fsw |= FSW_ES;
        } else {
            self.fsw &= !FSW_ES;
        }
    }

    fn set_condition_codes_from_cmp(&mut self, a: f64, b: f64) -> Result<()> {
        self.fsw &= !(FSW_C0 | FSW_C1 | FSW_C2 | FSW_C3);
        if a.is_nan() || b.is_nan() {
            self.fsw |= FSW_C0 | FSW_C2 | FSW_C3;
            self.signal_invalid(true)?;
            return Ok(());
        }

        if a > b {
            // all condition bits already cleared
        } else if a < b {
            self.fsw |= FSW_C0;
        } else {
            self.fsw |= FSW_C3;
        }
        Ok(())
    }

    fn stack_overflow(&mut self) -> Result<()> {
        self.fsw |= FSW_C1;
        self.signal_invalid(false)?;
        self.fsw |= FSW_SF;
        Ok(())
    }

    fn stack_underflow(&mut self) -> Result<()> {
        self.fsw &= !FSW_C1;
        self.fsw |= FSW_SF;
        self.signal_invalid(false)
    }

    fn stack_underflow_value(&mut self) -> Result<f64> {
        self.stack_underflow()?;
        Ok(f64::NAN)
    }

    fn signal_invalid(&mut self, quiet_compare: bool) -> Result<()> {
        if quiet_compare {
            // For a minimal model we treat unordered compares as invalid, matching
            // the common "bad input" path; hardware is more nuanced (QNaN vs SNaN).
        }
        self.signal_exception(FSW_IE)
    }

    fn signal_zero_divide(&mut self) -> Result<()> {
        self.signal_exception(FSW_ZE)
    }

    fn signal_exception(&mut self, flag: u16) -> Result<()> {
        self.fsw |= flag;
        self.sync_es();

        let masks = self.fcw & FCW_EXCEPTION_MASK;
        if flag & !masks != 0 {
            Err(Fault::MathFault)
        } else {
            Ok(())
        }
    }
}

fn eflags_from_cmp(a: f64, b: f64) -> Eflags {
    if a.is_nan() || b.is_nan() {
        return Eflags {
            cf: true,
            pf: true,
            zf: true,
        };
    }
    if a > b {
        Eflags::default()
    } else if a < b {
        Eflags {
            cf: true,
            pf: false,
            zf: false,
        }
    } else {
        Eflags {
            cf: false,
            pf: false,
            zf: true,
        }
    }
}

fn ext80_from_f64(v: f64) -> u128 {
    let bits = v.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7FF) as u16;
    let frac = bits & ((1u64 << 52) - 1);

    let mut sign_exp: u16 = sign << 15;
    let mant: u64 = match exp {
        0x7FF => {
            // Inf / NaN.
            sign_exp |= 0x7FFF;
            if frac == 0 {
                1u64 << 63
            } else {
                // Quiet NaN payload (minimal).
                (1u64 << 63) | (1u64 << 62)
            }
        }
        0 => {
            if frac == 0 {
                // Zero.
                0
            } else {
                // Subnormal `f64` values are representable as normal 80-bit values.
                let k = 63 - frac.leading_zeros(); // 0..=51
                let exp_unbiased = (k as i32) - 1074;
                let exp_ext = (exp_unbiased + 16383) as u16;
                sign_exp |= exp_ext;
                frac << (63 - k)
            }
        }
        _ => {
            // Normal.
            let exp_unbiased = (exp as i32) - 1023;
            let exp_ext = (exp_unbiased + 16383) as u16;
            sign_exp |= exp_ext;
            (1u64 << 63) | (frac << 11)
        }
    };

    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&mant.to_le_bytes());
    out[8..10].copy_from_slice(&sign_exp.to_le_bytes());
    u128::from_le_bytes(out)
}

fn f64_from_ext80(v: u128) -> f64 {
    let bytes = v.to_le_bytes();
    let mant = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let sign_exp = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let sign = (sign_exp >> 15) & 1;
    let exp = sign_exp & 0x7FFF;

    if exp == 0 && mant == 0 {
        return if sign == 1 { -0.0 } else { 0.0 };
    }

    if exp == 0x7FFF {
        let int_bit = mant >> 63;
        let frac = mant & ((1u64 << 63) - 1);
        if int_bit == 1 && frac == 0 {
            return if sign == 1 {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }

        let nan = f64::NAN;
        return if sign == 1 { -nan } else { nan };
    }

    // Treat subnormal 80-bit values as a scaled fixed-point number.
    let m = (mant as f64) / ((1u64 << 63) as f64);
    let exp_unbiased = if exp == 0 {
        1i32 - 16383
    } else {
        (exp as i32) - 16383
    };

    let mut out = m * 2f64.powi(exp_unbiased);
    if sign == 1 {
        out = -out;
    }
    out
}
