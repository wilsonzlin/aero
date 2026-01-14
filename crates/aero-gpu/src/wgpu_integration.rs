//! Optional helpers for integrating with `wgpu`.
//!
//! The main crate remains dependency-free by default; enable the `wgpu` feature
//! to pull in the actual `wgpu` crate and use these conversions/handlers.

use crate::{
    now_ms, GpuBackendKind, GpuErrorCategory, GpuErrorEvent, GpuErrorSeverityKind, GpuSurfaceError,
};

fn classify_wgpu_error(err: &wgpu::Error) -> (GpuErrorSeverityKind, GpuErrorCategory) {
    match err {
        wgpu::Error::Validation { .. } => {
            (GpuErrorSeverityKind::Error, GpuErrorCategory::Validation)
        }
        wgpu::Error::OutOfMemory { .. } => {
            (GpuErrorSeverityKind::Fatal, GpuErrorCategory::OutOfMemory)
        }
        _ => (GpuErrorSeverityKind::Error, GpuErrorCategory::Unknown),
    }
}

pub fn wgpu_error_to_event(backend_kind: GpuBackendKind, err: &wgpu::Error) -> GpuErrorEvent {
    let message = err.to_string();
    let (severity, category) = classify_wgpu_error(err);
    GpuErrorEvent::new(now_ms(), backend_kind, severity, category, message)
        .with_detail("wgpu_debug", format!("{err:?}"))
}

/// Registers a `wgpu` uncaptured error handler and forwards each error as a
/// `GpuErrorEvent`.
///
/// Note: this is intended for the WASM GPU worker and should forward the
/// resulting events over IPC to the main thread.
pub fn register_wgpu_uncaptured_error_handler(
    device: &wgpu::Device,
    backend_kind: GpuBackendKind,
    emit_event: impl Fn(GpuErrorEvent) + Send + 'static,
) {
    device.on_uncaptured_error(Box::new(move |err| {
        emit_event(wgpu_error_to_event(backend_kind, &err));
    }));
}

impl From<wgpu::SurfaceError> for GpuSurfaceError {
    fn from(value: wgpu::SurfaceError) -> Self {
        match value {
            wgpu::SurfaceError::Lost => GpuSurfaceError::Lost,
            wgpu::SurfaceError::Outdated => GpuSurfaceError::Outdated,
            wgpu::SurfaceError::Timeout => GpuSurfaceError::Timeout,
            wgpu::SurfaceError::OutOfMemory => GpuSurfaceError::OutOfMemory,
        }
    }
}

#[cfg(all(test, feature = "wgpu"))]
mod tests {
    use super::*;

    #[test]
    fn classify_wgpu_error_maps_by_variant() {
        let oom = wgpu::Error::OutOfMemory {
            source: Box::new(std::io::Error::other("oom")),
        };
        assert_eq!(
            classify_wgpu_error(&oom),
            (GpuErrorSeverityKind::Fatal, GpuErrorCategory::OutOfMemory)
        );

        let validation = wgpu::Error::Validation {
            source: Box::new(std::io::Error::other("validation")),
            description: "validation error".into(),
        };
        assert_eq!(
            classify_wgpu_error(&validation),
            (GpuErrorSeverityKind::Error, GpuErrorCategory::Validation)
        );

        let internal = wgpu::Error::Internal {
            source: Box::new(std::io::Error::other("internal")),
            description: "internal error".into(),
        };
        assert_eq!(
            classify_wgpu_error(&internal),
            (GpuErrorSeverityKind::Error, GpuErrorCategory::Unknown)
        );
    }
}
