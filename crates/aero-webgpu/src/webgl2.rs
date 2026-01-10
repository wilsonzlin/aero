use crate::BackendCaps;

/// WebGL2 fallback placeholder.
///
/// A real implementation should provide:
/// - GL context init + feature probing
/// - shader compilation and program caching
/// - framebuffer presentation
///
/// Until then we keep a stub so higher layers can compile and the backend
/// selection path is explicit.
pub struct WebGl2Stub {
    caps: BackendCaps,
    reason: String,
}

impl WebGl2Stub {
    pub(crate) fn new(reason: String) -> Self {
        Self {
            caps: crate::Backend::conservative_webgl2_caps(),
            reason,
        }
    }

    pub fn caps(&self) -> &BackendCaps {
        &self.caps
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}
