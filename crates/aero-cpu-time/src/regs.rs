#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CpuRegs {
    pub rax: u64,
    pub rdx: u64,
    pub rcx: u64,
}
