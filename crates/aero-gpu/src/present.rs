//! Presentation policy (color space + alpha mode) shared by all presenter backends.
//!
//! ## Goals
//! - **Match Windows swapchain expectations**: the final scanout surface is effectively opaque
//!   and gamma-correct for an sRGB display.
//! - **Keep rendering math correct**: the emulator's GPU pipeline should operate in **linear**
//!   space; presentation is responsible for encoding to sRGB when required.
//! - **Make WebGPU and WebGL2 match**: avoid backend-specific "looks right" hacks.
//!
//! ## Policy (default)
//! - **Input framebuffer encoding:** `RGBA8` *linear* (`rgba8unorm`).
//! - **Presented output encoding:** **sRGB** when possible, with a deterministic shader fallback.
//! - **Presented alpha mode:** **opaque** (Windows desktop swapchains are effectively opaque).
//!
//! These defaults are chosen to match Windows 7-era D3D behavior for the *final* output.
//! The guest may use alpha internally, but the final scanout should not accidentally blend
//! with the web page background.
//!
//! ## Debug toggles
//! For validation/debugging we support forcing:
//! - linear output (`OutputColorSpace::Linear`)
//! - sRGB output (`OutputColorSpace::Srgb`)
//! - premultiplied alpha (`PresentAlphaMode::Premultiplied`)
//!
//! See `blit.wgsl` for how these flags affect the final composite.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferColorSpace {
    /// Texture contains linear values (typical for render targets).
    Linear,
    /// Texture contains sRGB-encoded values (rare for render targets; useful for upload paths).
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorSpace {
    /// Present without any gamma encoding.
    Linear,
    /// Present for an sRGB display (either via an sRGB surface format or shader encoding).
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentAlphaMode {
    /// Final output is treated as opaque; alpha is forced to 1.0.
    ///
    /// WebGPU: `alphaMode: "opaque"`
    /// WebGL2: context created with `{ alpha: false }`
    Opaque,
    /// Final output uses premultiplied alpha.
    ///
    /// WebGPU: `alphaMode: "premultiplied"` **and** the presenter must premultiply RGB.
    /// WebGL2: context created with `{ alpha: true, premultipliedAlpha: true }`.
    Premultiplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentOptions {
    pub framebuffer_color_space: FramebufferColorSpace,
    pub output_color_space: OutputColorSpace,
    pub alpha_mode: PresentAlphaMode,
}

impl Default for PresentOptions {
    fn default() -> Self {
        Self {
            framebuffer_color_space: FramebufferColorSpace::Linear,
            output_color_space: OutputColorSpace::Srgb,
            alpha_mode: PresentAlphaMode::Opaque,
        }
    }
}

/// Minimal surface-format model for selection logic.
///
/// The real implementation should use `wgpu::TextureFormat` (native + wasm targets) and/or
/// the WebGPU string formats, but the policy is the same:
/// **prefer an sRGB-capable surface for sRGB output**, otherwise fall back to linear + shader
/// encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceFormat {
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgba8Unorm,
    Rgba8UnormSrgb,
}

impl SurfaceFormat {
    pub fn is_srgb(self) -> bool {
        matches!(self, SurfaceFormat::Bgra8UnormSrgb | SurfaceFormat::Rgba8UnormSrgb)
    }

    pub fn to_srgb(self) -> Option<Self> {
        match self {
            SurfaceFormat::Bgra8Unorm => Some(SurfaceFormat::Bgra8UnormSrgb),
            SurfaceFormat::Rgba8Unorm => Some(SurfaceFormat::Rgba8UnormSrgb),
            SurfaceFormat::Bgra8UnormSrgb | SurfaceFormat::Rgba8UnormSrgb => Some(self),
        }
    }
}

/// Choose the best surface format for presentation.
///
/// - If `output_color_space == Srgb`, pick an `*Srgb` format when available.
/// - Otherwise prefer the first provided format.
pub fn choose_surface_format(
    available: &[SurfaceFormat],
    output_color_space: OutputColorSpace,
) -> Option<SurfaceFormat> {
    if available.is_empty() {
        return None;
    }

    if output_color_space == OutputColorSpace::Srgb {
        // Prefer an sRGB surface, but keep the channel ordering stable (BGRA vs RGBA).
        if let Some(first) = available.first().copied() {
            if let Some(preferred_srgb) = first.to_srgb() {
                if available.contains(&preferred_srgb) {
                    return Some(preferred_srgb);
                }
            }
        }
        if let Some(any_srgb) = available.iter().copied().find(SurfaceFormat::is_srgb) {
            return Some(any_srgb);
        }
    }

    Some(available[0])
}

/// Whether the presenter must apply sRGB gamma encoding in the blit shader.
///
/// If the surface is already an sRGB surface, the GPU will encode automatically and the
/// shader must **not** apply gamma (to avoid double-encoding).
pub fn needs_srgb_encode_in_shader(
    output_color_space: OutputColorSpace,
    surface_format: SurfaceFormat,
) -> bool {
    output_color_space == OutputColorSpace::Srgb && !surface_format.is_srgb()
}

pub fn should_premultiply_in_shader(alpha_mode: PresentAlphaMode) -> bool {
    alpha_mode == PresentAlphaMode::Premultiplied
}

pub fn should_force_opaque_alpha(alpha_mode: PresentAlphaMode) -> bool {
    alpha_mode == PresentAlphaMode::Opaque
}

