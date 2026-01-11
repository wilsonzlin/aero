use crate::{
    stats::GpuStats, GpuBackendKind, GpuErrorCategory, GpuErrorEvent, GpuErrorSeverityKind,
};

fn backend_kind_as_str(kind: GpuBackendKind) -> &'static str {
    match kind {
        GpuBackendKind::WebGpu => "webgpu",
        GpuBackendKind::WebGl2 => "webgl2",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    Running,
    Recovering,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendAvailability {
    pub webgpu: bool,
    pub webgl2: bool,
}

impl BackendAvailability {
    pub fn only_webgpu() -> Self {
        Self {
            webgpu: true,
            webgl2: false,
        }
    }

    pub fn only_webgl2() -> Self {
        Self {
            webgpu: false,
            webgl2: true,
        }
    }

    pub fn both() -> Self {
        Self {
            webgpu: true,
            webgl2: true,
        }
    }

    pub fn is_available(self, backend: GpuBackendKind) -> bool {
        match backend {
            GpuBackendKind::WebGpu => self.webgpu,
            GpuBackendKind::WebGl2 => self.webgl2,
        }
    }

    pub fn fallback_for(self, backend: GpuBackendKind) -> Option<GpuBackendKind> {
        match backend {
            GpuBackendKind::WebGpu if self.webgl2 => Some(GpuBackendKind::WebGl2),
            GpuBackendKind::WebGl2 if self.webgpu => Some(GpuBackendKind::WebGpu),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Recovered { backend_kind: GpuBackendKind },
    Failed,
}

/// Device-lost recovery state machine.
///
/// This is intentionally generic: callers provide the actual re-init function
/// for a given backend. The state machine enforces the required ordering:
/// 1) retry the current backend, 2) fallback to the other backend if available.
#[derive(Debug)]
pub struct GpuRecoveryMachine {
    state: RecoveryState,
    backend_kind: GpuBackendKind,
    availability: BackendAvailability,
}

impl GpuRecoveryMachine {
    pub fn new(backend_kind: GpuBackendKind, availability: BackendAvailability) -> Self {
        Self {
            state: RecoveryState::Running,
            backend_kind,
            availability,
        }
    }

    pub fn state(&self) -> RecoveryState {
        self.state
    }

    pub fn backend_kind(&self) -> GpuBackendKind {
        self.backend_kind
    }

    pub fn availability(&self) -> BackendAvailability {
        self.availability
    }

    pub fn handle_device_lost(
        &mut self,
        time_ms: u64,
        stats: &GpuStats,
        mut emit_event: impl FnMut(GpuErrorEvent),
        mut reinit: impl FnMut(GpuBackendKind) -> Result<(), String>,
    ) -> RecoveryOutcome {
        if self.state == RecoveryState::Failed {
            emit_event(GpuErrorEvent::new(
                time_ms,
                self.backend_kind,
                GpuErrorSeverityKind::Fatal,
                GpuErrorCategory::DeviceLost,
                "GPU recovery requested while already failed",
            ));
            return RecoveryOutcome::Failed;
        }

        self.state = RecoveryState::Recovering;

        let current = self.backend_kind;
        if !self.availability.is_available(current) {
            emit_event(GpuErrorEvent::new(
                time_ms,
                current,
                GpuErrorSeverityKind::Error,
                GpuErrorCategory::Init,
                format!("Current backend {:?} is not available", current),
            ));
        }

        // Attempt re-init on current backend first.
        stats.inc_recoveries_attempted();
        emit_event(GpuErrorEvent::new(
            time_ms,
            current,
            GpuErrorSeverityKind::Info,
            GpuErrorCategory::DeviceLost,
            format!("Attempting GPU recovery on {:?}", current),
        ));

        match reinit(current) {
            Ok(()) => {
                stats.inc_recoveries_succeeded();
                self.state = RecoveryState::Running;
                emit_event(GpuErrorEvent::new(
                    time_ms,
                    current,
                    GpuErrorSeverityKind::Info,
                    GpuErrorCategory::DeviceLost,
                    format!("GPU recovery succeeded on {:?}", current),
                ));
                return RecoveryOutcome::Recovered {
                    backend_kind: current,
                };
            }
            Err(err) => {
                emit_event(
                    GpuErrorEvent::new(
                        time_ms,
                        current,
                        GpuErrorSeverityKind::Warning,
                        GpuErrorCategory::DeviceLost,
                        format!("GPU recovery failed on {:?}: {}", current, err),
                    )
                    .with_detail("attempt_backend", backend_kind_as_str(current)),
                );
            }
        }

        // If that failed, try the fallback backend if available.
        if let Some(fallback) = self.availability.fallback_for(current) {
            stats.inc_recoveries_attempted();
            emit_event(GpuErrorEvent::new(
                time_ms,
                fallback,
                GpuErrorSeverityKind::Info,
                GpuErrorCategory::DeviceLost,
                format!("Attempting GPU recovery fallback to {:?}", fallback),
            ));

            match reinit(fallback) {
                Ok(()) => {
                    stats.inc_recoveries_succeeded();
                    self.backend_kind = fallback;
                    self.state = RecoveryState::Running;
                    emit_event(GpuErrorEvent::new(
                        time_ms,
                        fallback,
                        GpuErrorSeverityKind::Info,
                        GpuErrorCategory::DeviceLost,
                        format!("GPU recovery fallback succeeded on {:?}", fallback),
                    ));
                    return RecoveryOutcome::Recovered {
                        backend_kind: fallback,
                    };
                }
                Err(err) => {
                    emit_event(GpuErrorEvent::new(
                        time_ms,
                        fallback,
                        GpuErrorSeverityKind::Error,
                        GpuErrorCategory::DeviceLost,
                        format!("GPU recovery fallback failed on {:?}: {}", fallback, err),
                    ));
                }
            }
        }

        self.state = RecoveryState::Failed;
        emit_event(GpuErrorEvent::new(
            time_ms,
            current,
            GpuErrorSeverityKind::Fatal,
            GpuErrorCategory::DeviceLost,
            "GPU recovery exhausted all backends; entering Failed state",
        ));
        RecoveryOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_falls_back_to_other_backend() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());

        let mut events = Vec::new();
        let outcome = machine.handle_device_lost(
            42,
            &stats,
            |e| events.push(e),
            |backend| match backend {
                GpuBackendKind::WebGpu => Err("webgpu init failed".to_string()),
                GpuBackendKind::WebGl2 => Ok(()),
            },
        );

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                backend_kind: GpuBackendKind::WebGl2
            }
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGl2);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 1);

        assert!(events
            .iter()
            .any(|e| e.message.contains("fallback succeeded")));
    }

    #[test]
    fn recovery_enters_failed_state_when_all_backends_fail() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());
        let mut events = Vec::new();

        let outcome = machine.handle_device_lost(
            1,
            &stats,
            |e| events.push(e),
            |_backend| Err("nope".to_string()),
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);
        assert!(events
            .iter()
            .any(|e| e.severity == GpuErrorSeverityKind::Fatal));
    }
}
