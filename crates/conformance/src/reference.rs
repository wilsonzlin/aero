#[cfg(all(target_arch = "x86_64", unix))]
mod host;

#[cfg(all(target_arch = "x86_64", unix))]
pub use host::ReferenceBackend;

#[cfg(not(all(target_arch = "x86_64", unix)))]
pub struct ReferenceBackend;

#[cfg(not(all(target_arch = "x86_64", unix)))]
impl ReferenceBackend {
    pub fn new() -> Result<Self, &'static str> {
        Err("host executor requires x86_64 + unix")
    }

    pub fn memory_base(&self) -> u64 {
        0
    }

    pub fn execute(&mut self, _case: &crate::corpus::TestCase) -> crate::ExecOutcome {
        crate::ExecOutcome {
            state: crate::CpuState::default(),
            memory: Vec::new(),
            fault: Some(crate::Fault::Unsupported(
                "host executor requires x86_64 + unix",
            )),
        }
    }
}
