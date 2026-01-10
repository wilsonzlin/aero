use std::collections::BTreeMap;

use crate::now_ms;
use crate::profiler::GpuBackendKind;

fn backend_kind_as_str(kind: GpuBackendKind) -> &'static str {
    match kind {
        GpuBackendKind::WebGpu => "webgpu",
        GpuBackendKind::WebGl2 => "webgl2",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuErrorSeverityKind {
    Info,
    Warning,
    Error,
    /// Fatal errors should stop rendering and require a restart/reload.
    Fatal,
}

impl GpuErrorSeverityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuErrorSeverityKind::Info => "Info",
            GpuErrorSeverityKind::Warning => "Warning",
            GpuErrorSeverityKind::Error => "Error",
            GpuErrorSeverityKind::Fatal => "Fatal",
        }
    }
}

/// Compatibility alias for the name used in the task description.
pub type GpuErrorSeverity = GpuErrorSeverityKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuErrorCategory {
    Init,
    DeviceLost,
    Surface,
    ShaderCompile,
    PipelineCreate,
    Validation,
    OutOfMemory,
    Unknown,
}

impl GpuErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuErrorCategory::Init => "Init",
            GpuErrorCategory::DeviceLost => "DeviceLost",
            GpuErrorCategory::Surface => "Surface",
            GpuErrorCategory::ShaderCompile => "ShaderCompile",
            GpuErrorCategory::PipelineCreate => "PipelineCreate",
            GpuErrorCategory::Validation => "Validation",
            GpuErrorCategory::OutOfMemory => "OutOfMemory",
            GpuErrorCategory::Unknown => "Unknown",
        }
    }
}

/// Structured GPU diagnostics event, designed to be forwarded to the main
/// thread verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuErrorEvent {
    pub time_ms: u64,
    pub backend_kind: GpuBackendKind,
    pub severity: GpuErrorSeverityKind,
    pub category: GpuErrorCategory,
    pub message: String,
    /// Optional structured detail payload.
    ///
    /// Kept as `String` values so we can remain dependency-free; callers can
    /// still attach structured JSON (as a string) if desired.
    pub details: Option<BTreeMap<String, String>>,
}

impl GpuErrorEvent {
    pub fn new(
        time_ms: u64,
        backend_kind: GpuBackendKind,
        severity: GpuErrorSeverityKind,
        category: GpuErrorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            time_ms,
            backend_kind,
            severity,
            category,
            message: message.into(),
            details: None,
        }
    }

    pub fn now(
        backend_kind: GpuBackendKind,
        severity: GpuErrorSeverityKind,
        category: GpuErrorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self::new(now_ms(), backend_kind, severity, category, message)
    }

    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    pub fn to_json(&self) -> String {
        // Manual JSON encoding keeps the crate dependency-free.
        let mut json = String::new();
        json.push('{');
        push_json_kv_u64(&mut json, "time_ms", self.time_ms, true);
        push_json_kv_str(
            &mut json,
            "backend_kind",
            backend_kind_as_str(self.backend_kind),
            false,
        );
        push_json_kv_str(&mut json, "severity", self.severity.as_str(), false);
        push_json_kv_str(&mut json, "category", self.category.as_str(), false);
        push_json_kv_string(&mut json, "message", &self.message, false);

        if let Some(details) = &self.details {
            json.push(',');
            json.push_str("\"details\":");
            json.push('{');
            let mut first = true;
            for (k, v) in details {
                if !first {
                    json.push(',');
                }
                first = false;
                json.push('"');
                json.push_str(&json_escape(k));
                json.push('"');
                json.push(':');
                json.push('"');
                json.push_str(&json_escape(v));
                json.push('"');
            }
            json.push('}');
        }

        json.push('}');
        json
    }
}

fn push_json_kv_u64(out: &mut String, key: &str, val: u64, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push('"');
    out.push(':');
    out.push_str(&val.to_string());
}

fn push_json_kv_str(out: &mut String, key: &str, val: &str, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push('"');
    out.push(':');
    out.push('"');
    out.push_str(val);
    out.push('"');
}

fn push_json_kv_string(out: &mut String, key: &str, val: &str, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push('"');
    out.push(':');
    out.push('"');
    out.push_str(&json_escape(val));
    out.push('"');
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Control characters must be escaped in JSON.
            c if c <= '\u{1F}' => {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "\\u{:04x}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_error_event_json_is_well_formed() {
        let event = GpuErrorEvent::new(
            123,
            GpuBackendKind::WebGpu,
            GpuErrorSeverityKind::Error,
            GpuErrorCategory::Surface,
            "message \"with\" escapes",
        )
        .with_detail("k", "v\nline");

        let json = event.to_json();
        assert!(json.contains("\"time_ms\":123"));
        assert!(json.contains("\"backend_kind\":\"webgpu\""));
        assert!(json.contains("\\\"with\\\""));
        assert!(json.contains("\"details\""));
        assert!(json.contains("\\n"));
    }
}
