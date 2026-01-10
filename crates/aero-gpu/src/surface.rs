use std::collections::VecDeque;

use crate::{
    stats::GpuStats, GpuBackendKind, GpuErrorCategory, GpuErrorEvent, GpuErrorSeverityKind,
};

/// Minimal surface error set used by the present/reconfigure recovery path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuSurfaceError {
    Lost,
    Outdated,
    Timeout,
    OutOfMemory,
    Other(String),
}

impl GpuSurfaceError {
    fn severity_and_category(&self) -> (GpuErrorSeverityKind, GpuErrorCategory) {
        match self {
            GpuSurfaceError::OutOfMemory => (GpuErrorSeverityKind::Fatal, GpuErrorCategory::OutOfMemory),
            GpuSurfaceError::Lost | GpuSurfaceError::Outdated => (GpuErrorSeverityKind::Warning, GpuErrorCategory::Surface),
            GpuSurfaceError::Timeout => (GpuErrorSeverityKind::Warning, GpuErrorCategory::Surface),
            GpuSurfaceError::Other(_) => (GpuErrorSeverityKind::Error, GpuErrorCategory::Surface),
        }
    }
}

pub trait SurfaceFrame {
    fn present(self);
}

pub trait SurfaceProvider {
    type Frame: SurfaceFrame;

    fn acquire_frame(&mut self) -> Result<Self::Frame, GpuSurfaceError>;
    fn reconfigure(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented,
    Dropped,
    FatalOutOfMemory,
}

/// Convenience wrapper that tracks whether rendering should continue.
///
/// `OutOfMemory` during present is treated as fatal and permanently disables
/// further presents.
#[derive(Debug)]
pub struct GpuPresenter {
    backend_kind: GpuBackendKind,
    stats: GpuStats,
    rendering_enabled: bool,
}

impl GpuPresenter {
    pub fn new(backend_kind: GpuBackendKind) -> Self {
        Self {
            backend_kind,
            stats: GpuStats::new(),
            rendering_enabled: true,
        }
    }

    pub fn backend_kind(&self) -> GpuBackendKind {
        self.backend_kind
    }

    pub fn stats(&self) -> &GpuStats {
        &self.stats
    }

    pub fn rendering_enabled(&self) -> bool {
        self.rendering_enabled
    }

    pub fn get_gpu_stats(&self) -> String {
        self.stats.get_gpu_stats()
    }

    pub fn present<S: SurfaceProvider>(
        &mut self,
        surface: &mut S,
        time_ms: u64,
        emit_event: impl FnMut(GpuErrorEvent),
    ) -> PresentOutcome {
        if !self.rendering_enabled {
            return PresentOutcome::Dropped;
        }

        let outcome = present_with_retry(
            surface,
            time_ms,
            self.backend_kind,
            &self.stats,
            emit_event,
        );
        if matches!(outcome, PresentOutcome::FatalOutOfMemory) {
            self.rendering_enabled = false;
        }
        outcome
    }
}

/// Present a frame with a single automatic surface reconfigure retry.
///
/// This implements the required behavior:
/// - `Lost`/`Outdated`: reconfigure and retry once.
/// - `OutOfMemory`: emit fatal event and return `FatalOutOfMemory`.
pub fn present_with_retry<S: SurfaceProvider>(
    surface: &mut S,
    time_ms: u64,
    backend_kind: GpuBackendKind,
    stats: &GpuStats,
    mut emit_event: impl FnMut(GpuErrorEvent),
) -> PresentOutcome {
    let mut attempt = 0u8;

    loop {
        attempt += 1;
        stats.inc_presents_attempted();

        match surface.acquire_frame() {
            Ok(frame) => {
                frame.present();
                stats.inc_presents_succeeded();
                return PresentOutcome::Presented;
            }
            Err(err) => {
                let (severity, category) = err.severity_and_category();
                emit_event(GpuErrorEvent::new(
                    time_ms,
                    backend_kind,
                    severity,
                    category,
                    format!("Surface present error: {:?}", err),
                ));

                match err {
                    GpuSurfaceError::Lost | GpuSurfaceError::Outdated if attempt == 1 => {
                        surface.reconfigure();
                        stats.inc_surface_reconfigures();
                        // Retry once.
                        continue;
                    }
                    GpuSurfaceError::OutOfMemory => return PresentOutcome::FatalOutOfMemory,
                    _ => return PresentOutcome::Dropped,
                }
            }
        }
    }
}

/// A deterministic test surface that can simulate a sequence of acquire results.
///
/// This doubles as a "debug hook" because callers can inject a `Lost`/`Outdated`
/// error on demand without crashing the render loop.
#[derive(Debug, Default)]
pub struct SimulatedSurface {
    outcomes: VecDeque<Result<(), GpuSurfaceError>>,
    pub reconfigure_calls: u64,
    pub present_calls: u64,
}

impl SimulatedSurface {
    pub fn new(outcomes: impl IntoIterator<Item = Result<(), GpuSurfaceError>>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            reconfigure_calls: 0,
            present_calls: 0,
        }
    }

    pub fn push_outcome(&mut self, outcome: Result<(), GpuSurfaceError>) {
        self.outcomes.push_back(outcome);
    }
}

#[derive(Debug)]
pub struct SimulatedFrame {
    surface: *mut SimulatedSurface,
}

impl SurfaceFrame for SimulatedFrame {
    fn present(self) {
        // SAFETY: `SimulatedFrame` is only ever constructed from a valid mutable
        // reference in `SimulatedSurface::acquire_frame` and consumed immediately by
        // the caller via `present_with_retry`.
        unsafe {
            (*self.surface).present_calls += 1;
        }
    }
}

impl SurfaceProvider for SimulatedSurface {
    type Frame = SimulatedFrame;

    fn acquire_frame(&mut self) -> Result<Self::Frame, GpuSurfaceError> {
        match self.outcomes.pop_front().unwrap_or(Ok(())) {
            Ok(()) => Ok(SimulatedFrame {
                surface: self as *mut SimulatedSurface,
            }),
            Err(err) => Err(err),
        }
    }

    fn reconfigure(&mut self) {
        self.reconfigure_calls += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GpuErrorCategory, GpuErrorSeverityKind};

    #[test]
    fn present_lost_triggers_reconfigure_and_retry() {
        let stats = GpuStats::new();
        let mut surface = SimulatedSurface::new([Err(GpuSurfaceError::Lost), Ok(())]);
        let mut events = Vec::new();
        let outcome = present_with_retry(
            &mut surface,
            1,
            GpuBackendKind::WebGpu,
            &stats,
            |e| events.push(e),
        );

        assert_eq!(outcome, PresentOutcome::Presented);
        assert_eq!(surface.reconfigure_calls, 1);
        assert_eq!(surface.present_calls, 1);

        let snap = stats.snapshot();
        assert_eq!(snap.presents_attempted, 2);
        assert_eq!(snap.presents_succeeded, 1);
        assert_eq!(snap.surface_reconfigures, 1);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Warning);
        assert_eq!(events[0].category, GpuErrorCategory::Surface);
    }

    #[test]
    fn present_out_of_memory_is_fatal() {
        let stats = GpuStats::new();
        let mut surface = SimulatedSurface::new([Err(GpuSurfaceError::OutOfMemory)]);
        let mut events = Vec::new();

        let outcome = present_with_retry(
            &mut surface,
            1,
            GpuBackendKind::WebGpu,
            &stats,
            |e| events.push(e),
        );

        assert_eq!(outcome, PresentOutcome::FatalOutOfMemory);
        assert_eq!(surface.reconfigure_calls, 0);
        assert_eq!(surface.present_calls, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, GpuErrorSeverityKind::Fatal);
        assert_eq!(events[0].category, GpuErrorCategory::OutOfMemory);
    }

    #[test]
    fn presenter_disables_rendering_after_out_of_memory() {
        let mut presenter = GpuPresenter::new(GpuBackendKind::WebGpu);
        let mut surface = SimulatedSurface::new([Err(GpuSurfaceError::OutOfMemory), Ok(())]);
        let mut events = Vec::new();

        let outcome = presenter.present(&mut surface, 1, |e| events.push(e));
        assert_eq!(outcome, PresentOutcome::FatalOutOfMemory);
        assert!(!presenter.rendering_enabled());

        // Subsequent presents are dropped without panicking.
        let outcome = presenter.present(&mut surface, 2, |_e| {});
        assert_eq!(outcome, PresentOutcome::Dropped);
    }
}
