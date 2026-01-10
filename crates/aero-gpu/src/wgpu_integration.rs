//! Optional helpers for integrating with `wgpu`.
//!
//! The main crate remains dependency-free by default; enable the `wgpu` feature
//! to pull in the actual `wgpu` crate and use these conversions/handlers.

use crate::{
    now_ms, GpuBackendKind, GpuErrorCategory, GpuErrorEvent, GpuErrorSeverityKind, GpuSurfaceError,
};

fn classify_wgpu_error(message: &str) -> (GpuErrorSeverityKind, GpuErrorCategory) {
    // `wgpu::Error` does not expose a stable machine-readable code across
    // versions; keep this robust by classifying via the formatted message.
    let msg = message.to_ascii_lowercase();
    if msg.contains("outofmemory") || msg.contains("out of memory") {
        (GpuErrorSeverityKind::Fatal, GpuErrorCategory::OutOfMemory)
    } else if msg.contains("validation") {
        (GpuErrorSeverityKind::Error, GpuErrorCategory::Validation)
    } else if msg.contains("device lost") || msg.contains("devicelost") {
        (GpuErrorSeverityKind::Error, GpuErrorCategory::DeviceLost)
    } else {
        (GpuErrorSeverityKind::Error, GpuErrorCategory::Unknown)
    }
}

pub fn wgpu_error_to_event(backend_kind: GpuBackendKind, err: &wgpu::Error) -> GpuErrorEvent {
    let message = err.to_string();
    let (severity, category) = classify_wgpu_error(&message);
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
