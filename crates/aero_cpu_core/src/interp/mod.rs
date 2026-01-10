pub mod x87;
pub mod decode;
pub mod string;

use crate::bus::Bus;
use crate::cpu::Cpu;

#[derive(Clone, Debug)]
pub struct DecodedInst {
    pub len: usize,
    pub kind: InstKind,
}

#[derive(Clone, Debug)]
pub enum InstKind {
    String(string::DecodedStringInst),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    InvalidOpcode(u8),
    TruncatedInstruction,
}

pub fn exec<B: Bus>(cpu: &mut Cpu, bus: &mut B, inst: &DecodedInst) -> Result<(), ExecError> {
    match &inst.kind {
        InstKind::String(s) => string::exec_string(cpu, bus, s),
    }
}
