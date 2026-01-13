pub mod mix;
pub mod pcm;
pub mod remix;
pub mod resample;

pub use pcm::{convert_f32_to_i16, f32_to_i16};

use core::fmt;

use pcm::{decode_interleaved_to_f32, PcmSpec};
use remix::remix_interleaved;
use resample::{Resampler, ResamplerKind};

#[derive(Debug)]
pub enum DspError {
    Pcm(pcm::PcmError),
    Remix(remix::RemixError),
    Resample(resample::ResampleError),
}

impl fmt::Display for DspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pcm(e) => write!(f, "pcm: {e}"),
            Self::Remix(e) => write!(f, "remix: {e}"),
            Self::Resample(e) => write!(f, "resample: {e}"),
        }
    }
}

impl std::error::Error for DspError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pcm(e) => Some(e),
            Self::Remix(e) => Some(e),
            Self::Resample(e) => Some(e),
        }
    }
}

impl From<pcm::PcmError> for DspError {
    fn from(value: pcm::PcmError) -> Self {
        Self::Pcm(value)
    }
}

impl From<remix::RemixError> for DspError {
    fn from(value: remix::RemixError) -> Self {
        Self::Remix(value)
    }
}

impl From<resample::ResampleError> for DspError {
    fn from(value: resample::ResampleError) -> Self {
        Self::Resample(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineOrder {
    None,
    RemixOnly,
    ResampleOnly,
    RemixThenResample,
    ResampleThenRemix,
}

/// Convert a guest PCM stream into a Web Audio-friendly stream: interleaved `f32`,
/// fixed output channels, and fixed output sample rate.
///
/// Designed for reuse in a hot path:
/// - Scratch buffers are held in the struct and reused.
/// - Resampling preserves phase across calls.
pub struct StreamProcessor {
    input: PcmSpec,
    output_sample_rate: u32,
    output_channels: usize,

    order: PipelineOrder,
    resampler: Option<Resampler>,

    // Scratch buffers (interleaved)
    decode_buf: Vec<f32>,
    tmp_a: Vec<f32>,
    tmp_b: Vec<f32>,
}

impl StreamProcessor {
    pub fn new(
        input: PcmSpec,
        output_sample_rate: u32,
        output_channels: usize,
        resampler_kind: ResamplerKind,
    ) -> Result<Self, DspError> {
        let needs_resample = input.sample_rate != output_sample_rate;
        let needs_remix = input.channels != output_channels;

        let order = match (needs_remix, needs_resample) {
            (false, false) => PipelineOrder::None,
            (true, false) => PipelineOrder::RemixOnly,
            (false, true) => PipelineOrder::ResampleOnly,
            (true, true) => {
                if input.channels > output_channels {
                    PipelineOrder::RemixThenResample
                } else {
                    PipelineOrder::ResampleThenRemix
                }
            }
        };

        let resample_channels = match order {
            PipelineOrder::RemixThenResample => output_channels,
            PipelineOrder::ResampleThenRemix | PipelineOrder::ResampleOnly => input.channels,
            PipelineOrder::None | PipelineOrder::RemixOnly => 0,
        };

        let resampler = if needs_resample {
            Some(Resampler::new(
                resampler_kind,
                input.sample_rate,
                output_sample_rate,
                resample_channels,
            )?)
        } else {
            None
        };

        Ok(Self {
            input,
            output_sample_rate,
            output_channels,
            order,
            resampler,
            decode_buf: Vec::new(),
            tmp_a: Vec::new(),
            tmp_b: Vec::new(),
        })
    }

    #[inline]
    pub fn input_spec(&self) -> PcmSpec {
        self.input
    }

    #[inline]
    pub fn output_sample_rate(&self) -> u32 {
        self.output_sample_rate
    }

    #[inline]
    pub fn output_channels(&self) -> usize {
        self.output_channels
    }

    pub fn reset(&mut self) {
        if let Some(r) = &mut self.resampler {
            r.reset();
        }
        self.decode_buf.clear();
        self.tmp_a.clear();
        self.tmp_b.clear();
    }

    /// Decode + resample + remix one input block.
    ///
    /// `out` is cleared and overwritten.
    pub fn process(&mut self, input_bytes: &[u8], out: &mut Vec<f32>) -> Result<(), DspError> {
        match self.order {
            PipelineOrder::None => {
                decode_interleaved_to_f32(
                    input_bytes,
                    self.input.format,
                    self.input.channels,
                    out,
                )?;
                Ok(())
            }
            PipelineOrder::RemixOnly => {
                decode_interleaved_to_f32(
                    input_bytes,
                    self.input.format,
                    self.input.channels,
                    &mut self.decode_buf,
                )?;
                remix_interleaved(
                    &self.decode_buf,
                    self.input.channels,
                    self.output_channels,
                    out,
                )?;
                Ok(())
            }
            PipelineOrder::ResampleOnly => {
                decode_interleaved_to_f32(
                    input_bytes,
                    self.input.format,
                    self.input.channels,
                    &mut self.decode_buf,
                )?;
                let r = self.resampler.as_mut().expect("resampler present");
                r.process_interleaved(&self.decode_buf, out)?;
                Ok(())
            }
            PipelineOrder::RemixThenResample => {
                decode_interleaved_to_f32(
                    input_bytes,
                    self.input.format,
                    self.input.channels,
                    &mut self.decode_buf,
                )?;
                remix_interleaved(
                    &self.decode_buf,
                    self.input.channels,
                    self.output_channels,
                    &mut self.tmp_a,
                )?;
                let r = self.resampler.as_mut().expect("resampler present");
                r.process_interleaved(&self.tmp_a, out)?;
                Ok(())
            }
            PipelineOrder::ResampleThenRemix => {
                decode_interleaved_to_f32(
                    input_bytes,
                    self.input.format,
                    self.input.channels,
                    &mut self.decode_buf,
                )?;
                let r = self.resampler.as_mut().expect("resampler present");
                r.process_interleaved(&self.decode_buf, &mut self.tmp_a)?;
                remix_interleaved(&self.tmp_a, self.input.channels, self.output_channels, out)?;
                Ok(())
            }
        }
    }

    /// Flush any tail samples at end-of-stream.
    ///
    /// `out` is cleared and overwritten.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), DspError> {
        out.clear();
        let Some(r) = &mut self.resampler else {
            return Ok(());
        };

        match self.order {
            PipelineOrder::ResampleOnly | PipelineOrder::RemixThenResample => {
                r.flush_interleaved(out);
                Ok(())
            }
            PipelineOrder::ResampleThenRemix => {
                r.flush_interleaved(&mut self.tmp_b);
                remix_interleaved(&self.tmp_b, self.input.channels, self.output_channels, out)?;
                Ok(())
            }
            PipelineOrder::None | PipelineOrder::RemixOnly => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::audio::dsp::pcm::{PcmSampleFormat, PcmSpec};

    fn make_i16_pcm_bytes(frames: usize, channels: usize) -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        let mut bytes = Vec::with_capacity(frames * channels * 2);
        for _ in 0..frames * channels {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let s = (state >> 16) as i16;
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        bytes
    }

    fn process_single_shot(sp: &mut StreamProcessor, input_bytes: &[u8]) -> Vec<f32> {
        let mut out = Vec::new();
        let mut tail = Vec::new();
        sp.process(input_bytes, &mut out).unwrap();
        sp.flush(&mut tail).unwrap();
        out.extend_from_slice(&tail);
        out
    }

    fn process_chunked(
        sp: &mut StreamProcessor,
        input_bytes: &[u8],
        input_channels: usize,
        chunk_frames: &[usize],
    ) -> Vec<f32> {
        assert!(!chunk_frames.is_empty());

        let bytes_per_frame = input_channels * PcmSampleFormat::I16.bytes_per_sample();
        assert!(input_bytes.len().is_multiple_of(bytes_per_frame));

        let mut out_all = Vec::new();
        let mut tmp = Vec::new();

        let mut offset = 0usize;
        let mut chunk_idx = 0usize;
        while offset < input_bytes.len() {
            let remaining_frames = (input_bytes.len() - offset) / bytes_per_frame;
            let want = chunk_frames[chunk_idx % chunk_frames.len()];
            let n = want.min(remaining_frames);
            let len = n * bytes_per_frame;
            sp.process(&input_bytes[offset..offset + len], &mut tmp)
                .unwrap();
            out_all.extend_from_slice(&tmp);
            offset += len;
            chunk_idx += 1;
        }

        sp.flush(&mut tmp).unwrap();
        out_all.extend_from_slice(&tmp);
        out_all
    }

    #[test]
    fn stream_processor_chunked_equals_single_shot() {
        let frames = 1024usize;
        let input_channels = 2usize;
        let bytes = make_i16_pcm_bytes(frames, input_channels);

        let input = PcmSpec {
            format: PcmSampleFormat::I16,
            channels: input_channels,
            sample_rate: 44_100,
        };

        // Exercise both the "resample only" and "remix then resample" pipeline orders.
        for output_channels in [2usize, 1usize] {
            let mut sp = StreamProcessor::new(input, 48_000, output_channels, ResamplerKind::Linear)
                .unwrap();
            let out_single = process_single_shot(&mut sp, &bytes);
            assert!(out_single.len().is_multiple_of(output_channels));

            let mut sp = StreamProcessor::new(input, 48_000, output_channels, ResamplerKind::Linear)
                .unwrap();
            let out_chunked = process_chunked(&mut sp, &bytes, input_channels, &[7, 13, 64]);
            assert!(out_chunked.len().is_multiple_of(output_channels));

            assert_eq!(
                out_chunked, out_single,
                "StreamProcessor output differs across chunk boundaries (output_channels={output_channels})"
            );
        }
    }
}
