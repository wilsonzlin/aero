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
    use pcm::PcmSampleFormat;

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

    fn f32s_to_le_bytes(samples: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for &s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        bytes
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
            let mut sp =
                StreamProcessor::new(input, 48_000, output_channels, ResamplerKind::Linear)
                    .unwrap();
            let out_single = process_single_shot(&mut sp, &bytes);
            assert!(out_single.len().is_multiple_of(output_channels));

            let mut sp =
                StreamProcessor::new(input, 48_000, output_channels, ResamplerKind::Linear)
                    .unwrap();
            let out_chunked = process_chunked(&mut sp, &bytes, input_channels, &[7, 13, 64]);
            assert!(out_chunked.len().is_multiple_of(output_channels));

            assert_eq!(
                out_chunked, out_single,
                "StreamProcessor output differs across chunk boundaries (output_channels={output_channels})"
            );
        }
    }

    #[test]
    fn stream_processor_process_none_decodes_only() {
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 2,
            sample_rate: 48_000,
        };
        let mut p = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

        let samples = [0.25f32, -0.5, 0.25, -0.5];
        let bytes = f32s_to_le_bytes(&samples);
        let mut out = Vec::new();
        p.process(&bytes, &mut out).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn stream_processor_process_remix_only() {
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 3,
            sample_rate: 48_000,
        };
        let mut p = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

        // Two identical frames: [L, R, C]
        let samples = [1.0f32, 2.0, 4.0, 1.0, 2.0, 4.0];
        let bytes = f32s_to_le_bytes(&samples);
        let mut out = Vec::new();
        p.process(&bytes, &mut out).unwrap();
        assert_eq!(out, vec![3.0, 4.0, 3.0, 4.0]);
    }

    #[test]
    fn stream_processor_process_resample_only() {
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 2,
            sample_rate: 44_100,
        };
        let mut p = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

        // Two identical frames.
        let samples = [0.5f32, -0.25, 0.5, -0.25];
        let bytes = f32s_to_le_bytes(&samples);
        let mut out = Vec::new();
        p.process(&bytes, &mut out).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn stream_processor_process_resample_then_remix() {
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 1,
            sample_rate: 44_100,
        };
        let mut p = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

        // Two identical mono frames.
        let samples = [0.25f32, 0.25];
        let bytes = f32s_to_le_bytes(&samples);
        let mut out = Vec::new();
        p.process(&bytes, &mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25, 0.25, 0.25]);
    }

    #[test]
    fn stream_processor_process_remix_then_resample() {
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 3,
            sample_rate: 44_100,
        };
        let mut p = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

        // Two identical frames: [L, R, C]
        let samples = [1.0f32, 2.0, 4.0, 1.0, 2.0, 4.0];
        let bytes = f32s_to_le_bytes(&samples);
        let mut out = Vec::new();
        p.process(&bytes, &mut out).unwrap();
        assert_eq!(out, vec![3.0, 4.0, 3.0, 4.0]);
    }

    #[test]
    fn stream_processor_flush_clears_output_when_no_resampler() {
        // PipelineOrder::None.
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 2,
            sample_rate: 1,
        };
        let mut sp = StreamProcessor::new(input, 1, 2, ResamplerKind::Linear).unwrap();
        let mut out = vec![1.0f32, 2.0, 3.0];
        sp.flush(&mut out).unwrap();
        assert!(out.is_empty());

        // PipelineOrder::RemixOnly.
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 2,
            sample_rate: 1,
        };
        let mut sp = StreamProcessor::new(input, 1, 1, ResamplerKind::Linear).unwrap();
        let mut out = vec![1.0f32, 2.0, 3.0];
        sp.flush(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn stream_processor_flush_produces_deterministic_tail_samples() {
        // Choose tiny sample rates so the linear resampler step is exactly 0.5, making the
        // tail length deterministic for a short constant-valued input.
        let input_frames = [0.25f32, 0.25];
        let input_bytes = f32s_to_le_bytes(&input_frames);

        // PipelineOrder::ResampleOnly (no remix).
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 1,
            sample_rate: 1,
        };
        let mut sp = StreamProcessor::new(input, 2, 1, ResamplerKind::Linear).unwrap();
        let mut out = Vec::new();
        sp.process(&input_bytes, &mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25, 0.25]);
        sp.flush(&mut out).unwrap();
        assert_eq!(out, vec![0.25]);

        // PipelineOrder::ResampleThenRemix (upmix in flush).
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 1,
            sample_rate: 1,
        };
        let mut sp = StreamProcessor::new(input, 2, 2, ResamplerKind::Linear).unwrap();
        let mut out = Vec::new();
        sp.process(&input_bytes, &mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25, 0.25, 0.25, 0.25, 0.25]);
        sp.flush(&mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25]);

        // PipelineOrder::RemixThenResample (downmix before flush).
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 2,
            sample_rate: 1,
        };
        let stereo_frames = [0.25f32, 0.25, 0.25, 0.25];
        let stereo_bytes = f32s_to_le_bytes(&stereo_frames);
        let mut sp = StreamProcessor::new(input, 2, 1, ResamplerKind::Linear).unwrap();
        let mut out = Vec::new();
        sp.process(&stereo_bytes, &mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25, 0.25]);
        sp.flush(&mut out).unwrap();
        assert_eq!(out, vec![0.25]);
    }

    #[test]
    fn stream_processor_reset_discards_resampler_history() {
        // Use tiny sample rates so the linear resampler step is exactly 0.5.
        let input = PcmSpec {
            format: PcmSampleFormat::F32,
            channels: 1,
            sample_rate: 1,
        };

        // Two input blocks; the second block's first output depends on the previous block's
        // last sample unless we reset.
        let block_a = f32s_to_le_bytes(&[0.0f32, 1.0]);
        let block_b = f32s_to_le_bytes(&[1.0f32, 0.0]);

        // With continuous state, the second block begins with interpolation between the last
        // sample of A (1.0) and the first sample of B (1.0), yielding an initial 1.0 sample
        // before the block's own frames.
        let mut sp = StreamProcessor::new(input, 2, 1, ResamplerKind::Linear).unwrap();
        let mut out_a = Vec::new();
        sp.process(&block_a, &mut out_a).unwrap();
        assert_eq!(out_a, vec![0.0, 0.5, 1.0]);

        let mut out_b = Vec::new();
        sp.process(&block_b, &mut out_b).unwrap();
        assert_eq!(out_b, vec![1.0, 1.0, 0.5, 0.0]);

        let mut tail = Vec::new();
        sp.flush(&mut tail).unwrap();
        assert_eq!(tail, vec![0.0]);

        // After reset, the resampler should behave as if starting fresh on the next block.
        let mut sp = StreamProcessor::new(input, 2, 1, ResamplerKind::Linear).unwrap();
        let mut tmp = Vec::new();
        sp.process(&block_a, &mut tmp).unwrap();
        sp.reset();

        let mut out_b = Vec::new();
        sp.process(&block_b, &mut out_b).unwrap();
        assert_eq!(out_b, vec![1.0, 0.5, 0.0]);

        let mut tail = Vec::new();
        sp.flush(&mut tail).unwrap();
        assert_eq!(tail, vec![0.0]);
    }
}
