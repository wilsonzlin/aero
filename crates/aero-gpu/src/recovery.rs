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
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            42,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                match backend {
                    GpuBackendKind::WebGpu => Err("webgpu init failed".to_string()),
                    GpuBackendKind::WebGl2 => Ok(()),
                }
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

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        );

        // Event ordering should be deterministic:
        // 1) Attempt current, 2) fail current, 3) attempt fallback, 4) fallback succeeds.
        assert_eq!(events.len(), 4, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 42));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "webgpu init failed"
            )
        );
        assert_eq!(
            events[1]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[2].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[3].message,
            format!(
                "GPU recovery fallback succeeded on {:?}",
                GpuBackendKind::WebGl2
            )
        );
    }

    #[test]
    fn recovery_falls_back_from_webgl2_to_webgpu() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGl2, BackendAvailability::both());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            43,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                match backend {
                    GpuBackendKind::WebGl2 => Err("webgl2 init failed".to_string()),
                    GpuBackendKind::WebGpu => Ok(()),
                }
            },
        );

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                backend_kind: GpuBackendKind::WebGpu
            }
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 1);

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGl2, GpuBackendKind::WebGpu]
        );

        // Event ordering should be deterministic:
        // 1) Attempt current, 2) fail current, 3) attempt fallback, 4) fallback succeeds.
        assert_eq!(events.len(), 4, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 43));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGl2)
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[1].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGl2,
                "webgl2 init failed"
            )
        );
        assert_eq!(
            events[1]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgl2")
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGpu
            )
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[3].message,
            format!(
                "GPU recovery fallback succeeded on {:?}",
                GpuBackendKind::WebGpu
            )
        );
    }

    #[test]
    fn recovery_emits_init_error_when_current_webgl2_marked_unavailable_but_still_attempts_it() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGl2, BackendAvailability::only_webgpu());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            6,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Ok(())
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

        assert_eq!(attempted, vec![GpuBackendKind::WebGl2]);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 1);
        assert_eq!(snap.recoveries_succeeded, 1);

        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.time_ms == 6));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGl2)
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[2].message,
            format!("GPU recovery succeeded on {:?}", GpuBackendKind::WebGl2)
        );
    }

    #[test]
    fn recovery_emits_init_error_when_current_backend_marked_unavailable_but_still_attempts_it() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::only_webgl2());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            5,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Ok(())
            },
        );

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                backend_kind: GpuBackendKind::WebGpu
            }
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        // Even though the backend is marked unavailable, we should still attempt reinit on it first.
        assert_eq!(attempted, vec![GpuBackendKind::WebGpu]);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 1);
        assert_eq!(snap.recoveries_succeeded, 1);

        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.time_ms == 5));
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGpu
            )
        );
        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );
        assert_eq!(events[2].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            format!("GPU recovery succeeded on {:?}", GpuBackendKind::WebGpu)
        );
    }

    #[test]
    fn recovery_falls_back_even_when_current_webgl2_marked_unavailable() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGl2, BackendAvailability::only_webgpu());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            8,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                match backend {
                    GpuBackendKind::WebGl2 => Err("webgl2 init failed".to_string()),
                    GpuBackendKind::WebGpu => Ok(()),
                }
            },
        );

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                backend_kind: GpuBackendKind::WebGpu
            }
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGl2, GpuBackendKind::WebGpu]
        );

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 1);

        // Event ordering should be deterministic:
        // 1) Init error (unavailable current), 2) attempt current, 3) fail current,
        // 4) attempt fallback, 5) fallback succeeds.
        assert_eq!(events.len(), 5, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 8));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGl2)
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[2].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGl2,
                "webgl2 init failed"
            )
        );
        assert_eq!(
            events[2]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgl2")
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[3].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGpu
            )
        );

        assert_eq!(events[4].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[4].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[4].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[4].message,
            format!(
                "GPU recovery fallback succeeded on {:?}",
                GpuBackendKind::WebGpu
            )
        );
    }

    #[test]
    fn recovery_falls_back_even_when_current_backend_marked_unavailable() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::only_webgl2());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            7,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                match backend {
                    GpuBackendKind::WebGpu => Err("webgpu init failed".to_string()),
                    GpuBackendKind::WebGl2 => Ok(()),
                }
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

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        );

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 1);

        // Event ordering should be deterministic:
        // 1) Init error (unavailable current), 2) attempt current, 3) fail current,
        // 4) attempt fallback, 5) fallback succeeds.
        assert_eq!(events.len(), 5, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 7));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGpu
            )
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "webgpu init failed"
            )
        );
        assert_eq!(
            events[2]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[3].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[4].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[4].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[4].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[4].message,
            format!(
                "GPU recovery fallback succeeded on {:?}",
                GpuBackendKind::WebGl2
            )
        );
    }

    #[test]
    fn recovery_marked_unavailable_enters_failed_when_current_and_fallback_attempts_fail() {
        let stats = GpuStats::new();
        // Current backend (WebGpu) is marked unavailable, but fallback (WebGl2) is available.
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::only_webgl2());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            11,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Err(format!("{:?} init failed", backend))
            },
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        );

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 0);

        // Event ordering should be deterministic:
        // 1) Init error (unavailable current), 2) attempt current, 3) fail current,
        // 4) attempt fallback, 5) fail fallback, 6) fatal.
        assert_eq!(events.len(), 6, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 11));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGpu
            )
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "WebGpu init failed"
            )
        );
        assert_eq!(
            events[2]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[3].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[4].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[4].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[4].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[4].message,
            format!(
                "GPU recovery fallback failed on {:?}: {}",
                GpuBackendKind::WebGl2,
                "WebGl2 init failed"
            )
        );

        assert_eq!(events[5].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[5].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[5].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[5].message,
            "GPU recovery exhausted all backends; entering Failed state"
        );
    }

    #[test]
    fn recovery_marked_unavailable_with_no_fallback_still_attempts_current_then_fails() {
        let stats = GpuStats::new();
        // Degenerate case: neither backend is considered available, but the machine still tries the
        // current backend once before giving up.
        let mut machine = GpuRecoveryMachine::new(
            GpuBackendKind::WebGpu,
            BackendAvailability {
                webgpu: false,
                webgl2: false,
            },
        );

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            12,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Err("init failed".to_string())
            },
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(attempted, vec![GpuBackendKind::WebGpu]);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 1);
        assert_eq!(snap.recoveries_succeeded, 0);

        // Event ordering should be deterministic:
        // 1) Init error (unavailable current), 2) attempt current, 3) fail current, 4) fatal.
        assert_eq!(events.len(), 4, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 12));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[0].category, GpuErrorCategory::Init);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!(
                "Current backend {:?} is not available",
                GpuBackendKind::WebGpu
            )
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "init failed"
            )
        );
        assert_eq!(
            events[2]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[3].message,
            "GPU recovery exhausted all backends; entering Failed state"
        );
    }

    #[test]
    fn recovery_enters_failed_state_when_current_and_fallback_backends_fail() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());
        let mut events = Vec::new();
        let mut attempted = Vec::new();

        let outcome = machine.handle_device_lost(
            1,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Err(format!("{:?} init failed", backend))
            },
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(
            attempted,
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        );

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 0);

        // Event ordering should be deterministic:
        // 1) Attempt current, 2) fail current, 3) attempt fallback, 4) fail fallback, 5) fatal.
        assert_eq!(events.len(), 5, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 1));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "WebGpu init failed"
            )
        );
        assert_eq!(
            events[1]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[2].message,
            format!(
                "Attempting GPU recovery fallback to {:?}",
                GpuBackendKind::WebGl2
            )
        );

        assert_eq!(events[3].severity, GpuErrorSeverityKind::Error);
        assert_eq!(events[3].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[3].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[3].message,
            format!(
                "GPU recovery fallback failed on {:?}: {}",
                GpuBackendKind::WebGl2,
                "WebGl2 init failed"
            )
        );

        assert_eq!(events[4].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[4].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[4].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[4].message,
            "GPU recovery exhausted all backends; entering Failed state"
        );
    }

    #[test]
    fn recovery_enters_failed_state_when_no_fallback_backend_is_available() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::only_webgpu());
        let mut events = Vec::new();
        let mut attempted = Vec::new();

        let outcome = machine.handle_device_lost(
            2,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Err("webgpu init failed".to_string())
            },
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(attempted, vec![GpuBackendKind::WebGpu]);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 1);
        assert_eq!(snap.recoveries_succeeded, 0);

        // Event ordering should be deterministic:
        // 1) Attempt current, 2) fail current, 3) fatal (no fallback).
        assert_eq!(events.len(), 3, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 2));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!(
                "GPU recovery failed on {:?}: {}",
                GpuBackendKind::WebGpu,
                "webgpu init failed"
            )
        );
        assert_eq!(
            events[1]
                .details
                .as_ref()
                .and_then(|d| d.get("attempt_backend"))
                .map(String::as_str),
            Some("webgpu")
        );

        assert_eq!(events[2].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[2].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[2].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[2].message,
            "GPU recovery exhausted all backends; entering Failed state"
        );
    }

    #[test]
    fn recovery_succeeds_on_current_backend_without_using_fallback() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());

        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            50,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Ok(())
            },
        );

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                backend_kind: GpuBackendKind::WebGpu
            }
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGpu);

        assert_eq!(attempted, vec![GpuBackendKind::WebGpu]);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 1);
        assert_eq!(snap.recoveries_succeeded, 1);

        // Event ordering should be deterministic: attempt then success.
        assert_eq!(events.len(), 2, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 50));

        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGpu)
        );
        assert!(events[0].details.is_none());

        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[1].message,
            format!("GPU recovery succeeded on {:?}", GpuBackendKind::WebGpu)
        );
        assert!(events[1].details.is_none());
    }

    #[test]
    fn recovery_does_not_attempt_reinit_after_reaching_failed_state() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());

        // First device-lost event: both backends fail, transitioning the machine to Failed.
        let _outcome =
            machine.handle_device_lost(100, &stats, |_e| {}, |_backend| Err("nope".to_string()));
        assert_eq!(machine.state(), RecoveryState::Failed);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 0);

        // Second device-lost event: should short-circuit without attempting reinit or bumping stats.
        let mut events = Vec::new();
        let outcome = machine.handle_device_lost(
            101,
            &stats,
            |e| events.push(e),
            |_backend| panic!("reinit should not be called once recovery has entered Failed"),
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 2);
        assert_eq!(snap.recoveries_succeeded, 0);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_ms, 101);
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            "GPU recovery requested while already failed"
        );
    }

    #[test]
    fn recovery_stats_accumulate_across_multiple_device_lost_events() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());

        // First device-lost: fail WebGPU, recover via WebGL2.
        let _outcome = machine.handle_device_lost(
            200,
            &stats,
            |_e| {},
            |backend| match backend {
                GpuBackendKind::WebGpu => Err("webgpu init failed".to_string()),
                GpuBackendKind::WebGl2 => Ok(()),
            },
        );
        assert_eq!(machine.state(), RecoveryState::Running);
        assert_eq!(machine.backend_kind(), GpuBackendKind::WebGl2);

        // Second device-lost: recover on current backend (WebGL2) directly.
        let mut events = Vec::new();
        let mut attempted = Vec::new();
        let outcome = machine.handle_device_lost(
            201,
            &stats,
            |e| events.push(e),
            |backend| {
                attempted.push(backend);
                Ok(())
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

        assert_eq!(attempted, vec![GpuBackendKind::WebGl2]);

        // Stats should accumulate across calls: (2 attempts + 1 success) from the first device-lost,
        // then (1 attempt + 1 success) from the second.
        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 3);
        assert_eq!(snap.recoveries_succeeded, 2);

        // Second-call event ordering should be deterministic: attempt then success.
        assert_eq!(events.len(), 2, "{events:#?}");
        assert!(events.iter().all(|e| e.time_ms == 201));
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[0].message,
            format!("Attempting GPU recovery on {:?}", GpuBackendKind::WebGl2)
        );
        assert_eq!(events[1].severity, GpuErrorSeverityKind::Info);
        assert_eq!(events[1].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[1].backend_kind, GpuBackendKind::WebGl2);
        assert_eq!(
            events[1].message,
            format!("GPU recovery succeeded on {:?}", GpuBackendKind::WebGl2)
        );
    }

    #[test]
    fn recovery_requested_while_already_failed_emits_fatal_without_attempting_reinit() {
        let stats = GpuStats::new();
        let mut machine =
            GpuRecoveryMachine::new(GpuBackendKind::WebGpu, BackendAvailability::both());
        machine.state = RecoveryState::Failed;

        let mut events = Vec::new();
        let outcome = machine.handle_device_lost(
            9,
            &stats,
            |e| events.push(e),
            |_backend| panic!("reinit should not be called while already Failed"),
        );

        assert_eq!(outcome, RecoveryOutcome::Failed);
        assert_eq!(machine.state(), RecoveryState::Failed);

        let snap = stats.snapshot();
        assert_eq!(snap.recoveries_attempted, 0);
        assert_eq!(snap.recoveries_succeeded, 0);

        assert_eq!(events.len(), 1);
        assert!(events.iter().all(|e| e.time_ms == 9));
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[0].category, GpuErrorCategory::DeviceLost);
        assert_eq!(events[0].backend_kind, GpuBackendKind::WebGpu);
        assert_eq!(
            events[0].message,
            "GPU recovery requested while already failed"
        );
    }
}
