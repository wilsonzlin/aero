use crate::vertex::declaration::DeclUsage;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use thiserror::Error;

/// Map D3D9 `(usage, usage_index)` pairs to WGSL `@location(n)`.
///
/// The goal is to keep locations:
/// * deterministic across pipelines (to maximize shader cache hits),
/// * within WebGPU's guaranteed `maxVertexAttributes` lower bound (16),
/// * compatible with both vertex declarations and FVF-derived layouts.
pub trait VertexLocationMap: fmt::Debug + Send + Sync {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError>;
}

/// Adaptive semantic-based location mapping.
///
/// D3D9 vertex declarations and vertex shader `dcl_*` blocks can legally use many semantic/index
/// combinations beyond the common subset covered by [`StandardLocationMap`]. WebGPU requires vertex
/// input locations to be numeric and within `maxVertexAttributes`, so we need a deterministic
/// allocator.
///
/// [`AdaptiveLocationMap`] takes the ordered list of `(usage, usage_index)` pairs present in a
/// declaration and assigns each one a WGSL `@location(n)`:
/// * Common semantics are pinned to the same locations as [`StandardLocationMap`] to preserve shader
///   cache hit rates.
/// * Any remaining semantics are assigned to the lowest available locations in list order.
///
/// This map is intended for programmable pipelines (vertex shader input semantics).
#[derive(Debug, Clone)]
pub struct AdaptiveLocationMap {
    map: HashMap<(DeclUsage, u8), u32>,
}

impl AdaptiveLocationMap {
    /// WebGPU's guaranteed minimum `maxVertexAttributes` limit.
    pub const WEBGPU_MIN_VERTEX_ATTRIBUTES: u32 = 16;

    /// Build a location map for a declaration's semantic list.
    ///
    /// The input order must be deterministic (e.g. D3D9 vertex element order, or shader `dcl`
    /// instruction order) so the resulting mapping is stable.
    pub fn new(
        semantics: impl IntoIterator<Item = (DeclUsage, u8)>,
    ) -> Result<Self, LocationMapError> {
        Self::new_with_limit(semantics, Self::WEBGPU_MIN_VERTEX_ATTRIBUTES)
    }

    pub fn new_with_limit(
        semantics: impl IntoIterator<Item = (DeclUsage, u8)>,
        max_locations: u32,
    ) -> Result<Self, LocationMapError> {
        let max_locations = max_locations.max(1);

        // Deduplicate while preserving first-seen order.
        let mut ordered = Vec::<(DeclUsage, u8)>::new();
        let mut seen_pairs = BTreeSet::<(u8, u8)>::new();
        for (usage, usage_index) in semantics {
            let key = (usage as u8, usage_index);
            if seen_pairs.insert(key) {
                ordered.push((usage, usage_index));
            }
        }

        // Step 1: reserve the legacy StandardLocationMap assignments for any semantics that it can
        // represent.
        let standard = StandardLocationMap;
        let mut used_locations = BTreeSet::<u32>::new();
        let mut map = HashMap::<(DeclUsage, u8), u32>::new();
        for &(usage, usage_index) in &ordered {
            if let Ok(loc) = standard.location_for(usage, usage_index) {
                map.insert((usage, usage_index), loc);
                used_locations.insert(loc);
            }
        }

        // Step 2: allocate remaining semantics to the lowest available locations.
        let next_free = |used: &BTreeSet<u32>| -> Option<u32> {
            (0..max_locations).find(|l| !used.contains(l))
        };

        for (usage, usage_index) in ordered {
            if map.contains_key(&(usage, usage_index)) {
                continue;
            }
            let Some(loc) = next_free(&used_locations) else {
                return Err(LocationMapError::OutOfLocations {
                    usage,
                    usage_index,
                    max: max_locations,
                });
            };
            map.insert((usage, usage_index), loc);
            used_locations.insert(loc);
        }

        Ok(Self { map })
    }
}

impl VertexLocationMap for AdaptiveLocationMap {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError> {
        self.map
            .get(&(usage, usage_index))
            .copied()
            .ok_or(LocationMapError::UnsupportedSemantic { usage, usage_index })
    }
}

/// Default mapping used for shader-based pipelines.
///
/// This intentionally fits the most common D3D9 semantics into locations `0..16`:
///
/// | D3D usage          | index | WGSL location |
/// |-------------------|-------|--------------|
/// | POSITION          | 0     | 0            |
/// | NORMAL            | 0     | 1            |
/// | TANGENT           | 0     | 2            |
/// | BINORMAL          | 0     | 3            |
/// | BLENDWEIGHT       | 0     | 4            |
/// | BLENDINDICES      | 0     | 5            |
/// | COLOR             | 0     | 6            |
/// | COLOR             | 1     | 7            |
/// | TEXCOORD          | 0..7  | 8..15        |
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardLocationMap;

impl VertexLocationMap for StandardLocationMap {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError> {
        match usage {
            DeclUsage::Position => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::PositionT => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Normal => match usage_index {
                0 => Ok(1),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Tangent => match usage_index {
                0 => Ok(2),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Binormal => match usage_index {
                0 => Ok(3),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendWeight => match usage_index {
                0 => Ok(4),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendIndices => match usage_index {
                0 => Ok(5),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Color => match usage_index {
                0 => Ok(6),
                1 => Ok(7),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::TexCoord => match usage_index {
                0..=7 => Ok(8 + usage_index as u32),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            other => Err(LocationMapError::UnsupportedSemantic {
                usage: other,
                usage_index,
            }),
        }
    }
}

/// Location map intended for fixed-function / FVF-generated shaders.
///
/// Fixed-function uses a narrower set of semantics than programmable shaders and benefits from a
/// layout that keeps common FVF fields packed at low locations:
///
/// | D3D usage          | index | WGSL location |
/// |-------------------|-------|--------------|
/// | POSITION          | 0     | 0            |
/// | NORMAL            | 0     | 1            |
/// | PSIZE             | 0     | 2            |
/// | COLOR             | 0     | 3            |
/// | COLOR             | 1     | 4            |
/// | TEXCOORD          | 0..7  | 5..12        |
/// | BLENDWEIGHT       | 0     | 13           |
/// | BLENDINDICES      | 0     | 14           |
/// | TANGENT           | 0     | 15           |
///
/// `BINORMAL` is intentionally rejected because it would exceed the guaranteed WebGPU minimum of
/// 16 attributes, and fixed-function content rarely uses it.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixedFunctionLocationMap;

impl VertexLocationMap for FixedFunctionLocationMap {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError> {
        match usage {
            DeclUsage::Position => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::PositionT => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Normal => match usage_index {
                0 => Ok(1),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::PSize => match usage_index {
                0 => Ok(2),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Color => match usage_index {
                0 => Ok(3),
                1 => Ok(4),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::TexCoord => match usage_index {
                0..=7 => Ok(5 + usage_index as u32),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendWeight => match usage_index {
                0 => Ok(13),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendIndices => match usage_index {
                0 => Ok(14),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Tangent => match usage_index {
                0 => Ok(15),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LocationMapError {
    #[error("unsupported vertex semantic {usage:?}{usage_index} for WGSL location mapping")]
    UnsupportedSemantic { usage: DeclUsage, usage_index: u8 },

    #[error(
        "vertex semantic {usage:?}{usage_index} could not be mapped to a WGSL location (exceeded max vertex attributes {max})"
    )]
    OutOfLocations {
        usage: DeclUsage,
        usage_index: u8,
        max: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_is_deterministic() {
        let semantics = vec![
            (DeclUsage::Position, 0),
            (DeclUsage::TexCoord, 0),
            (DeclUsage::Fog, 0),
            (DeclUsage::Normal, 1),
            (DeclUsage::TexCoord, 8),
        ];

        let a = AdaptiveLocationMap::new(semantics.clone()).unwrap();
        let b = AdaptiveLocationMap::new(semantics).unwrap();

        for (usage, idx) in [
            (DeclUsage::Position, 0),
            (DeclUsage::TexCoord, 0),
            (DeclUsage::Fog, 0),
            (DeclUsage::Normal, 1),
            (DeclUsage::TexCoord, 8),
        ] {
            assert_eq!(
                a.location_for(usage, idx).unwrap(),
                b.location_for(usage, idx).unwrap()
            );
        }
    }

    #[test]
    fn adaptive_preserves_standard_mappings() {
        let semantics = vec![
            (DeclUsage::Position, 0),
            (DeclUsage::Normal, 0),
            (DeclUsage::Color, 0),
            (DeclUsage::Color, 1),
            (DeclUsage::TexCoord, 0),
            (DeclUsage::TexCoord, 7),
        ];

        let adaptive = AdaptiveLocationMap::new(semantics.clone()).unwrap();
        let standard = StandardLocationMap;

        for (usage, idx) in semantics {
            assert_eq!(
                adaptive.location_for(usage, idx).unwrap(),
                standard.location_for(usage, idx).unwrap()
            );
        }
    }

    #[test]
    fn adaptive_supports_texcoord8_regression() {
        // StandardLocationMap rejects TEXCOORD8, but the declaration is still within 16 attributes.
        let semantics = vec![
            (DeclUsage::Position, 0),
            (DeclUsage::TexCoord, 0),
            (DeclUsage::TexCoord, 1),
            (DeclUsage::TexCoord, 2),
            (DeclUsage::TexCoord, 3),
            (DeclUsage::TexCoord, 4),
            (DeclUsage::TexCoord, 5),
            (DeclUsage::TexCoord, 6),
            (DeclUsage::TexCoord, 7),
            (DeclUsage::TexCoord, 8),
        ];

        let adaptive = AdaptiveLocationMap::new(semantics).unwrap();
        assert!(adaptive.location_for(DeclUsage::TexCoord, 8).is_ok());
    }

    #[test]
    fn adaptive_errors_when_over_budget() {
        // Create 17 distinct semantics (more than WebGPU's guaranteed 16 attributes).
        let semantics = (0u8..17u8)
            .map(|i| (DeclUsage::TexCoord, i))
            .collect::<Vec<_>>();

        let err = AdaptiveLocationMap::new(semantics).unwrap_err();
        assert!(
            matches!(err, LocationMapError::OutOfLocations { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn adaptive_allocates_uncommon_semantics_to_lowest_free_locations() {
        // Depth and Sample are valid D3D9 declaration usages, but they are uncommon and not part of
        // the StandardLocationMap. Ensure AdaptiveLocationMap allocates them deterministically to
        // the lowest free locations after reserving the standard assignments.
        let semantics = vec![
            (DeclUsage::Position, 0), // pinned -> 0
            (DeclUsage::TexCoord, 0), // pinned -> 8
            (DeclUsage::Depth, 0),    // allocated -> 1
            (DeclUsage::Sample, 0),   // allocated -> 2
        ];

        let adaptive = AdaptiveLocationMap::new(semantics).unwrap();
        assert_eq!(adaptive.location_for(DeclUsage::Position, 0).unwrap(), 0);
        assert_eq!(adaptive.location_for(DeclUsage::TexCoord, 0).unwrap(), 8);
        assert_eq!(adaptive.location_for(DeclUsage::Depth, 0).unwrap(), 1);
        assert_eq!(adaptive.location_for(DeclUsage::Sample, 0).unwrap(), 2);
    }
}
