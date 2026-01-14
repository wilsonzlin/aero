#[cfg(all(target_arch = "x86_64", unix))]
mod host;

#[cfg(feature = "qemu-reference")]
mod qemu;

use crate::corpus::TestCase;
use crate::ExecOutcome;
#[cfg(not(all(target_arch = "x86_64", unix)))]
use crate::{CpuState, Fault};

#[cfg_attr(not(all(target_arch = "x86_64", unix)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
    Host,
    #[cfg(feature = "qemu-reference")]
    Qemu,
}

pub struct ReferenceBackend {
    kind: BackendKind,
    #[cfg(all(target_arch = "x86_64", unix))]
    host: Option<host::HostReferenceBackend>,
    #[cfg(feature = "qemu-reference")]
    qemu: Option<qemu::QemuReferenceBackend>,
}

impl ReferenceBackend {
    pub fn new() -> Result<Self, &'static str> {
        let requested = std::env::var("AERO_CONFORMANCE_REFERENCE").ok();

        #[cfg(feature = "qemu-reference")]
        let qemu_available = qemu::QemuReferenceBackend::available();
        #[cfg(not(feature = "qemu-reference"))]
        let qemu_available = false;

        // If QEMU is explicitly requested and available, prefer it.
        if matches!(requested.as_deref(), Some("qemu")) && qemu_available {
            #[cfg(feature = "qemu-reference")]
            {
                return Ok(Self {
                    kind: BackendKind::Qemu,
                    #[cfg(all(target_arch = "x86_64", unix))]
                    host: None,
                    qemu: Some(qemu::QemuReferenceBackend::new()?),
                });
            }
        }

        // Default to the host backend when available.
        #[cfg(all(target_arch = "x86_64", unix))]
        {
            return Ok(Self {
                kind: BackendKind::Host,
                host: Some(host::HostReferenceBackend::new()?),
                #[cfg(feature = "qemu-reference")]
                qemu: None,
            });
        }

        // On non-x86_64/unix hosts, fall back to QEMU if compiled in and available.
        #[cfg(all(feature = "qemu-reference", not(all(target_arch = "x86_64", unix))))]
        {
            if qemu_available {
                return Ok(Self {
                    kind: BackendKind::Qemu,
                    #[cfg(all(target_arch = "x86_64", unix))]
                    host: None,
                    qemu: Some(qemu::QemuReferenceBackend::new()?),
                });
            }
        }

        #[cfg(not(all(target_arch = "x86_64", unix)))]
        Err("no usable reference backend (host requires x86_64+unix; qemu requires feature + qemu-system-*)")
    }

    #[cfg(feature = "qemu-reference")]
    pub(crate) fn is_qemu(&self) -> bool {
        matches!(self.kind, BackendKind::Qemu)
    }

    pub fn memory_base(&self) -> u64 {
        match self.kind {
            BackendKind::Host => {
                #[cfg(all(target_arch = "x86_64", unix))]
                {
                    self.host
                        .as_ref()
                        .expect("host backend present")
                        .memory_base()
                }
                #[cfg(not(all(target_arch = "x86_64", unix)))]
                {
                    0
                }
            }
            #[cfg(feature = "qemu-reference")]
            BackendKind::Qemu => self
                .qemu
                .as_ref()
                .expect("qemu backend present")
                .memory_base(),
        }
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        match self.kind {
            BackendKind::Host => {
                #[cfg(all(target_arch = "x86_64", unix))]
                {
                    self.host
                        .as_mut()
                        .expect("host backend present")
                        .execute(case)
                }
                #[cfg(not(all(target_arch = "x86_64", unix)))]
                {
                    let _ = case;
                    ExecOutcome {
                        state: CpuState::default(),
                        memory: Vec::new(),
                        fault: Some(Fault::Unsupported("host executor requires x86_64 + unix")),
                    }
                }
            }
            #[cfg(feature = "qemu-reference")]
            BackendKind::Qemu => self
                .qemu
                .as_mut()
                .expect("qemu backend present")
                .execute(case),
        }
    }
}
