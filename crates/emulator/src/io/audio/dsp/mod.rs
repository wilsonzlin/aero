pub mod mix;
pub mod pcm;
pub mod remix;
pub mod resample;

pub use pcm::{convert_f32_to_i16, f32_to_i16};

use pcm::{decode_interleaved_to_f32, PcmSpec};
use remix::remix_interleaved;
use resample::{Resampler, ResamplerKind};

#[derive(Debug)]
pub enum DspError {
    Pcm(pcm::PcmError),
    Remix(remix::RemixError),
    Resample(resample::ResampleError),
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
