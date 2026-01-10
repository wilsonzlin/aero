use super::bus::{MemoryBus, PortIo};
use super::cpu::{CpuMode, CpuState};
use super::error::EmuException;
use super::flags::{parity_even, Flag, LazyFlags, LazyOp, FLAG_IF, FLAG_TF};
use crate::msr::{IA32_TSC, IA32_TSC_AUX};
use iced_x86::{Code, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Continue,
    Halted,
}

pub struct Machine<M: MemoryBus, P: PortIo> {
    pub cpu: CpuState,
    pub mem: M,
    pub ports: P,
    addr_size_override: bool,
}

impl<M: MemoryBus, P: PortIo> Machine<M, P> {
    pub fn new(cpu: CpuState, mem: M, ports: P) -> Self {
        Self {
            cpu,
            mem,
            ports,
            addr_size_override: false,
        }
    }

    pub fn step(&mut self) -> Result<StepOutcome, EmuException> {
        if self.cpu.halted {
            return Ok(StepOutcome::Halted);
        }

        let bitness = match self.cpu.mode {
            CpuMode::Real => 16,
            CpuMode::Protected => 32,
            CpuMode::Long => 64,
        };

        let ip = self.cpu.rip & self.cpu.ip_mask();
        let cs_base = self.cpu.cs().base;
        let fetch_paddr = self.cpu.apply_a20(cs_base.wrapping_add(ip));

        let mut bytes = [0u8; 15];
        self.mem.read_bytes(fetch_paddr, &mut bytes)?;

        self.addr_size_override = has_addr_size_override(&bytes, self.cpu.mode);

        let mut decoder = Decoder::with_ip(bitness, &bytes, ip, DecoderOptions::NONE);
        let instr = decoder.decode();
        if instr.code() == Code::INVALID {
            return Err(EmuException::InvalidOpcode);
        }

        let len = instr.len() as u64;
        let next_ip = ip.wrapping_add(len) & self.cpu.ip_mask();

        let branched = self.exec(&instr, next_ip)?;
        if !branched && !self.cpu.halted {
            self.cpu.set_rip(next_ip);
        }

        self.cpu.tsc = self.cpu.tsc.wrapping_add(1);

        Ok(if self.cpu.halted {
            StepOutcome::Halted
        } else {
            StepOutcome::Continue
        })
    }

    pub fn run(&mut self, max_instructions: usize) -> Result<(), EmuException> {
        for _ in 0..max_instructions {
            match self.step()? {
                StepOutcome::Continue => {}
                StepOutcome::Halted => return Ok(()),
            }
        }
        Err(EmuException::Halted)
    }

    fn exec(&mut self, instr: &Instruction, next_ip: u64) -> Result<bool, EmuException> {
        match instr.mnemonic() {
            Mnemonic::Mov => {
                self.exec_mov(instr)?;
                Ok(false)
            }
            Mnemonic::Lea => {
                self.exec_lea(instr, next_ip)?;
                Ok(false)
            }
            Mnemonic::Xchg => {
                self.exec_xchg(instr)?;
                Ok(false)
            }
            Mnemonic::Movzx => {
                self.exec_movzx(instr, false)?;
                Ok(false)
            }
            Mnemonic::Movsx => {
                self.exec_movzx(instr, true)?;
                Ok(false)
            }
            Mnemonic::Cmovo
            | Mnemonic::Cmovno
            | Mnemonic::Cmovb
            | Mnemonic::Cmovae
            | Mnemonic::Cmove
            | Mnemonic::Cmovne
            | Mnemonic::Cmovbe
            | Mnemonic::Cmova
            | Mnemonic::Cmovs
            | Mnemonic::Cmovns
            | Mnemonic::Cmovp
            | Mnemonic::Cmovnp
            | Mnemonic::Cmovl
            | Mnemonic::Cmovge
            | Mnemonic::Cmovle
            | Mnemonic::Cmovg => {
                self.exec_cmovcc(instr)?;
                Ok(false)
            }
            Mnemonic::Seto
            | Mnemonic::Setno
            | Mnemonic::Setb
            | Mnemonic::Setae
            | Mnemonic::Sete
            | Mnemonic::Setne
            | Mnemonic::Setbe
            | Mnemonic::Seta
            | Mnemonic::Sets
            | Mnemonic::Setns
            | Mnemonic::Setp
            | Mnemonic::Setnp
            | Mnemonic::Setl
            | Mnemonic::Setge
            | Mnemonic::Setle
            | Mnemonic::Setg => {
                self.exec_setcc(instr)?;
                Ok(false)
            }

            Mnemonic::Push => {
                self.exec_push(instr)?;
                Ok(false)
            }
            Mnemonic::Pop => {
                self.exec_pop(instr)?;
                Ok(false)
            }
            Mnemonic::Pusha | Mnemonic::Pushad => {
                self.exec_pusha(instr)?;
                Ok(false)
            }
            Mnemonic::Popa | Mnemonic::Popad => {
                self.exec_popa(instr)?;
                Ok(false)
            }
            Mnemonic::Pushf | Mnemonic::Pushfd | Mnemonic::Pushfq => {
                self.exec_pushf(instr)?;
                Ok(false)
            }
            Mnemonic::Popf | Mnemonic::Popfd | Mnemonic::Popfq => {
                self.exec_popf(instr)?;
                Ok(false)
            }

            Mnemonic::Add => {
                self.exec_alu_bin(instr, AluBinOp::Add)?;
                Ok(false)
            }
            Mnemonic::Adc => {
                self.exec_alu_bin(instr, AluBinOp::Adc)?;
                Ok(false)
            }
            Mnemonic::Sub => {
                self.exec_alu_bin(instr, AluBinOp::Sub)?;
                Ok(false)
            }
            Mnemonic::Sbb => {
                self.exec_alu_bin(instr, AluBinOp::Sbb)?;
                Ok(false)
            }
            Mnemonic::Cmp => {
                self.exec_alu_bin(instr, AluBinOp::Cmp)?;
                Ok(false)
            }
            Mnemonic::Inc => {
                self.exec_incdec(instr, true)?;
                Ok(false)
            }
            Mnemonic::Dec => {
                self.exec_incdec(instr, false)?;
                Ok(false)
            }
            Mnemonic::Mul => {
                self.exec_mul(instr)?;
                Ok(false)
            }
            Mnemonic::Imul => {
                self.exec_imul(instr)?;
                Ok(false)
            }
            Mnemonic::Div => {
                self.exec_div(instr, false)?;
                Ok(false)
            }
            Mnemonic::Idiv => {
                self.exec_div(instr, true)?;
                Ok(false)
            }

            Mnemonic::And => {
                self.exec_alu_bin(instr, AluBinOp::And)?;
                Ok(false)
            }
            Mnemonic::Or => {
                self.exec_alu_bin(instr, AluBinOp::Or)?;
                Ok(false)
            }
            Mnemonic::Xor => {
                self.exec_alu_bin(instr, AluBinOp::Xor)?;
                Ok(false)
            }
            Mnemonic::Test => {
                self.exec_alu_bin(instr, AluBinOp::Test)?;
                Ok(false)
            }
            Mnemonic::Not => {
                self.exec_not(instr)?;
                Ok(false)
            }
            Mnemonic::Neg => {
                self.exec_neg(instr)?;
                Ok(false)
            }

            Mnemonic::Shl | Mnemonic::Sal => {
                self.exec_shift(instr, ShiftKind::Shl)?;
                Ok(false)
            }
            Mnemonic::Shr => {
                self.exec_shift(instr, ShiftKind::Shr)?;
                Ok(false)
            }
            Mnemonic::Sar => {
                self.exec_shift(instr, ShiftKind::Sar)?;
                Ok(false)
            }
            Mnemonic::Shld => {
                self.exec_shld_shrd(instr, true)?;
                Ok(false)
            }
            Mnemonic::Shrd => {
                self.exec_shld_shrd(instr, false)?;
                Ok(false)
            }
            Mnemonic::Rol => {
                self.exec_rotate(instr, RotateKind::Rol)?;
                Ok(false)
            }
            Mnemonic::Ror => {
                self.exec_rotate(instr, RotateKind::Ror)?;
                Ok(false)
            }
            Mnemonic::Rcl => {
                self.exec_rotate(instr, RotateKind::Rcl)?;
                Ok(false)
            }
            Mnemonic::Rcr => {
                self.exec_rotate(instr, RotateKind::Rcr)?;
                Ok(false)
            }
            Mnemonic::Bswap => {
                self.exec_bswap(instr)?;
                Ok(false)
            }
            Mnemonic::Xadd => {
                self.exec_xadd(instr)?;
                Ok(false)
            }
            Mnemonic::Bt => {
                self.exec_bit_op(instr, BitOpKind::Bt)?;
                Ok(false)
            }
            Mnemonic::Bts => {
                self.exec_bit_op(instr, BitOpKind::Bts)?;
                Ok(false)
            }
            Mnemonic::Btr => {
                self.exec_bit_op(instr, BitOpKind::Btr)?;
                Ok(false)
            }
            Mnemonic::Btc => {
                self.exec_bit_op(instr, BitOpKind::Btc)?;
                Ok(false)
            }
            Mnemonic::Bsf => {
                self.exec_bscan(instr, false)?;
                Ok(false)
            }
            Mnemonic::Bsr => {
                self.exec_bscan(instr, true)?;
                Ok(false)
            }

            Mnemonic::Jmp => self.exec_jmp(instr),
            Mnemonic::Call => self.exec_call(instr, next_ip),
            Mnemonic::Ret => self.exec_ret(instr),
            Mnemonic::Retf => self.exec_retf(instr),

            Mnemonic::Jo
            | Mnemonic::Jno
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jp
            | Mnemonic::Jnp
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg => self.exec_jcc(instr),

            Mnemonic::Loop => self.exec_loop(instr, LoopKind::Loop),
            Mnemonic::Loope => self.exec_loop(instr, LoopKind::Loope),
            Mnemonic::Loopne => self.exec_loop(instr, LoopKind::Loopne),
            Mnemonic::Jcxz | Mnemonic::Jecxz | Mnemonic::Jrcxz => self.exec_jcxz(instr),

            Mnemonic::Movsb | Mnemonic::Movsw | Mnemonic::Movsd | Mnemonic::Movsq => {
                self.exec_movs(instr)?;
                Ok(false)
            }
            Mnemonic::Stosb | Mnemonic::Stosw | Mnemonic::Stosd | Mnemonic::Stosq => {
                self.exec_stos(instr)?;
                Ok(false)
            }
            Mnemonic::Lodsb | Mnemonic::Lodsw | Mnemonic::Lodsd | Mnemonic::Lodsq => {
                self.exec_lods(instr)?;
                Ok(false)
            }
            Mnemonic::Cmpsb | Mnemonic::Cmpsw | Mnemonic::Cmpsd | Mnemonic::Cmpsq => {
                self.exec_cmps(instr)?;
                Ok(false)
            }
            Mnemonic::Scasb | Mnemonic::Scasw | Mnemonic::Scasd | Mnemonic::Scasq => {
                self.exec_scas(instr)?;
                Ok(false)
            }

            Mnemonic::Int => self.exec_int(instr, next_ip),
            Mnemonic::Int3 => self.exec_int3(next_ip),
            Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => self.exec_iret(instr),

            Mnemonic::Cli => {
                self.cpu.set_flag(Flag::If, false);
                Ok(false)
            }
            Mnemonic::Sti => {
                self.cpu.set_flag(Flag::If, true);
                Ok(false)
            }
            Mnemonic::Hlt => {
                self.cpu.halted = true;
                Ok(true)
            }

            Mnemonic::Cld => {
                self.cpu.set_flag(Flag::Df, false);
                Ok(false)
            }
            Mnemonic::Std => {
                self.cpu.set_flag(Flag::Df, true);
                Ok(false)
            }
            Mnemonic::Clc => {
                self.cpu.materialize_lazy_flags();
                self.cpu.set_flag(Flag::Cf, false);
                Ok(false)
            }
            Mnemonic::Stc => {
                self.cpu.materialize_lazy_flags();
                self.cpu.set_flag(Flag::Cf, true);
                Ok(false)
            }
            Mnemonic::Cmc => {
                let cf = self.cpu.get_flag(Flag::Cf);
                self.cpu.materialize_lazy_flags();
                self.cpu.set_flag(Flag::Cf, !cf);
                Ok(false)
            }

            Mnemonic::Cpuid => {
                self.exec_cpuid()?;
                Ok(false)
            }
            Mnemonic::Rdtsc => {
                self.exec_rdtsc()?;
                Ok(false)
            }
            Mnemonic::Rdtscp => {
                self.exec_rdtscp()?;
                Ok(false)
            }
            Mnemonic::Lfence | Mnemonic::Sfence | Mnemonic::Mfence => Ok(false),
            Mnemonic::Pause => {
                self.exec_pause();
                Ok(false)
            }
            Mnemonic::Rdmsr => {
                self.exec_rdmsr()?;
                Ok(false)
            }
            Mnemonic::Wrmsr => {
                self.exec_wrmsr()?;
                Ok(false)
            }
            Mnemonic::In => {
                self.exec_in(instr)?;
                Ok(false)
            }
            Mnemonic::Out => {
                self.exec_out(instr)?;
                Ok(false)
            }
            Mnemonic::Lgdt => {
                self.exec_lgdt(instr)?;
                Ok(false)
            }
            Mnemonic::Lidt => {
                self.exec_lidt(instr)?;
                Ok(false)
            }
            Mnemonic::Ltr => {
                self.exec_ltr(instr)?;
                Ok(false)
            }
            Mnemonic::Cbw | Mnemonic::Cwde | Mnemonic::Cdqe => {
                self.exec_cbw_family(instr)?;
                Ok(false)
            }
            Mnemonic::Cwd | Mnemonic::Cdq | Mnemonic::Cqo => {
                self.exec_cwd_family(instr)?;
                Ok(false)
            }

            Mnemonic::Nop => Ok(false),

            _ => Err(EmuException::Unimplemented(instr.code())),
        }
    }

    fn exec_mov(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let dst_kind = op_kind(instr, 0);
        let src = self.read_operand(instr, 1)?;

        if matches!(dst_kind, OpKind::Register) {
            let dst_reg = op_register(instr, 0);
            if is_segment_reg(dst_reg) && self.cpu.mode != CpuMode::Real {
                let selector = src as u16;
                self.cpu
                    .load_segment_from_gdt(dst_reg, selector, |addr| self.mem.read_u64(addr))?;
                return Ok(());
            }
        }

        self.write_operand(instr, 0, src)?;
        Ok(())
    }

    fn exec_lea(&mut self, instr: &Instruction, next_ip: u64) -> Result<(), EmuException> {
        if instr.op_count() != 2 || op_kind(instr, 1) != OpKind::Memory {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let dst = op_register(instr, 0);
        let offset = self.calc_effective_offset(instr, next_ip)?;
        self.cpu.write_reg(dst, offset)?;
        Ok(())
    }

    fn exec_xchg(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let a = self.read_operand(instr, 0)?;
        let b = self.read_operand(instr, 1)?;
        self.write_operand(instr, 0, b)?;
        self.write_operand(instr, 1, a)?;
        Ok(())
    }

    fn exec_movzx(&mut self, instr: &Instruction, sign_extend: bool) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let dst_reg = op_register(instr, 0);
        let dst_bits = reg_bits(dst_reg)?;

        let (src_val, src_bits) = self.read_operand_with_size(instr, 1)?;
        let src_masked = src_val & mask_for_bits(src_bits);
        let val = if sign_extend {
            sign_extend_value(src_masked, src_bits)?
        } else {
            src_masked
        };
        let masked = val & mask_for_bits(dst_bits);
        self.cpu.write_reg(dst_reg, masked)?;
        Ok(())
    }

    fn exec_cmovcc(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }

        let cond = match instr.mnemonic() {
            Mnemonic::Cmovo => self.cpu.get_flag(Flag::Of),
            Mnemonic::Cmovno => !self.cpu.get_flag(Flag::Of),
            Mnemonic::Cmovb => self.cpu.get_flag(Flag::Cf),
            Mnemonic::Cmovae => !self.cpu.get_flag(Flag::Cf),
            Mnemonic::Cmove => self.cpu.get_flag(Flag::Zf),
            Mnemonic::Cmovne => !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Cmovbe => self.cpu.get_flag(Flag::Cf) || self.cpu.get_flag(Flag::Zf),
            Mnemonic::Cmova => !self.cpu.get_flag(Flag::Cf) && !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Cmovs => self.cpu.get_flag(Flag::Sf),
            Mnemonic::Cmovns => !self.cpu.get_flag(Flag::Sf),
            Mnemonic::Cmovp => self.cpu.get_flag(Flag::Pf),
            Mnemonic::Cmovnp => !self.cpu.get_flag(Flag::Pf),
            Mnemonic::Cmovl => self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of),
            Mnemonic::Cmovge => self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of),
            Mnemonic::Cmovle => {
                self.cpu.get_flag(Flag::Zf)
                    || (self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of))
            }
            Mnemonic::Cmovg => {
                !self.cpu.get_flag(Flag::Zf)
                    && (self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of))
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };

        if cond {
            let src = self.read_operand(instr, 1)?;
            self.write_operand(instr, 0, src)?;
        }
        Ok(())
    }

    fn exec_setcc(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let cond = match instr.mnemonic() {
            Mnemonic::Seto => self.cpu.get_flag(Flag::Of),
            Mnemonic::Setno => !self.cpu.get_flag(Flag::Of),
            Mnemonic::Setb => self.cpu.get_flag(Flag::Cf),
            Mnemonic::Setae => !self.cpu.get_flag(Flag::Cf),
            Mnemonic::Sete => self.cpu.get_flag(Flag::Zf),
            Mnemonic::Setne => !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Setbe => self.cpu.get_flag(Flag::Cf) || self.cpu.get_flag(Flag::Zf),
            Mnemonic::Seta => !self.cpu.get_flag(Flag::Cf) && !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Sets => self.cpu.get_flag(Flag::Sf),
            Mnemonic::Setns => !self.cpu.get_flag(Flag::Sf),
            Mnemonic::Setp => self.cpu.get_flag(Flag::Pf),
            Mnemonic::Setnp => !self.cpu.get_flag(Flag::Pf),
            Mnemonic::Setl => self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of),
            Mnemonic::Setge => self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of),
            Mnemonic::Setle => {
                self.cpu.get_flag(Flag::Zf)
                    || (self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of))
            }
            Mnemonic::Setg => {
                !self.cpu.get_flag(Flag::Zf)
                    && (self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of))
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        self.write_operand(instr, 0, if cond { 1 } else { 0 })?;
        Ok(())
    }

    fn exec_push(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        self.push(val, bits)?;
        Ok(())
    }

    fn exec_pop(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let bits = operand_bits(instr, 0)?;
        let val = self.pop(bits)?;
        self.write_operand(instr, 0, val)?;
        Ok(())
    }

    fn exec_pusha(&mut self, _instr: &Instruction) -> Result<(), EmuException> {
        match self.cpu.mode {
            CpuMode::Real => {
                let ax = self.cpu.read_reg(Register::AX)?;
                let cx = self.cpu.read_reg(Register::CX)?;
                let dx = self.cpu.read_reg(Register::DX)?;
                let bx = self.cpu.read_reg(Register::BX)?;
                let sp = self.cpu.read_reg(Register::SP)?;
                let bp = self.cpu.read_reg(Register::BP)?;
                let si = self.cpu.read_reg(Register::SI)?;
                let di = self.cpu.read_reg(Register::DI)?;

                self.push(ax, 16)?;
                self.push(cx, 16)?;
                self.push(dx, 16)?;
                self.push(bx, 16)?;
                self.push(sp, 16)?;
                self.push(bp, 16)?;
                self.push(si, 16)?;
                self.push(di, 16)?;
            }
            CpuMode::Protected => {
                let eax = self.cpu.read_reg(Register::EAX)?;
                let ecx = self.cpu.read_reg(Register::ECX)?;
                let edx = self.cpu.read_reg(Register::EDX)?;
                let ebx = self.cpu.read_reg(Register::EBX)?;
                let esp = self.cpu.read_reg(Register::ESP)?;
                let ebp = self.cpu.read_reg(Register::EBP)?;
                let esi = self.cpu.read_reg(Register::ESI)?;
                let edi = self.cpu.read_reg(Register::EDI)?;

                self.push(eax, 32)?;
                self.push(ecx, 32)?;
                self.push(edx, 32)?;
                self.push(ebx, 32)?;
                self.push(esp, 32)?;
                self.push(ebp, 32)?;
                self.push(esi, 32)?;
                self.push(edi, 32)?;
            }
            CpuMode::Long => return Err(EmuException::InvalidOpcode),
        }
        Ok(())
    }

    fn exec_popa(&mut self, _instr: &Instruction) -> Result<(), EmuException> {
        match self.cpu.mode {
            CpuMode::Real => {
                let di = self.pop(16)?;
                let si = self.pop(16)?;
                let bp = self.pop(16)?;
                let _sp = self.pop(16)?; // discarded
                let bx = self.pop(16)?;
                let dx = self.pop(16)?;
                let cx = self.pop(16)?;
                let ax = self.pop(16)?;
                self.cpu.write_reg(Register::DI, di)?;
                self.cpu.write_reg(Register::SI, si)?;
                self.cpu.write_reg(Register::BP, bp)?;
                self.cpu.write_reg(Register::BX, bx)?;
                self.cpu.write_reg(Register::DX, dx)?;
                self.cpu.write_reg(Register::CX, cx)?;
                self.cpu.write_reg(Register::AX, ax)?;
            }
            CpuMode::Protected => {
                let edi = self.pop(32)?;
                let esi = self.pop(32)?;
                let ebp = self.pop(32)?;
                let _esp = self.pop(32)?;
                let ebx = self.pop(32)?;
                let edx = self.pop(32)?;
                let ecx = self.pop(32)?;
                let eax = self.pop(32)?;
                self.cpu.write_reg(Register::EDI, edi)?;
                self.cpu.write_reg(Register::ESI, esi)?;
                self.cpu.write_reg(Register::EBP, ebp)?;
                self.cpu.write_reg(Register::EBX, ebx)?;
                self.cpu.write_reg(Register::EDX, edx)?;
                self.cpu.write_reg(Register::ECX, ecx)?;
                self.cpu.write_reg(Register::EAX, eax)?;
            }
            CpuMode::Long => return Err(EmuException::InvalidOpcode),
        }
        Ok(())
    }

    fn exec_pushf(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 0 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let bits = match instr.mnemonic() {
            Mnemonic::Pushf => 16,
            Mnemonic::Pushfd => 32,
            Mnemonic::Pushfq => 64,
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        let rf = self.cpu.rflags() & mask_for_bits(bits);
        self.push(rf, bits)?;
        Ok(())
    }

    fn exec_popf(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 0 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let bits = match instr.mnemonic() {
            Mnemonic::Popf => 16,
            Mnemonic::Popfd => 32,
            Mnemonic::Popfq => 64,
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        let val = self.pop(bits)?;
        if bits == 64 {
            self.cpu.set_rflags(val);
        } else {
            let mask = mask_for_bits(bits);
            let rf = (self.cpu.rflags() & !mask) | (val & mask);
            self.cpu.set_rflags(rf);
        }
        Ok(())
    }

    fn exec_incdec(&mut self, instr: &Instruction, inc: bool) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }

        // INC/DEC do not update CF, so we must preserve it (potentially through lazy flags).
        let old_cf = self.cpu.get_flag(Flag::Cf);
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        let mask = mask_for_bits(bits);
        let masked = val & mask;
        let result = if inc {
            masked.wrapping_add(1) & mask
        } else {
            masked.wrapping_sub(1) & mask
        };

        self.cpu.materialize_lazy_flags();
        self.write_operand(instr, 0, result)?;

        // Update flags (except CF).
        self.cpu.set_flag(Flag::Zf, (result & mask) == 0);
        self.cpu.set_flag(Flag::Sf, (result & sign_bit(bits)) != 0);
        self.cpu.set_flag(Flag::Pf, parity_even(result as u8));
        if inc {
            self.cpu.set_flag(Flag::Af, ((masked & 0xF) + 1) > 0xF);
            self.cpu.set_flag(Flag::Of, masked == sign_bit(bits) - 1);
        } else {
            self.cpu.set_flag(Flag::Af, (masked & 0xF) == 0);
            self.cpu.set_flag(Flag::Of, masked == sign_bit(bits));
        }
        self.cpu.set_flag(Flag::Cf, old_cf);
        Ok(())
    }

    fn exec_not(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        let mask = mask_for_bits(bits);
        self.write_operand(instr, 0, (!val) & mask)?;
        Ok(())
    }

    fn exec_neg(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        let mask = mask_for_bits(bits);
        let masked = val & mask;
        let result = (0u64.wrapping_sub(masked)) & mask;

        self.cpu.materialize_lazy_flags();
        self.write_operand(instr, 0, result)?;

        self.cpu.set_flag(Flag::Cf, masked != 0);
        self.cpu.set_flag(Flag::Zf, result == 0);
        self.cpu.set_flag(Flag::Sf, (result & sign_bit(bits)) != 0);
        self.cpu.set_flag(Flag::Pf, parity_even(result as u8));
        self.cpu.set_flag(Flag::Af, (masked & 0xF) != 0);
        self.cpu.set_flag(Flag::Of, masked == sign_bit(bits));
        Ok(())
    }

    fn exec_alu_bin(&mut self, instr: &Instruction, op: AluBinOp) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }

        let dst_val = self.read_operand(instr, 0)?;
        let (src_val, _src_bits) = match op {
            AluBinOp::Test => self.read_operand_with_size(instr, 1),
            _ => self.read_operand_with_size(instr, 1),
        }?;
        let bits = operand_bits(instr, 0)?;

        let mask = mask_for_bits(bits);
        let a = dst_val & mask;
        let b = src_val & mask;

        match op {
            AluBinOp::Add | AluBinOp::Adc | AluBinOp::Sub | AluBinOp::Sbb | AluBinOp::Cmp => {
                let carry_in = match op {
                    AluBinOp::Adc => self.cpu.get_flag(Flag::Cf) as u8,
                    AluBinOp::Sbb => self.cpu.get_flag(Flag::Cf) as u8,
                    _ => 0,
                };
                let (result, lf) = match op {
                    AluBinOp::Add => {
                        let res = a.wrapping_add(b) & mask;
                        (
                            res,
                            LazyFlags {
                                op: LazyOp::Add { carry_in: 0 },
                                size_bits: bits,
                                lhs: a,
                                rhs: b,
                                result: res,
                            },
                        )
                    }
                    AluBinOp::Adc => {
                        let res = a.wrapping_add(b).wrapping_add(carry_in as u64) & mask;
                        (
                            res,
                            LazyFlags {
                                op: LazyOp::Add { carry_in },
                                size_bits: bits,
                                lhs: a,
                                rhs: b,
                                result: res,
                            },
                        )
                    }
                    AluBinOp::Sub => {
                        let res = a.wrapping_sub(b) & mask;
                        (
                            res,
                            LazyFlags {
                                op: LazyOp::Sub { borrow_in: 0 },
                                size_bits: bits,
                                lhs: a,
                                rhs: b,
                                result: res,
                            },
                        )
                    }
                    AluBinOp::Sbb => {
                        let res = a.wrapping_sub(b).wrapping_sub(carry_in as u64) & mask;
                        (
                            res,
                            LazyFlags {
                                op: LazyOp::Sub {
                                    borrow_in: carry_in,
                                },
                                size_bits: bits,
                                lhs: a,
                                rhs: b,
                                result: res,
                            },
                        )
                    }
                    AluBinOp::Cmp => {
                        let res = a.wrapping_sub(b) & mask;
                        (
                            res,
                            LazyFlags {
                                op: LazyOp::Sub { borrow_in: 0 },
                                size_bits: bits,
                                lhs: a,
                                rhs: b,
                                result: res,
                            },
                        )
                    }
                    _ => unreachable!(),
                };

                self.cpu.set_lazy_flags(lf);
                if op != AluBinOp::Cmp {
                    self.write_operand(instr, 0, result)?;
                }
            }
            AluBinOp::And | AluBinOp::Or | AluBinOp::Xor | AluBinOp::Test => {
                let result = match op {
                    AluBinOp::And | AluBinOp::Test => a & b,
                    AluBinOp::Or => a | b,
                    AluBinOp::Xor => a ^ b,
                    _ => unreachable!(),
                } & mask;

                self.cpu.set_lazy_flags(LazyFlags {
                    op: LazyOp::Logic,
                    size_bits: bits,
                    lhs: a,
                    rhs: b,
                    result,
                });

                if op != AluBinOp::Test {
                    self.write_operand(instr, 0, result)?;
                }
            }
        }
        Ok(())
    }

    fn exec_mul(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (src, bits) = self.read_operand_with_size(instr, 0)?;
        let mask = mask_for_bits(bits);
        let src = src & mask;
        self.cpu.materialize_lazy_flags();

        match bits {
            8 => {
                let al = self.cpu.read_reg(Register::AL)? as u16;
                let res = al * (src as u16);
                self.cpu.write_reg(Register::AX, res as u64)?;
                self.set_mul_flags_unsigned(res as u128, 8);
            }
            16 => {
                let ax = self.cpu.read_reg(Register::AX)? as u32;
                let res = ax * (src as u32);
                self.cpu.write_reg(Register::AX, (res & 0xffff) as u64)?;
                self.cpu.write_reg(Register::DX, (res >> 16) as u64)?;
                self.set_mul_flags_unsigned(res as u128, 16);
            }
            32 => {
                let eax = self.cpu.read_reg(Register::EAX)? as u64;
                let res = eax * (src as u64);
                self.cpu
                    .write_reg(Register::EAX, (res & 0xffff_ffff) as u64)?;
                self.cpu.write_reg(Register::EDX, (res >> 32) as u64)?;
                self.set_mul_flags_unsigned(res as u128, 32);
            }
            64 => {
                let rax = self.cpu.read_reg(Register::RAX)? as u128;
                let res = rax * src as u128;
                self.cpu.write_reg(Register::RAX, res as u64)?;
                self.cpu.write_reg(Register::RDX, (res >> 64) as u64)?;
                self.set_mul_flags_unsigned(res, 64);
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        Ok(())
    }

    fn exec_imul(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        self.cpu.materialize_lazy_flags();
        match instr.op_count() {
            1 => {
                // One-operand IMUL: implicit accumulator.
                let (src, bits) = self.read_operand_with_size(instr, 0)?;
                let mask = mask_for_bits(bits);
                let src = src & mask;
                match bits {
                    8 => {
                        let al = self.cpu.read_reg(Register::AL)? as i8 as i16;
                        let rhs = src as i8 as i16;
                        let res = al.wrapping_mul(rhs) as i16;
                        self.cpu.write_reg(Register::AX, res as u16 as u64)?;
                        self.set_mul_flags_signed(res as i128, 8);
                    }
                    16 => {
                        let ax = self.cpu.read_reg(Register::AX)? as i16 as i32;
                        let rhs = src as i16 as i32;
                        let res = ax.wrapping_mul(rhs) as i32;
                        self.cpu
                            .write_reg(Register::AX, (res as u32 & 0xffff) as u64)?;
                        self.cpu
                            .write_reg(Register::DX, ((res as u32) >> 16) as u64)?;
                        self.set_mul_flags_signed(res as i128, 16);
                    }
                    32 => {
                        let eax = self.cpu.read_reg(Register::EAX)? as i32 as i64;
                        let rhs = src as i32 as i64;
                        let res = eax.wrapping_mul(rhs) as i64;
                        self.cpu.write_reg(Register::EAX, res as u32 as u64)?;
                        self.cpu
                            .write_reg(Register::EDX, ((res as u64) >> 32) as u64)?;
                        self.set_mul_flags_signed(res as i128, 32);
                    }
                    64 => {
                        let rax = self.cpu.read_reg(Register::RAX)? as i64 as i128;
                        let rhs = src as i64 as i128;
                        let res = rax.wrapping_mul(rhs);
                        self.cpu.write_reg(Register::RAX, res as u64)?;
                        self.cpu.write_reg(Register::RDX, (res >> 64) as u64)?;
                        self.set_mul_flags_signed(res, 64);
                    }
                    _ => return Err(EmuException::Unimplemented(instr.code())),
                }
                Ok(())
            }
            2 | 3 => {
                // Two/three operand IMUL: dst = src * imm?
                let dst_reg = op_register(instr, 0);
                let dst_bits = reg_bits(dst_reg)?;
                let (lhs, _) = self.read_operand_with_size(instr, 1)?;
                let rhs = if instr.op_count() == 3 {
                    self.read_operand(instr, 2)?
                } else {
                    self.read_operand(instr, 0)?
                };

                let lhs = sign_extend_value(lhs & mask_for_bits(dst_bits), dst_bits)? as i128;
                let rhs = sign_extend_value(rhs & mask_for_bits(dst_bits), dst_bits)? as i128;
                let full = lhs.wrapping_mul(rhs);
                let truncated = (full as i128) & (mask_for_bits(dst_bits) as i128);
                self.cpu.write_reg(dst_reg, truncated as u64)?;

                let sign_ext = sign_extend_value(truncated as u64, dst_bits)? as i128;
                let overflow = sign_ext != full;
                self.cpu.set_flag(Flag::Cf, overflow);
                self.cpu.set_flag(Flag::Of, overflow);
                Ok(())
            }
            _ => Err(EmuException::Unimplemented(instr.code())),
        }
    }

    fn exec_div(&mut self, instr: &Instruction, signed: bool) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (divisor_raw, bits) = self.read_operand_with_size(instr, 0)?;
        let mask = mask_for_bits(bits);
        let divisor = divisor_raw & mask;

        if divisor == 0 {
            return Err(EmuException::DivideError);
        }

        self.cpu.materialize_lazy_flags();

        match (bits, signed) {
            (8, false) => {
                let dividend = self.cpu.read_reg(Register::AX)? as u16 as u128;
                let q = dividend / divisor as u128;
                let r = dividend % divisor as u128;
                if q > 0xff {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::AL, q as u64)?;
                self.cpu.write_reg(Register::AH, r as u64)?;
            }
            (16, false) => {
                let dividend = ((self.cpu.read_reg(Register::DX)? as u32) << 16
                    | self.cpu.read_reg(Register::AX)? as u32)
                    as u128;
                let q = dividend / divisor as u128;
                let r = dividend % divisor as u128;
                if q > 0xffff {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::AX, q as u64)?;
                self.cpu.write_reg(Register::DX, r as u64)?;
            }
            (32, false) => {
                let dividend = ((self.cpu.read_reg(Register::EDX)? as u64) << 32
                    | self.cpu.read_reg(Register::EAX)? as u64)
                    as u128;
                let q = dividend / divisor as u128;
                let r = dividend % divisor as u128;
                if q > 0xffff_ffff {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::EAX, q as u64)?;
                self.cpu.write_reg(Register::EDX, r as u64)?;
            }
            (64, false) => {
                let dividend = ((self.cpu.read_reg(Register::RDX)? as u128) << 64)
                    | self.cpu.read_reg(Register::RAX)? as u128;
                let q = dividend / divisor as u128;
                let r = dividend % divisor as u128;
                if q > u64::MAX as u128 {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::RAX, q as u64)?;
                self.cpu.write_reg(Register::RDX, r as u64)?;
            }
            (8, true) => {
                let dividend = self.cpu.read_reg(Register::AX)? as i16 as i128;
                let divisor = divisor as i8 as i128;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q < i8::MIN as i128 || q > i8::MAX as i128 {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::AL, (q as i8) as u8 as u64)?;
                self.cpu.write_reg(Register::AH, (r as i8) as u8 as u64)?;
            }
            (16, true) => {
                let dividend = (((self.cpu.read_reg(Register::DX)? as u32) << 16)
                    | self.cpu.read_reg(Register::AX)? as u32) as i32
                    as i128;
                let divisor = divisor as i16 as i128;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q < i16::MIN as i128 || q > i16::MAX as i128 {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::AX, q as u64)?;
                self.cpu.write_reg(Register::DX, r as u64)?;
            }
            (32, true) => {
                let dividend = (((self.cpu.read_reg(Register::EDX)? as u64) << 32)
                    | self.cpu.read_reg(Register::EAX)? as u64)
                    as i64 as i128;
                let divisor = divisor as i32 as i128;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q < i32::MIN as i128 || q > i32::MAX as i128 {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::EAX, q as u64)?;
                self.cpu.write_reg(Register::EDX, r as u64)?;
            }
            (64, true) => {
                let dividend = (((self.cpu.read_reg(Register::RDX)? as u128) << 64)
                    | self.cpu.read_reg(Register::RAX)? as u128)
                    as i128;
                let divisor = divisor as i64 as i128;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q < i64::MIN as i128 || q > i64::MAX as i128 {
                    return Err(EmuException::DivideError);
                }
                self.cpu.write_reg(Register::RAX, q as u64)?;
                self.cpu.write_reg(Register::RDX, r as u64)?;
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }

        Ok(())
    }

    fn exec_cbw_family(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 0 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        match instr.mnemonic() {
            Mnemonic::Cbw => {
                let al = self.cpu.read_reg(Register::AL)? as i8 as i16;
                self.cpu.write_reg(Register::AX, al as u16 as u64)?;
            }
            Mnemonic::Cwde => {
                let ax = self.cpu.read_reg(Register::AX)? as i16 as i32;
                self.cpu.write_reg(Register::EAX, ax as u32 as u64)?;
            }
            Mnemonic::Cdqe => {
                let eax = self.cpu.read_reg(Register::EAX)? as i32 as i64;
                self.cpu.write_reg(Register::RAX, eax as u64)?;
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }
        Ok(())
    }

    fn exec_cwd_family(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 0 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        match instr.mnemonic() {
            Mnemonic::Cwd => {
                let ax = self.cpu.read_reg(Register::AX)? as i16;
                let dx = if ax < 0 { 0xffff } else { 0 };
                self.cpu.write_reg(Register::DX, dx)?;
            }
            Mnemonic::Cdq => {
                let eax = self.cpu.read_reg(Register::EAX)? as i32;
                let edx = if eax < 0 { 0xffff_ffff } else { 0 };
                self.cpu.write_reg(Register::EDX, edx)?;
            }
            Mnemonic::Cqo => {
                let rax = self.cpu.read_reg(Register::RAX)? as i64;
                let rdx = if rax < 0 { u64::MAX } else { 0 };
                self.cpu.write_reg(Register::RDX, rdx)?;
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }
        Ok(())
    }

    fn exec_shift(&mut self, instr: &Instruction, kind: ShiftKind) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        let count = self.read_operand(instr, 1)? as u32;
        let mask = mask_for_bits(bits);
        let count = count & if bits == 64 { 0x3f } else { 0x1f };
        if count == 0 {
            return Ok(());
        }

        let val = val & mask;
        let mut result = val;
        let mut cf = false;
        for _ in 0..count {
            match kind {
                ShiftKind::Shl => {
                    cf = (result & sign_bit(bits)) != 0;
                    result = (result << 1) & mask;
                }
                ShiftKind::Shr => {
                    cf = (result & 1) != 0;
                    result >>= 1;
                }
                ShiftKind::Sar => {
                    cf = (result & 1) != 0;
                    let sign = result & sign_bit(bits);
                    result >>= 1;
                    result |= sign;
                }
            }
        }

        self.cpu.materialize_lazy_flags();
        self.write_operand(instr, 0, result)?;

        self.cpu.set_flag(Flag::Cf, cf);
        self.cpu.set_flag(Flag::Zf, (result & mask) == 0);
        self.cpu.set_flag(Flag::Sf, (result & sign_bit(bits)) != 0);
        self.cpu.set_flag(Flag::Pf, parity_even(result as u8));
        self.cpu.set_flag(Flag::Af, false);

        if count == 1 {
            let of = match kind {
                ShiftKind::Shl => ((result & sign_bit(bits)) != 0) ^ cf,
                ShiftKind::Shr => (val & sign_bit(bits)) != 0,
                ShiftKind::Sar => false,
            };
            self.cpu.set_flag(Flag::Of, of);
        } else {
            self.cpu.set_flag(Flag::Of, false);
        }

        Ok(())
    }

    fn exec_shld_shrd(&mut self, instr: &Instruction, left: bool) -> Result<(), EmuException> {
        if instr.op_count() != 3 {
            return Err(EmuException::Unimplemented(instr.code()));
        }

        let (dst_val, bits) = self.read_operand_with_size(instr, 0)?;
        let (src_val, _) = self.read_operand_with_size(instr, 1)?;
        let count_raw = self.read_operand(instr, 2)? as u32;

        let mask = mask_for_bits(bits);
        let dst = dst_val & mask;
        let src = src_val & mask;
        let count = count_raw & if bits == 64 { 0x3f } else { 0x1f };
        if count == 0 {
            return Ok(());
        }

        let width = bits * 2;
        let width_mask = if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };

        let (result, cf) = if left {
            let concat = (((dst as u128) << bits) | src as u128) & width_mask;
            let cf = ((concat >> (width - count)) & 1) != 0;
            let shifted = (concat << count) & width_mask;
            let result = ((shifted >> bits) & mask as u128) as u64;
            (result, cf)
        } else {
            let concat = (((src as u128) << bits) | dst as u128) & width_mask;
            let cf = ((concat >> (count - 1)) & 1) != 0;
            let shifted = concat >> count;
            let result = (shifted & mask as u128) as u64;
            (result, cf)
        };

        self.cpu.materialize_lazy_flags();
        self.write_operand(instr, 0, result)?;

        self.cpu.set_flag(Flag::Cf, cf);
        self.cpu.set_flag(Flag::Zf, (result & mask) == 0);
        self.cpu.set_flag(Flag::Sf, (result & sign_bit(bits)) != 0);
        self.cpu.set_flag(Flag::Pf, parity_even(result as u8));
        self.cpu.set_flag(Flag::Af, false);
        if count == 1 {
            let of = if left {
                ((result & sign_bit(bits)) != 0) ^ cf
            } else {
                let old_msb = (dst & sign_bit(bits)) != 0;
                let new_msb = (result & sign_bit(bits)) != 0;
                old_msb ^ new_msb
            };
            self.cpu.set_flag(Flag::Of, of);
        } else {
            self.cpu.set_flag(Flag::Of, false);
        }

        Ok(())
    }

    fn exec_rotate(&mut self, instr: &Instruction, kind: RotateKind) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (val, bits) = self.read_operand_with_size(instr, 0)?;
        let count_raw = self.read_operand(instr, 1)? as u32;
        let mask = mask_for_bits(bits);
        let width = match kind {
            RotateKind::Rol | RotateKind::Ror => bits,
            RotateKind::Rcl | RotateKind::Rcr => bits + 1,
        };
        let mut count = count_raw & if bits == 64 { 0x3f } else { 0x1f };
        if width != 0 {
            count %= width;
        }
        if count == 0 {
            return Ok(());
        }

        let (result, cf) = match kind {
            RotateKind::Rol | RotateKind::Ror => {
                let mut result = val & mask;
                for _ in 0..count {
                    match kind {
                        RotateKind::Rol => {
                            let msb = (result & sign_bit(bits)) != 0;
                            result = ((result << 1) & mask) | (msb as u64);
                        }
                        RotateKind::Ror => {
                            let lsb = (result & 1) != 0;
                            result = (result >> 1) | ((lsb as u64) << (bits - 1));
                        }
                        _ => unreachable!(),
                    }
                }
                let cf = match kind {
                    RotateKind::Rol => (result & 1) != 0,
                    RotateKind::Ror => (result & sign_bit(bits)) != 0,
                    _ => unreachable!(),
                };
                (result, cf)
            }
            RotateKind::Rcl | RotateKind::Rcr => {
                let cf_in = self.cpu.get_flag(Flag::Cf);
                let ext_bits = bits + 1;
                let ext_mask = (1u128 << ext_bits) - 1;
                let ext = (val as u128 & mask as u128) | ((cf_in as u128) << bits);
                let rotated = match kind {
                    RotateKind::Rcl => {
                        ((ext << count) | (ext >> (ext_bits - count))) & ext_mask
                    }
                    RotateKind::Rcr => {
                        ((ext >> count) | (ext << (ext_bits - count))) & ext_mask
                    }
                    _ => unreachable!(),
                };
                let cf = ((rotated >> bits) & 1) != 0;
                let result = (rotated & mask as u128) as u64;
                (result, cf)
            }
        };

        self.cpu.materialize_lazy_flags();
        self.write_operand(instr, 0, result)?;
        self.cpu.set_flag(Flag::Cf, cf);
        if count == 1 {
            let of = match kind {
                RotateKind::Rol | RotateKind::Rcl => ((result & sign_bit(bits)) != 0) ^ cf,
                RotateKind::Ror | RotateKind::Rcr => {
                    let msb = (result & sign_bit(bits)) != 0;
                    let second = (result & (sign_bit(bits) >> 1)) != 0;
                    msb ^ second
                }
            };
            self.cpu.set_flag(Flag::Of, of);
        }
        Ok(())
    }

    fn exec_bswap(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 || op_kind(instr, 0) != OpKind::Register {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let reg = op_register(instr, 0);
        let bits = reg_bits(reg)?;
        let val = self.cpu.read_reg(reg)?;
        let swapped = match bits {
            32 => (val as u32).swap_bytes() as u64,
            64 => (val as u64).swap_bytes(),
            _ => return Err(EmuException::InvalidOpcode),
        };
        self.cpu.write_reg(reg, swapped)?;
        Ok(())
    }

    fn exec_xadd(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }

        let (dst_val, bits) = self.read_operand_with_size(instr, 0)?;
        let (src_val, _) = self.read_operand_with_size(instr, 1)?;
        let mask = mask_for_bits(bits);

        let dst = dst_val & mask;
        let src = src_val & mask;
        let result = dst.wrapping_add(src) & mask;

        self.cpu.set_lazy_flags(LazyFlags {
            op: LazyOp::Add { carry_in: 0 },
            size_bits: bits,
            lhs: dst,
            rhs: src,
            result,
        });

        self.write_operand(instr, 0, result)?;

        let aliasing_regs = op_kind(instr, 0) == OpKind::Register
            && op_kind(instr, 1) == OpKind::Register
            && op_register(instr, 0) == op_register(instr, 1);
        if !aliasing_regs {
            self.write_operand(instr, 1, dst)?;
        }
        Ok(())
    }

    fn exec_bit_op(&mut self, instr: &Instruction, op: BitOpKind) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let bits = operand_bits(instr, 0)?;
        let mask = mask_for_bits(bits);

        let (index_raw, index_bits) = self.read_operand_with_size(instr, 1)?;
        let index = sign_extend_value(index_raw & mask_for_bits(index_bits), index_bits)? as i64;

        let bit_index = (index as u64) & ((bits as u64) - 1);
        let bit_mask = 1u64 << bit_index;

        let kind = op_kind(instr, 0);
        let (old_bit, new_val) = match kind {
            OpKind::Register => {
                let reg = op_register(instr, 0);
                let val = self.cpu.read_reg(reg)? & mask;
                let old_bit = (val & bit_mask) != 0;
                let new_val = match op {
                    BitOpKind::Bt => val,
                    BitOpKind::Bts => val | bit_mask,
                    BitOpKind::Btr => val & !bit_mask,
                    BitOpKind::Btc => val ^ bit_mask,
                };
                (old_bit, Some(new_val))
            }
            OpKind::Memory => {
                let base_addr = self.calc_linear_addr(instr, 0, 0)?;
                let unit_shift = match bits {
                    16 => 4,
                    32 => 5,
                    64 => 6,
                    _ => return Err(EmuException::InvalidOpcode),
                };
                let bytes_per_unit = (bits / 8) as i64;
                let unit_index = index >> unit_shift;
                let byte_off = unit_index.saturating_mul(bytes_per_unit);
                let addr = self.cpu.apply_a20(if byte_off >= 0 {
                    base_addr.wrapping_add(byte_off as u64)
                } else {
                    base_addr.wrapping_sub((-byte_off) as u64)
                });

                let val = match bits {
                    16 => self.mem.read_u16(addr)? as u64,
                    32 => self.mem.read_u32(addr)? as u64,
                    64 => self.mem.read_u64(addr)?,
                    _ => return Err(EmuException::InvalidOpcode),
                } & mask;

                let old_bit = (val & bit_mask) != 0;
                let new_val = match op {
                    BitOpKind::Bt => None,
                    BitOpKind::Bts => Some(val | bit_mask),
                    BitOpKind::Btr => Some(val & !bit_mask),
                    BitOpKind::Btc => Some(val ^ bit_mask),
                };
                if let Some(write_val) = new_val {
                    match bits {
                        16 => self.mem.write_u16(addr, write_val as u16)?,
                        32 => self.mem.write_u32(addr, write_val as u32)?,
                        64 => self.mem.write_u64(addr, write_val)?,
                        _ => return Err(EmuException::InvalidOpcode),
                    }
                }
                (old_bit, None)
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };

        if matches!(kind, OpKind::Register) {
            if let Some(v) = new_val {
                if op != BitOpKind::Bt {
                    self.write_operand(instr, 0, v)?;
                }
            }
        }

        self.cpu.materialize_lazy_flags();
        self.cpu.set_flag(Flag::Cf, old_bit);
        Ok(())
    }

    fn exec_bscan(&mut self, instr: &Instruction, reverse: bool) -> Result<(), EmuException> {
        if instr.op_count() != 2 || op_kind(instr, 0) != OpKind::Register {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let dst_reg = op_register(instr, 0);
        let (src_val, bits) = self.read_operand_with_size(instr, 1)?;
        let src = src_val & mask_for_bits(bits);

        self.cpu.materialize_lazy_flags();
        if src == 0 {
            self.cpu.set_flag(Flag::Zf, true);
            return Ok(());
        }

        let idx = if reverse {
            63 - (src as u64).leading_zeros()
        } else {
            (src as u64).trailing_zeros()
        } as u64;
        self.cpu.write_reg(dst_reg, idx)?;
        self.cpu.set_flag(Flag::Zf, false);
        Ok(())
    }

    fn exec_jmp(&mut self, instr: &Instruction) -> Result<bool, EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        match op_kind(instr, 0) {
            OpKind::NearBranch16 => self.cpu.set_rip(instr.near_branch16() as u64),
            OpKind::NearBranch32 => self.cpu.set_rip(instr.near_branch32() as u64),
            OpKind::NearBranch64 => self.cpu.set_rip(instr.near_branch64()),
            OpKind::Register | OpKind::Memory => {
                let target = self.read_operand(instr, 0)?;
                self.cpu.set_rip(target);
            }
            OpKind::FarBranch16 => {
                let selector = instr.far_branch_selector();
                let offset = instr.far_branch16() as u64;
                self.load_cs(selector, offset)?;
            }
            OpKind::FarBranch32 => {
                let selector = instr.far_branch_selector();
                let offset = instr.far_branch32() as u64;
                self.load_cs(selector, offset)?;
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }
        Ok(true)
    }

    fn exec_call(&mut self, instr: &Instruction, next_ip: u64) -> Result<bool, EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        match op_kind(instr, 0) {
            OpKind::NearBranch16 => {
                self.push(next_ip, 16)?;
                self.cpu.set_rip(instr.near_branch16() as u64);
            }
            OpKind::NearBranch32 => {
                self.push(next_ip, 32)?;
                self.cpu.set_rip(instr.near_branch32() as u64);
            }
            OpKind::NearBranch64 => {
                self.push(next_ip, 64)?;
                self.cpu.set_rip(instr.near_branch64());
            }
            OpKind::Register | OpKind::Memory => {
                let bits = operand_bits(instr, 0)?;
                self.push(next_ip, bits)?;
                let target = self.read_operand(instr, 0)?;
                self.cpu.set_rip(target);
            }
            OpKind::FarBranch16 => {
                let selector = instr.far_branch_selector();
                let offset = instr.far_branch16() as u64;
                self.push(self.cpu.cs().selector as u64, 16)?;
                self.push(next_ip, 16)?;
                self.load_cs(selector, offset)?;
            }
            OpKind::FarBranch32 => {
                let selector = instr.far_branch_selector();
                let offset = instr.far_branch32() as u64;
                self.push(self.cpu.cs().selector as u64, 16)?;
                self.push(next_ip, 32)?;
                self.load_cs(selector, offset)?;
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }
        Ok(true)
    }

    fn exec_ret(&mut self, instr: &Instruction) -> Result<bool, EmuException> {
        let bits = match self.cpu.mode {
            CpuMode::Real => 16,
            CpuMode::Protected => 32,
            CpuMode::Long => 64,
        };
        let ip = self.pop(bits)?;
        if instr.op_count() == 1 {
            let adj = self.read_operand(instr, 0)? as u64;
            self.adjust_sp(adj)?;
        }
        self.cpu.set_rip(ip);
        Ok(true)
    }

    fn exec_retf(&mut self, instr: &Instruction) -> Result<bool, EmuException> {
        let ip_bits = match self.cpu.mode {
            CpuMode::Real => 16,
            CpuMode::Protected => 32,
            CpuMode::Long => 64,
        };
        let ip = self.pop(ip_bits)?;
        let cs = self.pop(16)? as u16;
        if instr.op_count() == 1 {
            let adj = self.read_operand(instr, 0)? as u64;
            self.adjust_sp(adj)?;
        }
        self.load_cs(cs, ip)?;
        Ok(true)
    }

    fn exec_jcc(&mut self, instr: &Instruction) -> Result<bool, EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let cond = match instr.mnemonic() {
            Mnemonic::Jo => self.cpu.get_flag(Flag::Of),
            Mnemonic::Jno => !self.cpu.get_flag(Flag::Of),
            Mnemonic::Jb => self.cpu.get_flag(Flag::Cf),
            Mnemonic::Jae => !self.cpu.get_flag(Flag::Cf),
            Mnemonic::Je => self.cpu.get_flag(Flag::Zf),
            Mnemonic::Jne => !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Jbe => self.cpu.get_flag(Flag::Cf) || self.cpu.get_flag(Flag::Zf),
            Mnemonic::Ja => !self.cpu.get_flag(Flag::Cf) && !self.cpu.get_flag(Flag::Zf),
            Mnemonic::Js => self.cpu.get_flag(Flag::Sf),
            Mnemonic::Jns => !self.cpu.get_flag(Flag::Sf),
            Mnemonic::Jp => self.cpu.get_flag(Flag::Pf),
            Mnemonic::Jnp => !self.cpu.get_flag(Flag::Pf),
            Mnemonic::Jl => self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of),
            Mnemonic::Jge => self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of),
            Mnemonic::Jle => {
                self.cpu.get_flag(Flag::Zf)
                    || (self.cpu.get_flag(Flag::Sf) != self.cpu.get_flag(Flag::Of))
            }
            Mnemonic::Jg => {
                !self.cpu.get_flag(Flag::Zf)
                    && (self.cpu.get_flag(Flag::Sf) == self.cpu.get_flag(Flag::Of))
            }
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        if cond {
            match op_kind(instr, 0) {
                OpKind::NearBranch16 => self.cpu.set_rip(instr.near_branch16() as u64),
                OpKind::NearBranch32 => self.cpu.set_rip(instr.near_branch32() as u64),
                OpKind::NearBranch64 => self.cpu.set_rip(instr.near_branch64()),
                _ => return Err(EmuException::Unimplemented(instr.code())),
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn exec_loop(&mut self, instr: &Instruction, kind: LoopKind) -> Result<bool, EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (count_reg, bits) = self.string_count_reg()?;
        let mut count = self.cpu.read_reg(count_reg)? & mask_for_bits(bits);
        count = count.wrapping_sub(1) & mask_for_bits(bits);
        self.cpu.write_reg(count_reg, count)?;

        let zf = self.cpu.get_flag(Flag::Zf);
        let take = match kind {
            LoopKind::Loop => count != 0,
            LoopKind::Loope => count != 0 && zf,
            LoopKind::Loopne => count != 0 && !zf,
        };

        if take {
            match op_kind(instr, 0) {
                OpKind::NearBranch16 => self.cpu.set_rip(instr.near_branch16() as u64),
                OpKind::NearBranch32 => self.cpu.set_rip(instr.near_branch32() as u64),
                _ => return Err(EmuException::Unimplemented(instr.code())),
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn exec_jcxz(&mut self, instr: &Instruction) -> Result<bool, EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let (count_reg, bits) = self.string_count_reg()?;
        let count = self.cpu.read_reg(count_reg)? & mask_for_bits(bits);
        if count == 0 {
            match op_kind(instr, 0) {
                OpKind::NearBranch16 => self.cpu.set_rip(instr.near_branch16() as u64),
                OpKind::NearBranch32 => self.cpu.set_rip(instr.near_branch32() as u64),
                _ => return Err(EmuException::Unimplemented(instr.code())),
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn exec_movs(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        let elem = string_elem_size(instr.mnemonic())?;
        self.exec_string(instr, elem, StringOp::Movs)
    }

    fn exec_stos(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        let elem = string_elem_size(instr.mnemonic())?;
        self.exec_string(instr, elem, StringOp::Stos)
    }

    fn exec_lods(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        let elem = string_elem_size(instr.mnemonic())?;
        self.exec_string(instr, elem, StringOp::Lods)
    }

    fn exec_cmps(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        let elem = string_elem_size(instr.mnemonic())?;
        self.exec_string(instr, elem, StringOp::Cmps)
    }

    fn exec_scas(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        let elem = string_elem_size(instr.mnemonic())?;
        self.exec_string(instr, elem, StringOp::Scas)
    }

    fn exec_int(&mut self, instr: &Instruction, next_ip: u64) -> Result<bool, EmuException> {
        let vector = instr.immediate8();
        self.deliver_interrupt(vector, next_ip)
    }

    fn exec_int3(&mut self, next_ip: u64) -> Result<bool, EmuException> {
        self.deliver_interrupt(3, next_ip)
    }

    fn exec_iret(&mut self, _instr: &Instruction) -> Result<bool, EmuException> {
        match self.cpu.mode {
            CpuMode::Real => {
                let ip = self.pop(16)?;
                let cs = self.pop(16)? as u16;
                let flags = self.pop(16)? as u16;
                self.load_cs(cs, ip)?;
                let rf = (self.cpu.rflags() & !0xffff) | flags as u64;
                self.cpu.set_rflags(rf);
            }
            CpuMode::Protected => {
                let ip = self.pop(32)?;
                let cs = self.pop(32)? as u16;
                let flags = self.pop(32)? as u32;
                self.load_cs(cs, ip)?;
                let rf = (self.cpu.rflags() & !0xffff_ffff) | flags as u64;
                self.cpu.set_rflags(rf);
            }
            CpuMode::Long => {
                let ip = self.pop(64)?;
                let cs = self.pop(64)? as u16;
                let flags = self.pop(64)?;
                self.load_cs(cs, ip)?;
                self.cpu.set_rflags(flags);
            }
        }
        Ok(true)
    }

    fn exec_cpuid(&mut self) -> Result<(), EmuException> {
        let leaf = self.cpu.read_reg(Register::EAX)? as u32;
        let subleaf = self.cpu.read_reg(Register::ECX)? as u32;
        let (eax, ebx, ecx, edx): (u32, u32, u32, u32) = match leaf {
            0 => {
                // Vendor string in EBX, EDX, ECX.
                // CPUID returns the vendor string in EBX, EDX, ECX order.
                (
                    1,
                    u32::from_le_bytes(*b"Aero"),
                    u32::from_le_bytes(*b"CPU "),
                    u32::from_le_bytes(*b"Emu "),
                )
            }
            1 => {
                // A conservative baseline: SSE/SSE2, TSC, MSR.
                let eax = 0x0000_0663;
                let edx = (1 << 4) | (1 << 5) | (1 << 8) | (1 << 23) | (1 << 24); // TSC, MSR, CMPXCHG8B, SSE, SSE2
                let ecx = 0;
                (eax, 0, ecx, edx)
            }
            0x8000_0000 => (0x8000_0007, 0, 0, 0),
            0x8000_0001 => {
                // Extended feature flags.
                // - Bit 29: Long Mode (AMD64)
                // - Bit 27: RDTSCP
                let edx = (1 << 29) | (1 << 27);
                (0, 0, 0, edx)
            }
            0x8000_0007 => {
                // Advanced power management info.
                // - EDX bit 8: Invariant TSC
                let edx = 1 << 8;
                (0, 0, 0, edx)
            }
            _ => {
                let _ = subleaf;
                (0, 0, 0, 0)
            }
        };

        self.cpu.write_reg(Register::EAX, eax as u64)?;
        self.cpu.write_reg(Register::EBX, ebx as u64)?;
        self.cpu.write_reg(Register::ECX, ecx as u64)?;
        self.cpu.write_reg(Register::EDX, edx as u64)?;
        Ok(())
    }

    fn exec_rdtsc(&mut self) -> Result<(), EmuException> {
        let tsc = self.cpu.tsc;
        self.cpu
            .write_reg(Register::EAX, (tsc & 0xffff_ffff) as u64)?;
        self.cpu.write_reg(Register::EDX, (tsc >> 32) as u64)?;
        Ok(())
    }

    fn exec_rdtscp(&mut self) -> Result<(), EmuException> {
        let tsc = self.cpu.tsc;
        let aux = self
            .cpu
            .msr
            .get(&IA32_TSC_AUX)
            .copied()
            .unwrap_or(0) as u32;
        self.cpu
            .write_reg(Register::EAX, (tsc & 0xffff_ffff) as u64)?;
        self.cpu.write_reg(Register::EDX, (tsc >> 32) as u64)?;
        self.cpu.write_reg(Register::ECX, aux as u64)?;
        Ok(())
    }

    fn exec_rdmsr(&mut self) -> Result<(), EmuException> {
        let msr = self.cpu.read_reg(Register::ECX)? as u32;
        let val = match msr {
            IA32_TSC => self.cpu.tsc,
            IA32_TSC_AUX => self.cpu.msr.get(&msr).copied().unwrap_or(0) & 0xffff_ffff,
            _ => *self.cpu.msr.get(&msr).unwrap_or(&0),
        };
        self.cpu
            .write_reg(Register::EAX, (val & 0xffff_ffff) as u64)?;
        self.cpu.write_reg(Register::EDX, (val >> 32) as u64)?;
        Ok(())
    }

    fn exec_wrmsr(&mut self) -> Result<(), EmuException> {
        let msr = self.cpu.read_reg(Register::ECX)? as u32;
        let lo = self.cpu.read_reg(Register::EAX)? & 0xffff_ffff;
        let hi = self.cpu.read_reg(Register::EDX)? & 0xffff_ffff;
        let val = lo | (hi << 32);
        match msr {
            IA32_TSC => {
                // Canonicalize IA32_TSC through the dedicated CPU TSC field.
                self.cpu.tsc = val;
                self.cpu.msr.remove(&msr);
            }
            IA32_TSC_AUX => {
                // Only the low 32 bits are architecturally visible.
                self.cpu.msr.insert(msr, val & 0xffff_ffff);
            }
            _ => {
                self.cpu.msr.insert(msr, val);
            }
        }
        Ok(())
    }

    fn exec_pause(&self) {
        std::hint::spin_loop();
    }

    fn exec_in(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let port = self.read_operand(instr, 1)? as u16;
        let dst_reg = op_register(instr, 0);
        let bits = reg_bits(dst_reg)?;
        let val = match bits {
            8 => self.ports.in_u8(port) as u64,
            16 => self.ports.in_u16(port) as u64,
            32 => self.ports.in_u32(port) as u64,
            _ => return Err(EmuException::Unimplemented(instr.code())),
        };
        self.cpu.write_reg(dst_reg, val)?;
        Ok(())
    }

    fn exec_out(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 2 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let port = self.read_operand(instr, 0)? as u16;
        let (val, bits) = self.read_operand_with_size(instr, 1)?;
        match bits {
            8 => self.ports.out_u8(port, val as u8),
            16 => self.ports.out_u16(port, val as u16),
            32 => self.ports.out_u32(port, val as u32),
            _ => return Err(EmuException::Unimplemented(instr.code())),
        }
        Ok(())
    }

    fn exec_lgdt(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        self.exec_ldt(instr, true)
    }

    fn exec_lidt(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        self.exec_ldt(instr, false)
    }

    fn exec_ldt(&mut self, instr: &Instruction, is_gdt: bool) -> Result<(), EmuException> {
        if instr.op_count() != 1 || op_kind(instr, 0) != OpKind::Memory {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let addr = self.calc_linear_addr(instr, 0, 0)?;
        let limit = self.mem.read_u16(addr)? as u16;

        // LGDT/LIDT encode the same operand format:
        // - 16/32-bit modes: 2-byte limit + 4-byte base
        // - 64-bit mode:    2-byte limit + 8-byte base
        let base = match self.cpu.mode {
            CpuMode::Long => self.mem.read_u64(addr + 2)?,
            CpuMode::Real | CpuMode::Protected => self.mem.read_u32(addr + 2)? as u64,
        };

        if is_gdt {
            self.cpu.gdtr.base = base;
            self.cpu.gdtr.limit = limit;
        } else {
            self.cpu.idtr.base = base;
            self.cpu.idtr.limit = limit;
        }
        Ok(())
    }

    fn exec_ltr(&mut self, instr: &Instruction) -> Result<(), EmuException> {
        if instr.op_count() != 1 {
            return Err(EmuException::Unimplemented(instr.code()));
        }
        let selector = self.read_operand(instr, 0)? as u16;
        self.cpu.tr = selector;
        Ok(())
    }

    fn deliver_interrupt(&mut self, vector: u8, next_ip: u64) -> Result<bool, EmuException> {
        match self.cpu.mode {
            CpuMode::Real => {
                let flags = self.cpu.rflags() as u16;
                self.push(flags as u64, 16)?;
                self.push(self.cpu.cs().selector as u64, 16)?;
                self.push(next_ip, 16)?;

                // INT clears IF and TF.
                let rf = self.cpu.rflags() & !(FLAG_IF | FLAG_TF);
                self.cpu.set_rflags(rf);

                let ivt = vector as u64 * 4;
                let offset = self.mem.read_u16(ivt)? as u64;
                let cs = self.mem.read_u16(ivt + 2)? as u16;
                self.load_cs(cs, offset)?;
                Ok(true)
            }
            CpuMode::Protected => {
                let entry_addr = self.cpu.idtr.base + (vector as u64) * 8;
                let entry = self.mem.read_u64(entry_addr)?;
                let selector = ((entry >> 16) & 0xffff) as u16;
                let type_attr = ((entry >> 40) & 0xff) as u8;
                if type_attr & 0x80 == 0 {
                    return Err(EmuException::InvalidOpcode);
                }
                let offset_low = (entry & 0xffff) as u32;
                let offset_high = ((entry >> 48) & 0xffff) as u32;
                let offset = (offset_low | (offset_high << 16)) as u64;

                self.push(self.cpu.rflags() & 0xffff_ffff, 32)?;
                self.push(self.cpu.cs().selector as u64, 32)?;
                self.push(next_ip, 32)?;

                if type_attr & 0xF == 0xE {
                    let rf = self.cpu.rflags() & !FLAG_IF;
                    self.cpu.set_rflags(rf);
                }

                self.load_cs(selector, offset)?;
                Ok(true)
            }
            CpuMode::Long => {
                let entry_addr = self.cpu.idtr.base + (vector as u64) * 16;
                let lo = self.mem.read_u64(entry_addr)?;
                let hi = self.mem.read_u64(entry_addr + 8)?;
                let selector = ((lo >> 16) & 0xffff) as u16;
                let type_attr = ((lo >> 40) & 0xff) as u8;
                if type_attr & 0x80 == 0 {
                    return Err(EmuException::InvalidOpcode);
                }
                let offset_low = (lo & 0xffff) as u64;
                let offset_mid = ((lo >> 48) & 0xffff) as u64;
                let offset_high = (hi & 0xffff_ffff) as u64;
                let offset = offset_low | (offset_mid << 16) | (offset_high << 32);

                self.push(self.cpu.rflags(), 64)?;
                self.push(self.cpu.cs().selector as u64, 64)?;
                self.push(next_ip, 64)?;

                if type_attr & 0xF == 0xE {
                    let rf = self.cpu.rflags() & !FLAG_IF;
                    self.cpu.set_rflags(rf);
                }
                self.load_cs(selector, offset)?;
                Ok(true)
            }
        }
    }

    fn load_cs(&mut self, selector: u16, offset: u64) -> Result<(), EmuException> {
        match self.cpu.mode {
            CpuMode::Real => {
                self.cpu.set_segment_selector(Register::CS, selector)?;
            }
            CpuMode::Protected | CpuMode::Long => {
                self.cpu
                    .load_segment_from_gdt(Register::CS, selector, |addr| {
                        self.mem.read_u64(addr)
                    })?;
            }
        }
        self.cpu.set_rip(offset);
        Ok(())
    }

    fn push(&mut self, value: u64, bits: u32) -> Result<(), EmuException> {
        let bytes = bits / 8;
        let sp_reg = stack_ptr_reg(self.cpu.mode);
        let sp_mask = mask_for_bits(reg_bits(sp_reg)?);
        let mut sp = self.cpu.read_reg(sp_reg)? & sp_mask;
        sp = sp.wrapping_sub(bytes as u64) & sp_mask;
        self.cpu.write_reg(sp_reg, sp)?;
        let addr = self.cpu.apply_a20(self.cpu.ss().base.wrapping_add(sp));
        match bytes {
            2 => self.mem.write_u16(addr, value as u16)?,
            4 => self.mem.write_u32(addr, value as u32)?,
            8 => self.mem.write_u64(addr, value)?,
            _ => return Err(EmuException::Unimplemented(Code::INVALID)),
        }
        Ok(())
    }

    fn pop(&mut self, bits: u32) -> Result<u64, EmuException> {
        let bytes = bits / 8;
        let sp_reg = stack_ptr_reg(self.cpu.mode);
        let sp_mask = mask_for_bits(reg_bits(sp_reg)?);
        let sp = self.cpu.read_reg(sp_reg)? & sp_mask;
        let addr = self.cpu.apply_a20(self.cpu.ss().base.wrapping_add(sp));
        let val = match bytes {
            2 => self.mem.read_u16(addr)? as u64,
            4 => self.mem.read_u32(addr)? as u64,
            8 => self.mem.read_u64(addr)?,
            _ => return Err(EmuException::Unimplemented(Code::INVALID)),
        };
        let new_sp = sp.wrapping_add(bytes as u64) & sp_mask;
        self.cpu.write_reg(sp_reg, new_sp)?;
        Ok(val)
    }

    fn adjust_sp(&mut self, bytes: u64) -> Result<(), EmuException> {
        let sp_reg = stack_ptr_reg(self.cpu.mode);
        let sp_mask = mask_for_bits(reg_bits(sp_reg)?);
        let sp = self.cpu.read_reg(sp_reg)? & sp_mask;
        self.cpu
            .write_reg(sp_reg, sp.wrapping_add(bytes) & sp_mask)?;
        Ok(())
    }

    fn calc_linear_addr(
        &mut self,
        instr: &Instruction,
        _op_index: u32,
        next_ip: u64,
    ) -> Result<u64, EmuException> {
        let next_ip = if next_ip == 0 {
            self.cpu.rip.wrapping_add(instr.len() as u64) & self.cpu.ip_mask()
        } else {
            next_ip
        };

        let offset = self.calc_effective_offset(instr, next_ip)?;
        let mut seg = instr.memory_segment();
        if seg == Register::None {
            seg = Register::DS;
        }
        let seg_base = self.seg_base(seg);
        Ok(self.cpu.apply_a20(seg_base.wrapping_add(offset)))
    }

    fn calc_effective_offset(
        &mut self,
        instr: &Instruction,
        next_ip: u64,
    ) -> Result<u64, EmuException> {
        let base = instr.memory_base();
        let index = instr.memory_index();
        let scale = instr.memory_index_scale() as i128;

        // iced-x86 returns an *absolute* address in `memory_displacement64()` for RIP-relative
        // memory operands (i.e. it already includes `next_ip`). Normalize it back to a raw
        // displacement so the EA calculation doesn't double-add `next_ip`.
        let mut disp = instr.memory_displacement64() as i128;
        if base == Register::RIP {
            disp -= next_ip as i128;
        }

        let mut addr: i128 = disp;
        if base != Register::None {
            let base_val = match base {
                Register::RIP => next_ip,
                Register::EIP => next_ip & 0xffff_ffff,
                _ => self.cpu.read_reg(base)?,
            };
            addr += base_val as i128;
        }
        if index != Register::None {
            let idx = self.cpu.read_reg(index)?;
            addr += (idx as i128) * scale;
        }

        let addr_bits = self.effective_addr_bits();
        Ok((addr as u64) & mask_for_bits(addr_bits))
    }

    fn read_operand(&mut self, instr: &Instruction, op_index: u32) -> Result<u64, EmuException> {
        Ok(self.read_operand_with_size(instr, op_index)?.0)
    }

    fn read_operand_with_size(
        &mut self,
        instr: &Instruction,
        op_index: u32,
    ) -> Result<(u64, u32), EmuException> {
        let kind = op_kind_u32(instr, op_index);
        match kind {
            OpKind::Register => {
                let reg = op_register_u32(instr, op_index);
                let bits = reg_bits(reg)?;
                Ok((self.cpu.read_reg(reg)?, bits))
            }
            OpKind::Memory => {
                let bits = operand_bits_u32(instr, op_index)?;
                let addr = self.calc_linear_addr(instr, op_index, 0)?;
                let val = match bits {
                    8 => self.mem.read_u8(addr)? as u64,
                    16 => self.mem.read_u16(addr)? as u64,
                    32 => self.mem.read_u32(addr)? as u64,
                    64 => self.mem.read_u64(addr)?,
                    _ => return Err(EmuException::Unimplemented(instr.code())),
                };
                Ok((val, bits))
            }
            OpKind::Immediate8 => Ok((instr.immediate8() as u64, 8)),
            OpKind::Immediate16 => Ok((instr.immediate16() as u64, 16)),
            OpKind::Immediate32 => Ok((instr.immediate32() as u64, 32)),
            OpKind::Immediate64 => Ok((instr.immediate64(), 64)),
            OpKind::Immediate8to16 => Ok(((instr.immediate8() as i8 as i64) as u64, 16)),
            OpKind::Immediate8to32 => Ok(((instr.immediate8() as i8 as i64) as u64, 32)),
            OpKind::Immediate8to64 => Ok(((instr.immediate8() as i8 as i64) as u64, 64)),
            OpKind::Immediate32to64 => Ok(((instr.immediate32() as i32 as i64) as u64, 64)),
            _ => Err(EmuException::Unimplemented(instr.code())),
        }
    }

    fn write_operand(
        &mut self,
        instr: &Instruction,
        op_index: u32,
        value: u64,
    ) -> Result<(), EmuException> {
        let kind = op_kind_u32(instr, op_index);
        match kind {
            OpKind::Register => {
                let reg = op_register_u32(instr, op_index);
                self.cpu.write_reg(reg, value)
            }
            OpKind::Memory => {
                let bits = operand_bits_u32(instr, op_index)?;
                let addr = self.calc_linear_addr(instr, op_index, 0)?;
                match bits {
                    8 => self.mem.write_u8(addr, value as u8)?,
                    16 => self.mem.write_u16(addr, value as u16)?,
                    32 => self.mem.write_u32(addr, value as u32)?,
                    64 => self.mem.write_u64(addr, value)?,
                    _ => return Err(EmuException::Unimplemented(instr.code())),
                }
                Ok(())
            }
            _ => Err(EmuException::Unimplemented(instr.code())),
        }
    }

    fn set_mul_flags_unsigned(&mut self, full: u128, op_bits: u32) {
        let overflow = (full >> op_bits) != 0;
        self.cpu.set_flag(Flag::Cf, overflow);
        self.cpu.set_flag(Flag::Of, overflow);
        // Undefined flags: clear them for determinism.
        self.cpu.set_flag(Flag::Pf, false);
        self.cpu.set_flag(Flag::Af, false);
        self.cpu.set_flag(Flag::Zf, false);
        self.cpu.set_flag(Flag::Sf, false);
    }

    fn set_mul_flags_signed(&mut self, full: i128, op_bits: u32) {
        let low_mask = (1u128 << op_bits) - 1;
        let low = (full as u128) & low_mask;
        let sign = ((low >> (op_bits - 1)) & 1) != 0;
        let high = full >> op_bits;
        let overflow = if sign { high != -1 } else { high != 0 };
        self.cpu.set_flag(Flag::Cf, overflow);
        self.cpu.set_flag(Flag::Of, overflow);
        self.cpu.set_flag(Flag::Pf, false);
        self.cpu.set_flag(Flag::Af, false);
        self.cpu.set_flag(Flag::Zf, false);
        self.cpu.set_flag(Flag::Sf, false);
    }

    fn exec_string(
        &mut self,
        instr: &Instruction,
        elem_bytes: u32,
        op: StringOp,
    ) -> Result<(), EmuException> {
        let rep = instr.has_rep_prefix() || instr.has_repne_prefix();
        let repne = instr.has_repne_prefix();
        let (count_reg, count_bits) = self.string_count_reg()?;
        let (si_reg, di_reg) = self.string_index_regs()?;
        let df = self.cpu.get_flag(Flag::Df);
        let delta = if df {
            -(elem_bytes as i64)
        } else {
            elem_bytes as i64
        };

        let mut count = if rep {
            self.cpu.read_reg(count_reg)? & mask_for_bits(count_bits)
        } else {
            1
        };
        if count == 0 {
            return Ok(());
        }

        let src_seg = string_src_segment(instr);
        for _ in 0..count {
            match op {
                StringOp::Movs => {
                    let si = self.cpu.read_reg(si_reg)? & mask_for_bits(count_bits);
                    let di = self.cpu.read_reg(di_reg)? & mask_for_bits(count_bits);
                    let src_addr = self.cpu.apply_a20(self.seg_base(src_seg).wrapping_add(si));
                    let dst_addr = self
                        .cpu
                        .apply_a20(self.seg_base(Register::ES).wrapping_add(di));
                    let val = self.read_mem_elem(src_addr, elem_bytes)?;
                    self.write_mem_elem(dst_addr, elem_bytes, val)?;
                    self.bump_string_index(si_reg, si, delta, count_bits)?;
                    self.bump_string_index(di_reg, di, delta, count_bits)?;
                }
                StringOp::Stos => {
                    let di = self.cpu.read_reg(di_reg)? & mask_for_bits(count_bits);
                    let dst_addr = self
                        .cpu
                        .apply_a20(self.seg_base(Register::ES).wrapping_add(di));
                    let val = match elem_bytes {
                        1 => self.cpu.read_reg(Register::AL)?,
                        2 => self.cpu.read_reg(Register::AX)?,
                        4 => self.cpu.read_reg(Register::EAX)?,
                        8 => self.cpu.read_reg(Register::RAX)?,
                        _ => return Err(EmuException::Unimplemented(instr.code())),
                    };
                    self.write_mem_elem(dst_addr, elem_bytes, val)?;
                    self.bump_string_index(di_reg, di, delta, count_bits)?;
                }
                StringOp::Lods => {
                    let si = self.cpu.read_reg(si_reg)? & mask_for_bits(count_bits);
                    let src_addr = self.cpu.apply_a20(self.seg_base(src_seg).wrapping_add(si));
                    let val = self.read_mem_elem(src_addr, elem_bytes)?;
                    match elem_bytes {
                        1 => self.cpu.write_reg(Register::AL, val)?,
                        2 => self.cpu.write_reg(Register::AX, val)?,
                        4 => self.cpu.write_reg(Register::EAX, val)?,
                        8 => self.cpu.write_reg(Register::RAX, val)?,
                        _ => return Err(EmuException::Unimplemented(instr.code())),
                    };
                    self.bump_string_index(si_reg, si, delta, count_bits)?;
                }
                StringOp::Cmps => {
                    let si = self.cpu.read_reg(si_reg)? & mask_for_bits(count_bits);
                    let di = self.cpu.read_reg(di_reg)? & mask_for_bits(count_bits);
                    let src_addr = self.cpu.apply_a20(self.seg_base(src_seg).wrapping_add(si));
                    let dst_addr = self
                        .cpu
                        .apply_a20(self.seg_base(Register::ES).wrapping_add(di));
                    let src = self.read_mem_elem(src_addr, elem_bytes)?;
                    let dst = self.read_mem_elem(dst_addr, elem_bytes)?;
                    let bits = elem_bytes * 8;
                    let res = (dst.wrapping_sub(src)) & mask_for_bits(bits);
                    self.cpu.set_lazy_flags(LazyFlags {
                        op: LazyOp::Sub { borrow_in: 0 },
                        size_bits: bits,
                        lhs: dst,
                        rhs: src,
                        result: res,
                    });
                    self.bump_string_index(si_reg, si, delta, count_bits)?;
                    self.bump_string_index(di_reg, di, delta, count_bits)?;
                    if rep {
                        let zf = self.cpu.get_flag(Flag::Zf);
                        if repne {
                            if zf {
                                break;
                            }
                        } else if !zf {
                            break;
                        }
                    }
                }
                StringOp::Scas => {
                    let di = self.cpu.read_reg(di_reg)? & mask_for_bits(count_bits);
                    let dst_addr = self
                        .cpu
                        .apply_a20(self.seg_base(Register::ES).wrapping_add(di));
                    let mem_val = self.read_mem_elem(dst_addr, elem_bytes)?;
                    let acc = match elem_bytes {
                        1 => self.cpu.read_reg(Register::AL)?,
                        2 => self.cpu.read_reg(Register::AX)?,
                        4 => self.cpu.read_reg(Register::EAX)?,
                        8 => self.cpu.read_reg(Register::RAX)?,
                        _ => return Err(EmuException::Unimplemented(instr.code())),
                    };
                    let bits = elem_bytes * 8;
                    let res = (acc.wrapping_sub(mem_val)) & mask_for_bits(bits);
                    self.cpu.set_lazy_flags(LazyFlags {
                        op: LazyOp::Sub { borrow_in: 0 },
                        size_bits: bits,
                        lhs: acc,
                        rhs: mem_val,
                        result: res,
                    });
                    self.bump_string_index(di_reg, di, delta, count_bits)?;
                    if rep {
                        let zf = self.cpu.get_flag(Flag::Zf);
                        if repne {
                            if zf {
                                break;
                            }
                        } else if !zf {
                            break;
                        }
                    }
                }
            }

            if rep {
                count = count.wrapping_sub(1);
                self.cpu.write_reg(count_reg, count)?;
                if count == 0 {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(())
    }

    fn bump_string_index(
        &mut self,
        reg: Register,
        cur: u64,
        delta: i64,
        bits: u32,
    ) -> Result<(), EmuException> {
        let mask = mask_for_bits(bits);
        let next = if delta.is_negative() {
            cur.wrapping_sub(delta.wrapping_abs() as u64) & mask
        } else {
            cur.wrapping_add(delta as u64) & mask
        };
        self.cpu.write_reg(reg, next)?;
        Ok(())
    }

    fn read_mem_elem(&mut self, addr: u64, bytes: u32) -> Result<u64, EmuException> {
        Ok(match bytes {
            1 => self.mem.read_u8(addr)? as u64,
            2 => self.mem.read_u16(addr)? as u64,
            4 => self.mem.read_u32(addr)? as u64,
            8 => self.mem.read_u64(addr)?,
            _ => return Err(EmuException::InvalidOpcode),
        })
    }

    fn write_mem_elem(&mut self, addr: u64, bytes: u32, value: u64) -> Result<(), EmuException> {
        match bytes {
            1 => self.mem.write_u8(addr, value as u8)?,
            2 => self.mem.write_u16(addr, value as u16)?,
            4 => self.mem.write_u32(addr, value as u32)?,
            8 => self.mem.write_u64(addr, value)?,
            _ => return Err(EmuException::InvalidOpcode),
        }
        Ok(())
    }

    fn seg_base(&self, seg: Register) -> u64 {
        match seg {
            Register::ES => self.cpu.segments[0].base,
            Register::CS => self.cpu.segments[1].base,
            Register::SS => self.cpu.segments[2].base,
            Register::DS => self.cpu.segments[3].base,
            Register::FS => self.cpu.segments[4].base,
            Register::GS => self.cpu.segments[5].base,
            _ => self.cpu.ds().base,
        }
    }

    fn string_count_reg(&self) -> Result<(Register, u32), EmuException> {
        let bits = self.effective_addr_bits();
        let reg = match bits {
            16 => Register::CX,
            32 => Register::ECX,
            64 => Register::RCX,
            _ => return Err(EmuException::InvalidOpcode),
        };
        Ok((reg, bits))
    }

    fn string_index_regs(&self) -> Result<(Register, Register), EmuException> {
        let bits = self.effective_addr_bits();
        Ok(match bits {
            16 => (Register::SI, Register::DI),
            32 => (Register::ESI, Register::EDI),
            64 => (Register::RSI, Register::RDI),
            _ => return Err(EmuException::InvalidOpcode),
        })
    }

    fn effective_addr_bits(&self) -> u32 {
        match self.cpu.mode {
            CpuMode::Real => {
                if self.addr_size_override {
                    32
                } else {
                    16
                }
            }
            CpuMode::Protected => {
                if self.addr_size_override {
                    16
                } else {
                    32
                }
            }
            CpuMode::Long => {
                if self.addr_size_override {
                    32
                } else {
                    64
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AluBinOp {
    Add,
    Adc,
    Sub,
    Sbb,
    Cmp,
    And,
    Or,
    Xor,
    Test,
}

#[derive(Debug, Clone, Copy)]
enum ShiftKind {
    Shl,
    Shr,
    Sar,
}

#[derive(Debug, Clone, Copy)]
enum RotateKind {
    Rol,
    Ror,
    Rcl,
    Rcr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitOpKind {
    Bt,
    Bts,
    Btr,
    Btc,
}

#[derive(Debug, Clone, Copy)]
enum LoopKind {
    Loop,
    Loope,
    Loopne,
}

#[derive(Debug, Clone, Copy)]
enum StringOp {
    Movs,
    Stos,
    Lods,
    Cmps,
    Scas,
}

fn op_kind(instr: &Instruction, idx: u32) -> OpKind {
    op_kind_u32(instr, idx)
}

fn op_kind_u32(instr: &Instruction, idx: u32) -> OpKind {
    match idx {
        0 => instr.op0_kind(),
        1 => instr.op1_kind(),
        2 => instr.op2_kind(),
        3 => instr.op3_kind(),
        _ => OpKind::Register,
    }
}

fn op_register(instr: &Instruction, idx: u32) -> Register {
    op_register_u32(instr, idx)
}

fn op_register_u32(instr: &Instruction, idx: u32) -> Register {
    match idx {
        0 => instr.op0_register(),
        1 => instr.op1_register(),
        2 => instr.op2_register(),
        3 => instr.op3_register(),
        _ => Register::None,
    }
}

fn operand_bits(instr: &Instruction, idx: u32) -> Result<u32, EmuException> {
    operand_bits_u32(instr, idx)
}

fn operand_bits_u32(instr: &Instruction, idx: u32) -> Result<u32, EmuException> {
    let kind = op_kind_u32(instr, idx);
    match kind {
        OpKind::Register => reg_bits(op_register_u32(instr, idx)),
        OpKind::Memory => memory_bits(instr),
        OpKind::Immediate8 => Ok(8),
        OpKind::Immediate8to16 => Ok(16),
        OpKind::Immediate8to32 => Ok(32),
        OpKind::Immediate8to64 => Ok(64),
        OpKind::Immediate16 => Ok(16),
        OpKind::Immediate32 => Ok(32),
        OpKind::Immediate32to64 => Ok(64),
        OpKind::Immediate64 => Ok(64),
        OpKind::NearBranch16 => Ok(16),
        OpKind::NearBranch32 => Ok(32),
        OpKind::NearBranch64 => Ok(64),
        _ => Err(EmuException::Unimplemented(instr.code())),
    }
}

fn memory_bits(instr: &Instruction) -> Result<u32, EmuException> {
    let bytes = instr.memory_size().size();
    match bytes {
        1 => Ok(8),
        2 => Ok(16),
        4 => Ok(32),
        8 => Ok(64),
        _ => Err(EmuException::Unimplemented(instr.code())),
    }
}

fn reg_bits(reg: Register) -> Result<u32, EmuException> {
    let bytes = reg.size();
    match bytes {
        1 => Ok(8),
        2 => Ok(16),
        4 => Ok(32),
        8 => Ok(64),
        _ => Err(EmuException::InvalidOpcode),
    }
}

fn stack_ptr_reg(mode: CpuMode) -> Register {
    match mode {
        CpuMode::Real => Register::SP,
        CpuMode::Protected => Register::ESP,
        CpuMode::Long => Register::RSP,
    }
}

fn is_segment_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::ES | Register::CS | Register::SS | Register::DS | Register::FS | Register::GS
    )
}

fn mask_for_bits(bits: u32) -> u64 {
    match bits {
        8 => 0xff,
        16 => 0xffff,
        32 => 0xffff_ffff,
        64 => u64::MAX,
        _ => 0,
    }
}

fn sign_bit(bits: u32) -> u64 {
    1u64 << (bits - 1)
}

fn sign_extend_value(value: u64, bits: u32) -> Result<u64, EmuException> {
    Ok(match bits {
        8 => (value as i8 as i64) as u64,
        16 => (value as i16 as i64) as u64,
        32 => (value as i32 as i64) as u64,
        64 => value,
        _ => return Err(EmuException::InvalidOpcode),
    })
}

fn string_src_segment(instr: &Instruction) -> Register {
    let seg = instr.segment_prefix();
    if seg == Register::None {
        Register::DS
    } else {
        seg
    }
}

fn string_elem_size(mnemonic: Mnemonic) -> Result<u32, EmuException> {
    Ok(match mnemonic {
        Mnemonic::Movsb | Mnemonic::Stosb | Mnemonic::Lodsb | Mnemonic::Cmpsb | Mnemonic::Scasb => {
            1
        }
        Mnemonic::Movsw | Mnemonic::Stosw | Mnemonic::Lodsw | Mnemonic::Cmpsw | Mnemonic::Scasw => {
            2
        }
        Mnemonic::Movsd | Mnemonic::Stosd | Mnemonic::Lodsd | Mnemonic::Cmpsd | Mnemonic::Scasd => {
            4
        }
        Mnemonic::Movsq | Mnemonic::Stosq | Mnemonic::Lodsq | Mnemonic::Cmpsq | Mnemonic::Scasq => {
            8
        }
        _ => return Err(EmuException::InvalidOpcode),
    })
}

fn has_addr_size_override(bytes: &[u8], mode: CpuMode) -> bool {
    let mut i = 0usize;
    let mut seen = false;
    while i < bytes.len() {
        let b = bytes[i];
        let is_legacy_prefix = matches!(
            b,
            0xF0 | 0xF2 | 0xF3 // lock/rep
            | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 // segment overrides
            | 0x66 // operand-size override
            | 0x67 // address-size override
        );
        let is_rex = mode == CpuMode::Long && (0x40..=0x4F).contains(&b);
        if !(is_legacy_prefix || is_rex) {
            break;
        }
        if b == 0x67 {
            seen = true;
        }
        i += 1;
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyMem;
    impl MemoryBus for DummyMem {
        fn read_u8(&mut self, _paddr: u64) -> Result<u8, EmuException> {
            panic!("unexpected memory read in unit test")
        }

        fn write_u8(&mut self, _paddr: u64, _value: u8) -> Result<(), EmuException> {
            panic!("unexpected memory write in unit test")
        }
    }

    struct DummyPorts;
    impl PortIo for DummyPorts {
        fn in_u8(&mut self, _port: u16) -> u8 {
            0
        }

        fn out_u8(&mut self, _port: u16, _value: u8) {}
    }

    #[test]
    fn calc_effective_offset_rip_relative_does_not_double_add_next_ip() {
        // mov rax, qword ptr [rip+0x12345678]
        let bytes = [0x48, 0x8B, 0x05, 0x78, 0x56, 0x34, 0x12];
        let ip = 0x1000u64;

        let mut decoder = Decoder::with_ip(64, &bytes, ip, DecoderOptions::NONE);
        let instr = decoder.decode();
        assert_ne!(instr.code(), Code::INVALID);
        let next_ip = ip + instr.len() as u64;

        let cpu = CpuState::new(CpuMode::Long);
        let mut m = Machine::new(cpu, DummyMem, DummyPorts);
        let offset = m.calc_effective_offset(&instr, next_ip).expect("calc_effective_offset");

        assert_eq!(offset, next_ip.wrapping_add(0x12345678));
    }
}
