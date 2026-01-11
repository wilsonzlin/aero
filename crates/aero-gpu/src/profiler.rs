use crate::upload::GpuCapabilities;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const QUERIES_PER_FRAME: u64 = 5;
const QUERY_SIZE_BYTES: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuBackendKind {
    #[serde(rename = "webgpu")]
    WebGpu,
    #[serde(rename = "webgl2")]
    WebGl2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTimestampPhase {
    UploadStart = 0,
    UploadEnd = 1,
    RenderPassStart = 2,
    RenderPassEnd = 3,
    SubmitEnd = 4,
}

impl GpuTimestampPhase {
    fn as_query_offset(self) -> u64 {
        self as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuProfilerConfig {
    /// How many frames of timestamps we keep in the resolve ring buffer.
    pub query_history_frames: u64,
    /// How often we attempt to read back timestamps. Larger values reduce the chance of stalling.
    pub readback_interval_frames: u64,
}

impl Default for GpuProfilerConfig {
    fn default() -> Self {
        Self {
            query_history_frames: 60,
            readback_interval_frames: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameTimingsReport {
    pub frame_index: u64,
    pub backend: GpuBackendKind,
    /// CPU time spent encoding command buffers, in microseconds.
    pub cpu_encode_us: u64,
    /// CPU time spent submitting work to the GPU, in microseconds.
    pub cpu_submit_us: u64,
    /// GPU time for the frame, in microseconds (requires timestamp queries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_us: Option<u64>,
}

struct InProgressFrame {
    frame_index: u64,
    slot: usize,
    cpu_encode_start: Instant,
}

struct PendingSubmitFrame {
    frame_index: u64,
    slot: usize,
    cpu_encode_time: Duration,
}

struct TimestampState {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    resolve_stride_bytes: u64,
    timestamp_period_ns: f64,
}

struct PendingReadback {
    cpu_report: FrameTimingsReport,
    completion: std::sync::Arc<std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>>,
}

/// Collects per-frame CPU and (optionally) GPU timings.
///
/// When timestamp queries are supported (and enabled on the device), the profiler records GPU
/// timestamps for key phases and periodically reads them back using a small staging buffer to avoid
/// per-frame stalls. When unsupported (e.g. WebGL2 fallback), GPU timings are omitted and only CPU
/// timings are reported.
pub struct GpuProfiler {
    backend: GpuBackendKind,
    config: GpuProfilerConfig,
    next_frame_index: u64,
    in_progress: Option<InProgressFrame>,
    pending_submit: Option<PendingSubmitFrame>,
    cpu_reports: Vec<Option<FrameTimingsReport>>,
    latest_cpu_report: Option<FrameTimingsReport>,
    latest_gpu_report: Option<FrameTimingsReport>,
    pending_readback: Option<PendingReadback>,
    timestamps: Option<TimestampState>,
}

impl GpuProfiler {
    pub fn new_cpu_only(backend: GpuBackendKind) -> Self {
        Self::new_internal(backend, None, None, GpuProfilerConfig::default())
    }

    pub fn new_wgpu(backend: GpuBackendKind, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::new_wgpu_with_config(backend, device, queue, GpuProfilerConfig::default())
    }

    pub fn new_wgpu_with_config(
        backend: GpuBackendKind,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: GpuProfilerConfig,
    ) -> Self {
        let caps = GpuCapabilities::from_device(device);
        if !caps.supports_timestamp_queries() {
            return Self::new_internal(backend, None, None, config);
        }

        Self::new_internal(backend, Some(device), Some(queue), config)
    }

    fn new_internal(
        backend: GpuBackendKind,
        device: Option<&wgpu::Device>,
        queue: Option<&wgpu::Queue>,
        config: GpuProfilerConfig,
    ) -> Self {
        assert!(
            config.query_history_frames > 0,
            "query_history_frames must be > 0"
        );
        assert!(
            config.readback_interval_frames > 0,
            "readback_interval_frames must be > 0"
        );

        let cpu_reports = vec![None; config.query_history_frames as usize];

        let timestamps = match (device, queue) {
            (Some(device), Some(queue))
                if device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
                    && device
                        .features()
                        .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) =>
            {
                let query_count = QUERIES_PER_FRAME * config.query_history_frames;
                let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("aero.gpu-profiler.timestamp-query-set"),
                    ty: wgpu::QueryType::Timestamp,
                    count: query_count as u32,
                });

                let bytes_per_frame = QUERIES_PER_FRAME * QUERY_SIZE_BYTES;
                let resolve_stride_bytes =
                    align_up(bytes_per_frame, wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT);
                let resolve_buffer_size = resolve_stride_bytes * config.query_history_frames;

                let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aero.gpu-profiler.timestamp-resolve-buffer"),
                    size: resolve_buffer_size,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aero.gpu-profiler.timestamp-readback-staging-buffer"),
                    size: bytes_per_frame,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });

                Some(TimestampState {
                    query_set,
                    resolve_buffer,
                    readback_buffer,
                    resolve_stride_bytes,
                    timestamp_period_ns: queue.get_timestamp_period() as f64,
                })
            }
            _ => None,
        };

        Self {
            backend,
            config,
            next_frame_index: 0,
            in_progress: None,
            pending_submit: None,
            cpu_reports,
            latest_cpu_report: None,
            latest_gpu_report: None,
            pending_readback: None,
            timestamps,
        }
    }

    /// Starts a new frame and (when enabled) tries to read back a previous frame's GPU timestamps.
    pub fn begin_frame(&mut self, device: Option<&wgpu::Device>, queue: Option<&wgpu::Queue>) {
        if self.in_progress.is_some() || self.pending_submit.is_some() {
            panic!("begin_frame called while a frame is still in progress");
        }

        if self.timestamps.is_some() {
            let device = device.expect("wgpu Device required when timestamp profiling is enabled");
            let queue = queue.expect("wgpu Queue required when timestamp profiling is enabled");
            self.poll(device);
            self.maybe_start_readback(device, queue);
        }

        let slot = (self.next_frame_index % self.config.query_history_frames) as usize;
        self.in_progress = Some(InProgressFrame {
            frame_index: self.next_frame_index,
            slot,
            cpu_encode_start: Instant::now(),
        });
    }

    pub fn mark_timestamp(&self, encoder: &mut wgpu::CommandEncoder, phase: GpuTimestampPhase) {
        let Some(timestamps) = &self.timestamps else {
            return;
        };
        let frame = self
            .in_progress
            .as_ref()
            .expect("mark_timestamp called outside an active frame");

        let query_index = self.query_index(frame.slot, phase);
        encoder.write_timestamp(&timestamps.query_set, query_index as u32);
    }

    pub fn mark_upload_start(&self, encoder: &mut wgpu::CommandEncoder) {
        self.mark_timestamp(encoder, GpuTimestampPhase::UploadStart);
    }

    pub fn mark_upload_end(&self, encoder: &mut wgpu::CommandEncoder) {
        self.mark_timestamp(encoder, GpuTimestampPhase::UploadEnd);
    }

    pub fn mark_render_pass_start(&self, encoder: &mut wgpu::CommandEncoder) {
        self.mark_timestamp(encoder, GpuTimestampPhase::RenderPassStart);
    }

    pub fn mark_render_pass_end(&self, encoder: &mut wgpu::CommandEncoder) {
        self.mark_timestamp(encoder, GpuTimestampPhase::RenderPassEnd);
    }

    /// Finalizes CPU encode timing and appends query resolve commands when available.
    ///
    /// This also writes the `SubmitEnd` GPU timestamp.
    pub fn end_encode(&mut self, encoder: &mut wgpu::CommandEncoder) {
        // "Submit end" is the end of the submitted command buffer, not the CPU-side submit call.
        self.mark_timestamp(encoder, GpuTimestampPhase::SubmitEnd);

        let frame = self
            .in_progress
            .take()
            .expect("end_encode called without begin_frame");

        let cpu_encode_time = frame.cpu_encode_start.elapsed();

        if let Some(timestamps) = &self.timestamps {
            let base_query = (frame.slot as u64) * QUERIES_PER_FRAME;
            let query_range = u32::try_from(base_query).expect("query index")
                ..u32::try_from(base_query + QUERIES_PER_FRAME).expect("query index");
            let offset = (frame.slot as u64) * timestamps.resolve_stride_bytes;
            encoder.resolve_query_set(
                &timestamps.query_set,
                query_range,
                &timestamps.resolve_buffer,
                offset,
            );
        }

        self.pending_submit = Some(PendingSubmitFrame {
            frame_index: frame.frame_index,
            slot: frame.slot,
            cpu_encode_time,
        });
    }

    pub fn submit(&mut self, queue: &wgpu::Queue, command_buffer: wgpu::CommandBuffer) {
        let frame = self
            .pending_submit
            .take()
            .expect("submit called without end_encode");

        let submit_start = Instant::now();
        queue.submit([command_buffer]);
        let cpu_submit_time = submit_start.elapsed();

        let report = FrameTimingsReport {
            frame_index: frame.frame_index,
            backend: self.backend,
            cpu_encode_us: duration_to_us(frame.cpu_encode_time),
            cpu_submit_us: duration_to_us(cpu_submit_time),
            gpu_us: None,
        };

        self.cpu_reports[frame.slot] = Some(report.clone());
        self.latest_cpu_report = Some(report);
        self.next_frame_index = self.next_frame_index.saturating_add(1);
    }

    pub fn get_frame_timings(&self) -> Option<FrameTimingsReport> {
        self.latest_gpu_report
            .clone()
            .or_else(|| self.latest_cpu_report.clone())
    }

    /// Drive completion of pending GPU readbacks without blocking.
    ///
    /// Call this periodically (e.g. once per frame). `begin_frame` already calls it when timestamp
    /// profiling is enabled.
    pub fn poll(&mut self, device: &wgpu::Device) {
        if self.timestamps.is_none() {
            return;
        }

        device.poll(wgpu::Maintain::Poll);
        self.maybe_finish_readback();
    }

    fn maybe_start_readback(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.pending_readback.is_some() {
            return;
        }

        let Some(timestamps) = &self.timestamps else {
            return;
        };

        if self.next_frame_index < self.config.query_history_frames {
            return;
        }
        if self.next_frame_index % self.config.readback_interval_frames != 0 {
            return;
        }

        let slot = (self.next_frame_index % self.config.query_history_frames) as usize;
        let Some(cpu_report) = self.cpu_reports[slot].clone() else {
            return;
        };

        let expected_frame_index = self.next_frame_index - self.config.query_history_frames;
        if cpu_report.frame_index != expected_frame_index {
            return;
        }

        let offset = (slot as u64) * timestamps.resolve_stride_bytes;
        let size = QUERIES_PER_FRAME * QUERY_SIZE_BYTES;

        // Copy from the resolve ring into the dedicated staging buffer, then map the staging buffer.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero.gpu-profiler.readback-copy"),
        });
        encoder.copy_buffer_to_buffer(
            &timestamps.resolve_buffer,
            offset,
            &timestamps.readback_buffer,
            0,
            size,
        );
        queue.submit([encoder.finish()]);

        let completion: std::sync::Arc<
            std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        let completion_callback = completion.clone();

        let slice = timestamps.readback_buffer.slice(0..size);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            *completion_callback.lock().expect("map completion lock") = Some(result);
        });

        self.pending_readback = Some(PendingReadback {
            cpu_report,
            completion,
        });
    }

    fn maybe_finish_readback(&mut self) {
        let pending = match self.pending_readback.as_ref() {
            Some(pending) => pending,
            None => return,
        };
        let Some(timestamps) = &self.timestamps else {
            return;
        };

        let result = pending
            .completion
            .lock()
            .expect("map completion lock")
            .take();

        let Some(result) = result else {
            return;
        };

        let report = match result {
            Ok(()) => {
                let size = QUERIES_PER_FRAME * QUERY_SIZE_BYTES;
                let slice = timestamps.readback_buffer.slice(0..size);
                let data = slice.get_mapped_range();

                let mut query_values = [0u64; QUERIES_PER_FRAME as usize];
                for (i, out) in query_values.iter_mut().enumerate() {
                    let start = i * QUERY_SIZE_BYTES as usize;
                    let end = start + QUERY_SIZE_BYTES as usize;
                    *out = u64::from_le_bytes(
                        data[start..end].try_into().expect("query readback slice"),
                    );
                }
                drop(data);

                let start = query_values[GpuTimestampPhase::UploadStart as usize];
                let end = query_values[GpuTimestampPhase::SubmitEnd as usize];

                let gpu_us = end
                    .checked_sub(start)
                    .map(|diff| diff as f64 * timestamps.timestamp_period_ns / 1000.0)
                    .map(|us| us.round().max(0.0) as u64);

                let mut report = pending.cpu_report.clone();
                report.gpu_us = gpu_us;
                report
            }
            Err(_) => pending.cpu_report.clone(),
        };

        timestamps.readback_buffer.unmap();
        self.pending_readback = None;
        self.latest_gpu_report = Some(report);
    }

    fn query_index(&self, slot: usize, phase: GpuTimestampPhase) -> u64 {
        (slot as u64) * QUERIES_PER_FRAME + phase.as_query_offset()
    }
}

fn duration_to_us(duration: Duration) -> u64 {
    let us = duration.as_micros();
    u64::try_from(us).unwrap_or(u64::MAX)
}

fn align_up(value: u64, align: u64) -> u64 {
    if align == 0 {
        return value;
    }
    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_timings_report_json_shape() {
        let report = FrameTimingsReport {
            frame_index: 12,
            backend: GpuBackendKind::WebGpu,
            cpu_encode_us: 100,
            cpu_submit_us: 7,
            gpu_us: Some(333),
        };

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(
            value,
            json!({
                "frame_index": 12,
                "backend": "webgpu",
                "cpu_encode_us": 100,
                "cpu_submit_us": 7,
                "gpu_us": 333,
            })
        );
    }

    #[test]
    fn frame_timings_report_omits_gpu_field_when_unavailable() {
        let report = FrameTimingsReport {
            frame_index: 0,
            backend: GpuBackendKind::WebGl2,
            cpu_encode_us: 1,
            cpu_submit_us: 2,
            gpu_us: None,
        };

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(
            value,
            json!({
                "frame_index": 0,
                "backend": "webgl2",
                "cpu_encode_us": 1,
                "cpu_submit_us": 2,
            })
        );
    }
}
