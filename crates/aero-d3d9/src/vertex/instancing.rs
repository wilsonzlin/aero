use std::fmt;
use thiserror::Error;

/// D3D9 `SetStreamSourceFreq` flags.
const D3DSTREAMSOURCE_INDEXEDDATA: u32 = 0x4000_0000;
const D3DSTREAMSOURCE_INSTANCEDATA: u32 = 0x8000_0000;
const D3DSTREAMSOURCE_FREQUENCY_MASK: u32 = 0x3fff_ffff;

/// Raw `SetStreamSourceFreq` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSourceFreq(pub u32);

impl StreamSourceFreq {
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u32 {
        self.0
    }

    pub fn kind(self) -> Result<StreamFreqKind, StreamSourceFreqParseError> {
        let raw = self.0;
        let indexed = (raw & D3DSTREAMSOURCE_INDEXEDDATA) != 0;
        let instanced = (raw & D3DSTREAMSOURCE_INSTANCEDATA) != 0;

        if indexed && instanced {
            return Err(StreamSourceFreqParseError::BothFlagsSet { raw });
        }

        let freq = raw & D3DSTREAMSOURCE_FREQUENCY_MASK;
        if freq == 0 {
            return Err(StreamSourceFreqParseError::ZeroFrequency { raw });
        }

        if indexed {
            Ok(StreamFreqKind::IndexedData { instances: freq })
        } else if instanced {
            Ok(StreamFreqKind::InstanceData { divisor: freq })
        } else {
            Ok(StreamFreqKind::VertexData)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFreqKind {
    /// Per-vertex data (default).
    VertexData,
    /// Marks the "indexed data" stream and encodes the instance count.
    IndexedData { instances: u32 },
    /// Per-instance data stream; `divisor` controls how often to advance.
    InstanceData { divisor: u32 },
}

/// Derived step information for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStep {
    Vertex,
    Instance { divisor: u32 },
}

impl StreamStep {
    pub fn is_instance(self) -> bool {
        matches!(self, StreamStep::Instance { .. })
    }

    pub fn divisor(self) -> u32 {
        match self {
            StreamStep::Vertex => 0,
            StreamStep::Instance { divisor } => divisor,
        }
    }
}

/// `SetStreamSourceFreq` state for all streams.
///
/// D3D9 has 16 vertex streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamsFreqState {
    freqs: [StreamSourceFreq; 16],
}

impl Default for StreamsFreqState {
    fn default() -> Self {
        Self {
            freqs: [StreamSourceFreq(1); 16],
        }
    }
}

impl StreamsFreqState {
    pub fn set(&mut self, stream: u8, raw_freq: u32) -> Result<(), StreamSourceFreqParseError> {
        let Some(slot) = self.freqs.get_mut(stream as usize) else {
            return Err(StreamSourceFreqParseError::InvalidStream { stream });
        };

        // Validate now so downstream code can assume soundness.
        StreamSourceFreq(raw_freq).kind()?;
        *slot = StreamSourceFreq(raw_freq);
        Ok(())
    }

    pub fn get(&self, stream: u8) -> Option<StreamSourceFreq> {
        self.freqs.get(stream as usize).copied()
    }

    /// Compute derived instancing information.
    pub fn compute_stream_step(&self) -> Result<StreamStepState, StreamSourceFreqParseError> {
        let mut draw_instances = 1u32;
        let mut steps = [StreamStep::Vertex; 16];

        for (stream, &freq) in self.freqs.iter().enumerate() {
            match freq.kind()? {
                StreamFreqKind::VertexData => {}
                StreamFreqKind::IndexedData { instances } => {
                    if draw_instances != 1 && draw_instances != instances {
                        return Err(StreamSourceFreqParseError::ConflictingIndexedInstances {
                            existing: draw_instances,
                            new: instances,
                            stream: stream as u8,
                        });
                    }
                    draw_instances = instances;
                }
                StreamFreqKind::InstanceData { divisor } => {
                    steps[stream] = StreamStep::Instance { divisor };
                }
            }
        }

        Ok(StreamStepState {
            draw_instances,
            steps,
        })
    }
}

/// Per-stream instancing information derived from `SetStreamSourceFreq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStepState {
    draw_instances: u32,
    steps: [StreamStep; 16],
}

impl StreamStepState {
    pub fn draw_instances(&self) -> u32 {
        self.draw_instances
    }

    pub fn stream_step(&self, stream: u8) -> StreamStep {
        self.steps
            .get(stream as usize)
            .copied()
            .unwrap_or(StreamStep::Vertex)
    }

    pub fn needs_divisor_emulation(&self) -> bool {
        self.steps.iter().any(|s| match s {
            StreamStep::Instance { divisor } => *divisor != 1,
            StreamStep::Vertex => false,
        })
    }
}

impl fmt::Display for StreamStepState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "instances={}", self.draw_instances)?;
        for (i, step) in self.steps.iter().enumerate() {
            if *step != StreamStep::Vertex {
                write!(f, ", stream{i}={step:?}")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StreamSourceFreqParseError {
    #[error("stream index {stream} is out of range")]
    InvalidStream { stream: u8 },

    #[error(
        "SetStreamSourceFreq value has both INDEXEDDATA and INSTANCEDATA set (raw=0x{raw:08x})"
    )]
    BothFlagsSet { raw: u32 },

    #[error("SetStreamSourceFreq value has a zero frequency (raw=0x{raw:08x})")]
    ZeroFrequency { raw: u32 },

    #[error(
        "conflicting indexed instance counts: existing={existing} new={new} (stream={stream})"
    )]
    ConflictingIndexedInstances { existing: u32, new: u32, stream: u8 },
}

/// Expand per-instance stream data to emulate a non-1 divisor.
///
/// WebGPU only supports `stepMode: "instance"` with a divisor of 1. D3D9 encodes the divisor via
/// `D3DSTREAMSOURCE_INSTANCEDATA | divisor`.
///
/// This helper performs the simplest compatibility fallback: it duplicates each instance record
/// `divisor` times.
pub fn expand_instance_data(
    src: &[u8],
    stride: usize,
    divisor: u32,
    draw_instances: u32,
) -> Result<Vec<u8>, InstanceDataExpandError> {
    if stride == 0 {
        return Err(InstanceDataExpandError::ZeroStride);
    }
    if divisor == 0 {
        return Err(InstanceDataExpandError::ZeroDivisor);
    }

    let required_src_instances =
        ((draw_instances as u64) + (divisor as u64) - 1) / (divisor as u64);
    let required_src_bytes = (required_src_instances as usize)
        .checked_mul(stride)
        .ok_or(InstanceDataExpandError::SizeOverflow)?;
    if src.len() < required_src_bytes {
        return Err(InstanceDataExpandError::SourceTooSmall {
            expected: required_src_bytes,
            actual: src.len(),
        });
    }

    let dst_len = (draw_instances as usize)
        .checked_mul(stride)
        .ok_or(InstanceDataExpandError::SizeOverflow)?;
    let mut dst = vec![0u8; dst_len];

    for i in 0..draw_instances as usize {
        let src_idx = (i as u32 / divisor) as usize;
        let src_off = src_idx * stride;
        let dst_off = i * stride;
        dst[dst_off..dst_off + stride].copy_from_slice(&src[src_off..src_off + stride]);
    }

    Ok(dst)
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InstanceDataExpandError {
    #[error("instance data stride is zero")]
    ZeroStride,

    #[error("instance divisor is zero")]
    ZeroDivisor,

    #[error("instance expansion size overflow")]
    SizeOverflow,

    #[error("instance data buffer is too small: expected {expected} bytes, got {actual} bytes")]
    SourceTooSmall { expected: usize, actual: usize },
}
