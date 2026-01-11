//! Minimal host-side AeroGPU command processor.
//!
//! This module is **not** a full D3D implementation. Instead, it provides the
//! smallest set of state tracking needed to support D3D9Ex/DWM-facing semantics:
//!
//! - Monotonic fence completion (`signal_fence` from the submission descriptor)
//! - Monotonic present counters (suitable for `GetLastPresentCount`-style queries)
//! - Shared surface import/export bookkeeping
//!
//! Rendering/backends are intentionally out of scope here; higher layers can
//! translate the rest of the command stream to WebGPU, but Ex clients still need
//! stable synchronization and sharing primitives even if rendering is minimal.

use crate::protocol::{AeroGpuCmd, AeroGpuCmdStreamParseError, parse_cmd_stream};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandProcessorError {
    Parse(AeroGpuCmdStreamParseError),
    UnknownShareToken(u64),
    ShareTokenAlreadyExported { share_token: u64, existing: u32, new: u32 },
    SharedSurfaceAliasAlreadyBound { alias: u32, existing: u32, new: u32 },
}

impl std::fmt::Display for CommandProcessorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandProcessorError::Parse(err) => write!(f, "failed to parse command stream: {err}"),
            CommandProcessorError::UnknownShareToken(token) => {
                write!(f, "unknown shared surface token 0x{token:016X}")
            }
            CommandProcessorError::ShareTokenAlreadyExported {
                share_token,
                existing,
                new,
            } => write!(
                f,
                "shared surface token 0x{share_token:016X} already exported (existing_handle=0x{existing:X} new_handle=0x{new:X})"
            ),
            CommandProcessorError::SharedSurfaceAliasAlreadyBound { alias, existing, new } => write!(
                f,
                "shared surface alias handle 0x{alias:X} already bound (existing_handle=0x{existing:X} new_handle=0x{new:X})"
            ),
        }
    }
}

impl std::error::Error for CommandProcessorError {}

impl From<AeroGpuCmdStreamParseError> for CommandProcessorError {
    fn from(value: AeroGpuCmdStreamParseError) -> Self {
        CommandProcessorError::Parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AeroGpuEvent {
    /// The completed fence advanced to `fence`.
    FenceSignaled { fence: u64 },
    /// A present completed. `present_count` is monotonic per-processor.
    PresentCompleted { scanout_id: u32, present_count: u64 },
}

/// Host-side state machine for the AeroGPU command stream.
#[derive(Debug, Default)]
pub struct AeroGpuCommandProcessor {
    completed_fence: u64,
    present_count: u64,

    /// share_token -> original resource handle.
    shared_surface_by_token: HashMap<u64, u32>,
    /// alias resource handle -> original resource handle.
    shared_surface_aliases: HashMap<u32, u32>,
}

impl AeroGpuCommandProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn completed_fence(&self) -> u64 {
        self.completed_fence
    }

    pub fn present_count(&self) -> u64 {
        self.present_count
    }

    /// Returns the original resource handle for a (possibly aliased) handle.
    pub fn resolve_shared_surface(&self, handle: u32) -> u32 {
        self.shared_surface_aliases
            .get(&handle)
            .copied()
            .unwrap_or(handle)
    }

    /// Returns the exported handle for `share_token` if known.
    pub fn lookup_shared_surface_token(&self, share_token: u64) -> Option<u32> {
        self.shared_surface_by_token.get(&share_token).copied()
    }

    /// Process a single command buffer submission and update state.
    ///
    /// The caller supplies the submission's `signal_fence` value (from
    /// `aerogpu_submit_desc.signal_fence`). The processor will advance
    /// [`completed_fence`](Self::completed_fence) to at least that value and
    /// emit a corresponding [`AeroGpuEvent::FenceSignaled`].
    pub fn process_submission(
        &mut self,
        cmd_stream_bytes: &[u8],
        signal_fence: u64,
    ) -> Result<Vec<AeroGpuEvent>, CommandProcessorError> {
        let stream = parse_cmd_stream(cmd_stream_bytes)?;
        let mut events = Vec::new();

        for cmd in stream.cmds {
            match cmd {
                AeroGpuCmd::Present { scanout_id, .. }
                | AeroGpuCmd::PresentEx { scanout_id, .. } => {
                    self.present_count = self.present_count.wrapping_add(1);
                    events.push(AeroGpuEvent::PresentCompleted {
                        scanout_id,
                        present_count: self.present_count,
                    });
                }
                AeroGpuCmd::ExportSharedSurface {
                    resource_handle,
                    share_token,
                } => {
                    // If the handle is itself an alias, normalize to the original.
                    let original = self.resolve_shared_surface(resource_handle);
                    if let Some(existing) = self.shared_surface_by_token.get(&share_token).copied() {
                        // Treat re-export of the same token as idempotent, but reject attempts to
                        // retarget a token to a different resource (would corrupt sharing tables).
                        if existing != original {
                            return Err(CommandProcessorError::ShareTokenAlreadyExported {
                                share_token,
                                existing,
                                new: original,
                            });
                        }
                    } else {
                        self.shared_surface_by_token.insert(share_token, original);
                    }
                }
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle,
                    share_token,
                } => {
                    let Some(original) = self.shared_surface_by_token.get(&share_token).copied()
                    else {
                        return Err(CommandProcessorError::UnknownShareToken(share_token));
                    };
                    if let Some(existing) = self.shared_surface_aliases.get(&out_resource_handle).copied() {
                        // Idempotent re-import is allowed if it targets the same original.
                        if existing != original {
                            return Err(CommandProcessorError::SharedSurfaceAliasAlreadyBound {
                                alias: out_resource_handle,
                                existing,
                                new: original,
                            });
                        }
                    } else {
                        self.shared_surface_aliases
                            .insert(out_resource_handle, original);
                    }
                }
                _ => {
                    // For now the processor treats most commands as "handled elsewhere".
                }
            }
        }

        if signal_fence > self.completed_fence {
            self.completed_fence = signal_fence;
            events.push(AeroGpuEvent::FenceSignaled {
                fence: signal_fence,
            });
        }

        Ok(events)
    }
}
