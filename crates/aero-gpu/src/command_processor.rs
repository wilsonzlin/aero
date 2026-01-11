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

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use std::collections::HashMap;

/// Per-submission allocation table entry (Win7 WDDM 1.1 legacy path).
///
/// Each AeroGPU submission may carry a sideband list that maps a stable `alloc_id`
/// (referenced by `backing_alloc_id` in the command stream) to a guest physical
/// address (GPA) and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuSubmissionAllocation {
    pub alloc_id: u32,
    pub gpa: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceDesc {
    Buffer {
        usage_flags: u32,
        size_bytes: u64,
    },
    Texture2d {
        usage_flags: u32,
        format: u32,
        width: u32,
        height: u32,
        mip_levels: u32,
        array_layers: u32,
        row_pitch_bytes: u32,
    },
}

impl ResourceDesc {
    fn size_bytes(&self) -> Result<u64, CommandProcessorError> {
        match *self {
            ResourceDesc::Buffer { size_bytes, .. } => Ok(size_bytes),
            ResourceDesc::Texture2d {
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
                ..
            } => {
                // Conservative linear-size estimate:
                // - For mip0 we trust `row_pitch_bytes`.
                // - For additional mips we approximate by shifting the mip0 row pitch / height.
                //
                // This is intentionally conservative validation used for bounds checking and is
                // not relied upon for actual texture layout decisions.
                if width == 0 || height == 0 || mip_levels == 0 || array_layers == 0 {
                    return Err(CommandProcessorError::InvalidCreateTexture2d);
                }

                let mut total = 0u64;
                for level in 0..mip_levels {
                    let level_row_pitch = (u64::from(row_pitch_bytes) >> level).max(1);
                    let level_height = (u64::from(height) >> level).max(1);
                    let level_size = level_row_pitch
                        .checked_mul(level_height)
                        .ok_or(CommandProcessorError::SizeOverflow)?;
                    total = total
                        .checked_add(level_size)
                        .ok_or(CommandProcessorError::SizeOverflow)?;
                }
                total = total
                    .checked_mul(u64::from(array_layers))
                    .ok_or(CommandProcessorError::SizeOverflow)?;
                Ok(total)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResourceEntry {
    desc: ResourceDesc,
    backing_alloc_id: u32,
    backing_offset_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandProcessorError {
    Parse(AeroGpuCmdStreamParseError),

    // Shared surfaces
    InvalidShareToken(u64),
    UnknownShareToken(u64),
    UnknownSharedSurfaceHandle(u32),
    SharedSurfaceHandleInUse(u32),
    ShareTokenAlreadyExported {
        share_token: u64,
        existing: u32,
        new: u32,
    },
    SharedSurfaceAliasAlreadyBound {
        alias: u32,
        existing: u32,
        new: u32,
    },

    // Allocation-backed resources
    InvalidResourceHandle(u32),
    UnknownResourceHandle(u32),
    MissingAllocationTable(u32),
    UnknownAllocId(u32),
    SizeOverflow,
    ResourceOutOfBounds {
        resource_handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        resource_size_bytes: u64,
    },
    AllocationOutOfBounds {
        alloc_id: u32,
        offset_bytes: u64,
        size_bytes: u64,
        alloc_size_bytes: u64,
    },
    CreateRebindMismatch {
        resource_handle: u32,
    },
    InvalidCreateBuffer,
    InvalidCreateTexture2d,
}

impl std::fmt::Display for CommandProcessorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandProcessorError::Parse(err) => write!(f, "failed to parse command stream: {err}"),
            CommandProcessorError::InvalidShareToken(token) => write!(
                f,
                "invalid shared surface token 0x{token:016X} (0 is reserved)"
            ),
            CommandProcessorError::UnknownShareToken(token) => {
                write!(f, "unknown shared surface token 0x{token:016X}")
            }
            CommandProcessorError::UnknownSharedSurfaceHandle(handle) => {
                write!(f, "unknown shared surface handle 0x{handle:08X}")
            }
            CommandProcessorError::SharedSurfaceHandleInUse(handle) => {
                write!(f, "shared surface handle 0x{handle:08X} is already in use")
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
            CommandProcessorError::InvalidResourceHandle(handle) => {
                write!(
                    f,
                    "invalid resource handle 0x{handle:08X} (0 is reserved)"
                )
            }
            CommandProcessorError::UnknownResourceHandle(handle) => {
                write!(f, "unknown resource handle 0x{handle:08X}")
            }
            CommandProcessorError::MissingAllocationTable(alloc_id) => write!(
                f,
                "submission is missing an allocation table required to resolve alloc_id={alloc_id}"
            ),
            CommandProcessorError::UnknownAllocId(alloc_id) => {
                write!(f, "allocation table does not contain alloc_id={alloc_id}")
            }
            CommandProcessorError::SizeOverflow => write!(f, "size arithmetic overflow"),
            CommandProcessorError::ResourceOutOfBounds {
                resource_handle,
                offset_bytes,
                size_bytes,
                resource_size_bytes,
            } => write!(
                f,
                "resource 0x{resource_handle:08X} out-of-bounds: offset={offset_bytes} size={size_bytes} (resource_size={resource_size_bytes})"
            ),
            CommandProcessorError::AllocationOutOfBounds {
                alloc_id,
                offset_bytes,
                size_bytes,
                alloc_size_bytes,
            } => write!(
                f,
                "allocation alloc_id={alloc_id} out-of-bounds: offset={offset_bytes} size={size_bytes} (alloc_size={alloc_size_bytes})"
            ),
            CommandProcessorError::CreateRebindMismatch { resource_handle } => write!(
                f,
                "CREATE_* for existing handle 0x{resource_handle:08X} has mismatched immutable properties; destroy and recreate the handle"
            ),
            CommandProcessorError::InvalidCreateBuffer => {
                write!(f, "invalid CREATE_BUFFER parameters")
            }
            CommandProcessorError::InvalidCreateTexture2d => {
                write!(f, "invalid CREATE_TEXTURE2D parameters")
            }
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

    /// share_token -> underlying resource handle.
    shared_surface_by_token: HashMap<u64, u32>,

    /// Handle indirection table for shared surfaces.
    ///
    /// - Original surfaces are stored as `handle -> handle`
    /// - Imported surfaces are stored as `alias_handle -> underlying_handle`
    shared_surface_handles: HashMap<u32, u32>,

    /// Refcount table keyed by the underlying handle.
    ///
    /// Refcount includes the original handle entry plus all imported aliases.
    shared_surface_refcounts: HashMap<u32, u32>,

    /// Tracked resource descriptors + stable allocation bindings.
    resources: HashMap<u32, ResourceEntry>,
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

    /// Returns the underlying resource handle for a (possibly aliased) handle.
    ///
    /// Note: destroyed handles are treated as unknown and return `handle` unchanged.
    pub fn resolve_shared_surface(&self, handle: u32) -> u32 {
        self.shared_surface_handles
            .get(&handle)
            .copied()
            .unwrap_or(handle)
    }

    /// Returns the exported handle for `share_token` if known.
    pub fn lookup_shared_surface_token(&self, share_token: u64) -> Option<u32> {
        self.shared_surface_by_token.get(&share_token).copied()
    }

    fn register_shared_surface(&mut self, handle: u32) -> Result<(), CommandProcessorError> {
        if handle == 0 {
            return Err(CommandProcessorError::InvalidResourceHandle(handle));
        }
        if let Some(existing_underlying) = self.shared_surface_handles.get(&handle).copied() {
            // A handle that is already bound as an alias must not be reused as a new texture handle.
            if existing_underlying != handle {
                return Err(CommandProcessorError::SharedSurfaceHandleInUse(handle));
            }
            return Ok(());
        }
        // Handle reuse would corrupt the aliasing tables because this processor uses protocol
        // handles as the underlying resource id.
        if self.shared_surface_refcounts.contains_key(&handle) {
            return Err(CommandProcessorError::SharedSurfaceHandleInUse(handle));
        }
        self.shared_surface_handles.insert(handle, handle);
        *self.shared_surface_refcounts.entry(handle).or_insert(0) += 1;
        Ok(())
    }

    fn resolve_shared_surface_handle(&self, handle: u32) -> Option<u32> {
        self.shared_surface_handles.get(&handle).copied()
    }

    fn destroy_shared_surface_handle(&mut self, handle: u32) -> Option<u32> {
        let Some(underlying) = self.shared_surface_handles.remove(&handle) else {
            return None;
        };
        let Some(count) = self.shared_surface_refcounts.get_mut(&underlying) else {
            return None;
        };

        *count = count.saturating_sub(1);
        if *count != 0 {
            return None;
        }

        self.shared_surface_refcounts.remove(&underlying);
        self.shared_surface_by_token.retain(|_, v| *v != underlying);
        Some(underlying)
    }

    fn lookup_allocation(
        allocations: Option<&[AeroGpuSubmissionAllocation]>,
        alloc_id: u32,
    ) -> Result<AeroGpuSubmissionAllocation, CommandProcessorError> {
        let Some(allocations) = allocations else {
            return Err(CommandProcessorError::MissingAllocationTable(alloc_id));
        };
        allocations
            .iter()
            .copied()
            .find(|a| a.alloc_id == alloc_id)
            .ok_or(CommandProcessorError::UnknownAllocId(alloc_id))
    }

    fn validate_range_in_resource(
        handle: u32,
        resource_size_bytes: u64,
        offset_bytes: u64,
        size_bytes: u64,
    ) -> Result<(), CommandProcessorError> {
        let end = offset_bytes
            .checked_add(size_bytes)
            .ok_or(CommandProcessorError::SizeOverflow)?;
        if end > resource_size_bytes {
            return Err(CommandProcessorError::ResourceOutOfBounds {
                resource_handle: handle,
                offset_bytes,
                size_bytes,
                resource_size_bytes,
            });
        }
        Ok(())
    }

    fn validate_range_in_allocation(
        alloc: AeroGpuSubmissionAllocation,
        offset_bytes: u64,
        size_bytes: u64,
    ) -> Result<(), CommandProcessorError> {
        let end = offset_bytes
            .checked_add(size_bytes)
            .ok_or(CommandProcessorError::SizeOverflow)?;
        if end > alloc.size_bytes {
            return Err(CommandProcessorError::AllocationOutOfBounds {
                alloc_id: alloc.alloc_id,
                offset_bytes,
                size_bytes,
                alloc_size_bytes: alloc.size_bytes,
            });
        }
        Ok(())
    }

    fn release_shared_surface_token(&mut self, share_token: u64) {
        // KMD-emitted "share token is no longer importable" signal.
        //
        // Existing imported handles remain valid and keep the underlying resource alive via the
        // refcount tables; we only remove the token mapping so future imports fail deterministically.
        self.shared_surface_by_token.remove(&share_token);
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
        self.process_submission_with_allocations(cmd_stream_bytes, None, signal_fence)
    }

    /// Process a single submission along with its (optional) allocation table.
    ///
    /// The allocation table is required to resolve any `backing_alloc_id != 0`
    /// referenced by resource creation and dirty-range commands. It is expected
    /// to be provided by the Win7 WDDM KMD submission descriptor.
    pub fn process_submission_with_allocations(
        &mut self,
        cmd_stream_bytes: &[u8],
        allocations: Option<&[AeroGpuSubmissionAllocation]>,
        signal_fence: u64,
    ) -> Result<Vec<AeroGpuEvent>, CommandProcessorError> {
        let stream = parse_cmd_stream(cmd_stream_bytes)?;
        let mut events = Vec::new();

        for cmd in stream.cmds {
            match cmd {
                AeroGpuCmd::CreateBuffer {
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => {
                    if buffer_handle == 0 {
                        return Err(CommandProcessorError::InvalidResourceHandle(buffer_handle));
                    }
                    if let Some(underlying) = self.shared_surface_handles.get(&buffer_handle) {
                        // Shared surface aliases live in the same global handle namespace, so they
                        // must not be reused for a different resource type.
                        if *underlying != buffer_handle {
                            return Err(CommandProcessorError::SharedSurfaceHandleInUse(
                                buffer_handle,
                            ));
                        }
                    }
                    if size_bytes == 0 || size_bytes % 4 != 0 {
                        return Err(CommandProcessorError::InvalidCreateBuffer);
                    }
                    let desc = ResourceDesc::Buffer {
                        usage_flags,
                        size_bytes,
                    };

                    if backing_alloc_id != 0 {
                        let alloc = Self::lookup_allocation(allocations, backing_alloc_id)?;
                        let offset = u64::from(backing_offset_bytes);
                        Self::validate_range_in_allocation(alloc, offset, size_bytes)?;
                    }

                    match self.resources.get_mut(&buffer_handle) {
                        Some(existing) => {
                            if existing.desc != desc {
                                return Err(CommandProcessorError::CreateRebindMismatch {
                                    resource_handle: buffer_handle,
                                });
                            }
                            existing.backing_alloc_id = backing_alloc_id;
                            existing.backing_offset_bytes = backing_offset_bytes;
                        }
                        None => {
                            self.resources.insert(
                                buffer_handle,
                                ResourceEntry {
                                    desc,
                                    backing_alloc_id,
                                    backing_offset_bytes,
                                },
                            );
                        }
                    }
                }
                AeroGpuCmd::CreateTexture2d {
                    texture_handle,
                    usage_flags,
                    format,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => {
                    if texture_handle == 0 {
                        return Err(CommandProcessorError::InvalidResourceHandle(texture_handle));
                    }
                    if let Some(underlying) = self.shared_surface_handles.get(&texture_handle) {
                        if *underlying != texture_handle {
                            return Err(CommandProcessorError::SharedSurfaceHandleInUse(
                                texture_handle,
                            ));
                        }
                    } else if self.shared_surface_refcounts.contains_key(&texture_handle) {
                        return Err(CommandProcessorError::SharedSurfaceHandleInUse(
                            texture_handle,
                        ));
                    }
                    if width == 0 || height == 0 || mip_levels == 0 || array_layers == 0 {
                        return Err(CommandProcessorError::InvalidCreateTexture2d);
                    }

                    if backing_alloc_id != 0 && row_pitch_bytes == 0 {
                        return Err(CommandProcessorError::InvalidCreateTexture2d);
                    }

                    let desc = ResourceDesc::Texture2d {
                        usage_flags,
                        format,
                        width,
                        height,
                        mip_levels,
                        array_layers,
                        row_pitch_bytes,
                    };

                    if let Some(existing) = self.resources.get(&texture_handle) {
                        if existing.desc != desc {
                            return Err(CommandProcessorError::CreateRebindMismatch {
                                resource_handle: texture_handle,
                            });
                        }
                    }

                    let resource_size_bytes = desc.size_bytes()?;
                    if backing_alloc_id != 0 {
                        let alloc = Self::lookup_allocation(allocations, backing_alloc_id)?;
                        let offset = u64::from(backing_offset_bytes);
                        Self::validate_range_in_allocation(alloc, offset, resource_size_bytes)?;
                    }

                    self.register_shared_surface(texture_handle)?;

                    match self.resources.get_mut(&texture_handle) {
                        Some(existing) => {
                            existing.backing_alloc_id = backing_alloc_id;
                            existing.backing_offset_bytes = backing_offset_bytes;
                        }
                        None => {
                            self.resources.insert(
                                texture_handle,
                                ResourceEntry {
                                    desc,
                                    backing_alloc_id,
                                    backing_offset_bytes,
                                },
                            );
                        }
                    }
                }
                AeroGpuCmd::DestroyResource { resource_handle } => {
                    if let Some(underlying) = self.destroy_shared_surface_handle(resource_handle) {
                        self.resources.remove(&underlying);
                        continue;
                    }

                    // If this handle is still the underlying ID of a shared surface, do not remove
                    // the resource until the last alias is destroyed.
                    if !self.shared_surface_refcounts.contains_key(&resource_handle) {
                        self.resources.remove(&resource_handle);
                    }
                }
                AeroGpuCmd::ResourceDirtyRange {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                } => {
                    let underlying = self.resolve_shared_surface(resource_handle);
                    let Some(entry) = self.resources.get(&underlying).copied() else {
                        return Err(CommandProcessorError::UnknownResourceHandle(
                            resource_handle,
                        ));
                    };

                    // Dirty ranges are only meaningful for guest-backed resources. Some command
                    // streams may conservatively emit dirty notifications for host-owned resources;
                    // ignore them rather than failing validation.
                    if entry.backing_alloc_id == 0 {
                        continue;
                    }

                    let resource_size_bytes = entry.desc.size_bytes()?;
                    Self::validate_range_in_resource(
                        underlying,
                        resource_size_bytes,
                        offset_bytes,
                        size_bytes,
                    )?;

                    let alloc = Self::lookup_allocation(allocations, entry.backing_alloc_id)?;
                    let alloc_offset = u64::from(entry.backing_offset_bytes)
                        .checked_add(offset_bytes)
                        .ok_or(CommandProcessorError::SizeOverflow)?;
                    Self::validate_range_in_allocation(alloc, alloc_offset, size_bytes)?;
                }
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
                    if resource_handle == 0 {
                        return Err(CommandProcessorError::InvalidResourceHandle(
                            resource_handle,
                        ));
                    }
                    if share_token == 0 {
                        return Err(CommandProcessorError::InvalidShareToken(share_token));
                    }
                    // If the handle is itself an alias, normalize to the underlying surface.
                    let Some(underlying) = self.resolve_shared_surface_handle(resource_handle)
                    else {
                        return Err(CommandProcessorError::UnknownSharedSurfaceHandle(
                            resource_handle,
                        ));
                    };

                    if let Some(existing) = self.shared_surface_by_token.get(&share_token).copied()
                    {
                        // Treat re-export of the same token as idempotent, but reject attempts to
                        // retarget a token to a different resource (would corrupt sharing tables).
                        if existing != underlying {
                            return Err(CommandProcessorError::ShareTokenAlreadyExported {
                                share_token,
                                existing,
                                new: underlying,
                            });
                        }
                    } else {
                        self.shared_surface_by_token.insert(share_token, underlying);
                    }
                }
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle,
                    share_token,
                } => {
                    if out_resource_handle == 0 {
                        return Err(CommandProcessorError::InvalidResourceHandle(
                            out_resource_handle,
                        ));
                    }
                    if share_token == 0 {
                        return Err(CommandProcessorError::InvalidShareToken(share_token));
                    }
                    let Some(&underlying) = self.shared_surface_by_token.get(&share_token) else {
                        return Err(CommandProcessorError::UnknownShareToken(share_token));
                    };

                    // If the underlying surface has already been destroyed, treat the token as
                    // invalid.
                    if !self.shared_surface_refcounts.contains_key(&underlying) {
                        return Err(CommandProcessorError::UnknownShareToken(share_token));
                    }

                    if let Some(existing) = self.shared_surface_handles.get(&out_resource_handle) {
                        // Idempotent re-import is allowed if it targets the same original.
                        if *existing != underlying {
                            return Err(CommandProcessorError::SharedSurfaceAliasAlreadyBound {
                                alias: out_resource_handle,
                                existing: *existing,
                                new: underlying,
                            });
                        }
                    } else {
                        self.shared_surface_handles
                            .insert(out_resource_handle, underlying);
                        *self.shared_surface_refcounts.entry(underlying).or_insert(0) += 1;
                    }
                }
                AeroGpuCmd::ReleaseSharedSurface { share_token } => {
                    self.release_shared_surface_token(share_token);
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
