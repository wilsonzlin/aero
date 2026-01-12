use std::collections::{HashMap, HashSet};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SharedSurfaceError {
    #[error("invalid shared surface handle 0x{0:08X} (0 is reserved)")]
    InvalidHandle(u32),
    #[error("unknown shared surface handle 0x{0:08X}")]
    UnknownHandle(u32),
    #[error("invalid shared surface token 0x{0:016X} (0 is reserved)")]
    InvalidToken(u64),
    #[error("shared surface token 0x{0:016X} was previously released and cannot be reused")]
    TokenRetired(u64),
    #[error("unknown shared surface token 0x{0:016X}")]
    UnknownToken(u64),
    #[error(
        "shared surface token 0x{share_token:016X} already exported (existing=0x{existing:08X} new=0x{new:08X})"
    )]
    TokenAlreadyExported {
        share_token: u64,
        existing: u32,
        new: u32,
    },
    #[error(
        "shared surface alias handle 0x{alias:08X} already bound (existing=0x{existing:08X} new=0x{new:08X})"
    )]
    AliasAlreadyBound { alias: u32, existing: u32, new: u32 },
    #[error(
        "shared surface token 0x{share_token:016X} refers to destroyed handle 0x{underlying:08X}"
    )]
    TokenRefersToDestroyed { share_token: u64, underlying: u32 },
    #[error("shared surface handle 0x{handle:08X} is already an alias for 0x{underlying:08X}")]
    HandleIsAlias { handle: u32, underlying: u32 },
    #[error(
        "shared surface handle 0x{0:08X} is still in use (underlying id kept alive by shared surface aliases)"
    )]
    HandleStillInUse(u32),
    #[error(
        "shared surface handle 0x{0:08X} was destroyed (underlying id kept alive by shared surface aliases)"
    )]
    HandleDestroyed(u32),
}

/// Shared surface bookkeeping for `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`.
///
/// This models D3D9Ex / DWM sharing semantics at the host executor layer:
/// - `EXPORT` associates a stable 64-bit `share_token` with a live resource.
/// - `IMPORT` creates a new handle aliasing the exported resource.
/// - `RELEASE_SHARED_SURFACE` removes the `share_token` mapping (so future imports fail).
/// - `DESTROY_RESOURCE` decrements the reference count for the underlying resource and
///   destroys it only once the last reference is released.
#[derive(Debug, Default)]
pub(crate) struct SharedSurfaceTable {
    /// `share_token -> underlying resource handle`.
    by_token: HashMap<u64, u32>,
    /// `share_token` values that were previously valid but were released (or otherwise removed).
    ///
    /// This prevents misbehaving guests from "re-arming" a released token by re-exporting it for
    /// a different resource, which could otherwise resurrect stale handles.
    retired_tokens: HashSet<u64>,
    /// `handle -> underlying resource handle`.
    ///
    /// - Original resources are stored as `handle -> handle`
    /// - Imported aliases are stored as `alias_handle -> underlying_handle`
    handles: HashMap<u32, u32>,
    /// `underlying handle -> refcount`.
    refcounts: HashMap<u32, u32>,
}

impl SharedSurfaceTable {
    fn retire_tokens_for_underlying(&mut self, underlying: u32) {
        let retired = &mut self.retired_tokens;
        self.by_token.retain(|token, v| {
            if *v == underlying {
                retired.insert(*token);
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn clear(&mut self) {
        self.by_token.clear();
        self.retired_tokens.clear();
        self.handles.clear();
        self.refcounts.clear();
    }

    pub(crate) fn register_handle(&mut self, handle: u32) -> Result<(), SharedSurfaceError> {
        if handle == 0 {
            return Err(SharedSurfaceError::InvalidHandle(handle));
        }
        if let Some(&existing) = self.handles.get(&handle) {
            if existing != handle {
                return Err(SharedSurfaceError::HandleIsAlias {
                    handle,
                    underlying: existing,
                });
            }
            return Ok(());
        }
        if self.refcounts.contains_key(&handle) {
            // Underlying handles remain reserved as long as any aliases still reference them. If
            // the original handle was destroyed, it must not be reused as a new original until
            // the underlying resource is fully released.
            return Err(SharedSurfaceError::HandleStillInUse(handle));
        }
        self.handles.insert(handle, handle);
        *self.refcounts.entry(handle).or_insert(0) += 1;
        Ok(())
    }

    pub(crate) fn resolve_handle(&self, handle: u32) -> u32 {
        self.handles.get(&handle).copied().unwrap_or(handle)
    }

    /// Resolves a handle coming from an AeroGPU command stream.
    ///
    /// This differs from [`Self::resolve_handle`] by treating "reserved underlying IDs" as
    /// invalid: if an original handle has been destroyed while shared-surface aliases still exist,
    /// the underlying numeric ID is kept alive in `refcounts` to prevent handle reuse/collision,
    /// but the original handle value must not be used for subsequent commands.
    pub(crate) fn resolve_cmd_handle(&self, handle: u32) -> Result<u32, SharedSurfaceError> {
        if handle == 0 {
            return Ok(0);
        }
        if self.handles.contains_key(&handle) {
            return Ok(self.resolve_handle(handle));
        }
        if self.refcounts.contains_key(&handle) {
            return Err(SharedSurfaceError::HandleDestroyed(handle));
        }
        Ok(handle)
    }

    pub(crate) fn export(
        &mut self,
        resource_handle: u32,
        share_token: u64,
    ) -> Result<(), SharedSurfaceError> {
        if resource_handle == 0 {
            return Err(SharedSurfaceError::InvalidHandle(resource_handle));
        }
        if share_token == 0 {
            return Err(SharedSurfaceError::InvalidToken(share_token));
        }
        if self.retired_tokens.contains(&share_token) {
            return Err(SharedSurfaceError::TokenRetired(share_token));
        }
        let underlying = self
            .handles
            .get(&resource_handle)
            .copied()
            .ok_or(SharedSurfaceError::UnknownHandle(resource_handle))?;

        if let Some(&existing) = self.by_token.get(&share_token) {
            if existing != underlying {
                return Err(SharedSurfaceError::TokenAlreadyExported {
                    share_token,
                    existing,
                    new: underlying,
                });
            }
            return Ok(());
        }

        self.by_token.insert(share_token, underlying);
        Ok(())
    }

    pub(crate) fn import(
        &mut self,
        out_resource_handle: u32,
        share_token: u64,
    ) -> Result<(), SharedSurfaceError> {
        if out_resource_handle == 0 {
            return Err(SharedSurfaceError::InvalidHandle(out_resource_handle));
        }
        if share_token == 0 {
            return Err(SharedSurfaceError::InvalidToken(share_token));
        }
        let Some(&underlying) = self.by_token.get(&share_token) else {
            return Err(SharedSurfaceError::UnknownToken(share_token));
        };

        if !self.refcounts.contains_key(&underlying) {
            return Err(SharedSurfaceError::TokenRefersToDestroyed {
                share_token,
                underlying,
            });
        }

        if let Some(&existing) = self.handles.get(&out_resource_handle) {
            if existing != underlying {
                return Err(SharedSurfaceError::AliasAlreadyBound {
                    alias: out_resource_handle,
                    existing,
                    new: underlying,
                });
            }
            return Ok(());
        }
        if self.refcounts.contains_key(&out_resource_handle) {
            // Underlying handles remain reserved as long as any aliases still reference them. If
            // an original handle was destroyed, it must not be reused as a new alias handle until
            // the underlying resource is fully released.
            return Err(SharedSurfaceError::HandleStillInUse(out_resource_handle));
        }

        self.handles.insert(out_resource_handle, underlying);
        *self.refcounts.entry(underlying).or_insert(0) += 1;
        Ok(())
    }

    pub(crate) fn release_token(&mut self, share_token: u64) -> bool {
        if share_token == 0 {
            return false;
        }
        // Idempotent: unknown tokens are a no-op (see `aerogpu_cmd.h` contract).
        //
        // Only retire tokens that were actually exported at some point (present in `by_token`),
        // or that are already retired.
        if self.by_token.remove(&share_token).is_some() {
            self.retired_tokens.insert(share_token);
            return true;
        }
        false
    }

    /// Releases a handle (original or alias). Returns `(underlying_handle, last_ref)` if the
    /// handle was tracked.
    pub(crate) fn destroy_handle(&mut self, handle: u32) -> Option<(u32, bool)> {
        if handle == 0 {
            return None;
        }

        let Some(underlying) = self.handles.remove(&handle) else {
            // If the original handle has already been destroyed (removed from `handles`) but the
            // underlying resource is still alive due to aliases, treat duplicate destroys as an
            // idempotent no-op.
            if self.refcounts.contains_key(&handle) {
                return Some((handle, false));
            }
            return None;
        };
        let Some(count) = self.refcounts.get_mut(&underlying) else {
            // Table invariant broken (handle tracked but no refcount entry). Treat as last-ref so
            // callers can clean up the underlying resource instead of leaking it.
            self.retire_tokens_for_underlying(underlying);
            return Some((underlying, true));
        };

        *count = count.saturating_sub(1);
        if *count != 0 {
            return Some((underlying, false));
        }

        self.refcounts.remove(&underlying);
        self.retire_tokens_for_underlying(underlying);
        Some((underlying, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retires_tokens_when_last_ref_destroyed() {
        let mut table = SharedSurfaceTable::default();
        table.register_handle(1).unwrap();
        table.export(1, 0x10).unwrap();
        table.export(1, 0x20).unwrap();

        let (underlying, last) = table.destroy_handle(1).expect("destroy must succeed");
        assert_eq!(underlying, 1);
        assert!(last);

        assert!(!table.by_token.contains_key(&0x10));
        assert!(!table.by_token.contains_key(&0x20));
        assert!(table.retired_tokens.contains(&0x10));
        assert!(table.retired_tokens.contains(&0x20));
    }

    #[test]
    fn release_token_is_idempotent() {
        let mut table = SharedSurfaceTable::default();
        table.register_handle(1).unwrap();
        table.export(1, 0x10).unwrap();

        assert!(table.release_token(0x10));
        assert!(!table.release_token(0x10));
        assert!(!table.by_token.contains_key(&0x10));
        assert!(table.retired_tokens.contains(&0x10));
    }
}
