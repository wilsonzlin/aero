//! Legacy virtio-snd device model.
//!
//! This implementation predates the `aero-virtio` virtio stack and is retained
//! behind the `emulator/legacy-audio` feature for reference and targeted tests.
//! The canonical virtio-snd device model lives in `crates/aero-virtio`.

use crate::audio::worklet::AudioSink;
use crate::io::virtio::vio_core::{
    Descriptor, DescriptorChain, VirtQueue, VirtQueueError, VRING_DESC_F_WRITE,
};
use memory::{GuestMemory, GuestMemoryError};

pub const VIRTIO_ID_SND: u16 = 25;

pub const VIRTIO_SND_R_JACK_INFO: u32 = 0x0001;
pub const VIRTIO_SND_R_JACK_REMAP: u32 = 0x0002;
pub const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
pub const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0101;
pub const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0102;
pub const VIRTIO_SND_R_PCM_RELEASE: u32 = 0x0103;
pub const VIRTIO_SND_R_PCM_START: u32 = 0x0104;
pub const VIRTIO_SND_R_PCM_STOP: u32 = 0x0105;
pub const VIRTIO_SND_R_CHMAP_INFO: u32 = 0x0200;

pub const VIRTIO_SND_S_OK: u32 = 0x0000;
pub const VIRTIO_SND_S_BAD_MSG: u32 = 0x0001;
pub const VIRTIO_SND_S_NOT_SUPP: u32 = 0x0002;
pub const VIRTIO_SND_S_IO_ERR: u32 = 0x0003;

pub const VIRTIO_SND_D_OUTPUT: u8 = 0x00;

pub const VIRTIO_SND_PCM_FMT_S16: u8 = 0x05;
pub const VIRTIO_SND_PCM_RATE_48000: u8 = 0x07;

pub const VIRTIO_SND_PCM_FMT_MASK_S16: u64 = 1u64 << VIRTIO_SND_PCM_FMT_S16;
pub const VIRTIO_SND_PCM_RATE_MASK_48000: u64 = 1u64 << VIRTIO_SND_PCM_RATE_48000;

pub const PLAYBACK_STREAM_ID: u32 = 0;

const MAX_CONTROL_MSG_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioSndConfig {
    pub jacks: u32,
    pub streams: u32,
    pub chmaps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmParams {
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Idle,
    ParamsSet,
    Prepared,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PcmStream {
    params: Option<PcmParams>,
    state: StreamState,
}

impl Default for PcmStream {
    fn default() -> Self {
        Self {
            params: None,
            state: StreamState::Idle,
        }
    }
}

#[derive(Debug)]
pub struct VirtioSndDevice<S: AudioSink> {
    pub config: VirtioSndConfig,
    pub control_vq: VirtQueue,
    pub tx_vq: VirtQueue,

    sink: S,
    playback: PcmStream,
    isr_queue: bool,
}

impl<S: AudioSink> VirtioSndDevice<S> {
    pub fn new(sink: S, control_vq: VirtQueue, tx_vq: VirtQueue) -> Self {
        Self {
            config: VirtioSndConfig {
                jacks: 0,
                streams: 1,
                chmaps: 0,
            },
            control_vq,
            tx_vq,
            sink,
            playback: PcmStream::default(),
            isr_queue: false,
        }
    }

    pub fn take_isr(&mut self) -> u8 {
        let isr = if self.isr_queue { 0x1 } else { 0x0 };
        self.isr_queue = false;
        isr
    }

    pub fn process_control(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.control_vq.pop_available(mem)? {
            let response = self.handle_control_chain(mem, &chain)?;
            let written = write_in_chain(mem, &chain.descriptors, &response)?;

            if self.control_vq.push_used(mem, &chain, written as u32)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    pub fn process_tx(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.tx_vq.pop_available(mem)? {
            let status = self.handle_tx_chain(mem, &chain)?;
            let response = virtio_snd_pcm_status(status, 0);
            let written = write_in_chain(mem, &chain.descriptors, &response)?;

            if self.tx_vq.push_used(mem, &chain, written as u32)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    fn handle_control_chain(
        &mut self,
        mem: &impl GuestMemory,
        chain: &DescriptorChain,
    ) -> Result<Vec<u8>, VirtQueueError> {
        let request = read_out_chain(mem, &chain.descriptors)?;
        if request.len() < 4 {
            return Ok(virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG));
        }

        let code = u32::from_le_bytes(request[0..4].try_into().unwrap());
        let resp = match code {
            VIRTIO_SND_R_PCM_INFO => self.cmd_pcm_info(&request),
            VIRTIO_SND_R_PCM_SET_PARAMS => self.cmd_pcm_set_params(&request),
            VIRTIO_SND_R_PCM_PREPARE => self.cmd_pcm_simple(&request, StreamSimpleCmd::Prepare),
            VIRTIO_SND_R_PCM_RELEASE => self.cmd_pcm_simple(&request, StreamSimpleCmd::Release),
            VIRTIO_SND_R_PCM_START => self.cmd_pcm_simple(&request, StreamSimpleCmd::Start),
            VIRTIO_SND_R_PCM_STOP => self.cmd_pcm_simple(&request, StreamSimpleCmd::Stop),
            VIRTIO_SND_R_JACK_INFO | VIRTIO_SND_R_JACK_REMAP | VIRTIO_SND_R_CHMAP_INFO => {
                virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP)
            }
            _ => virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP),
        };
        Ok(resp)
    }

    fn handle_tx_chain(
        &mut self,
        mem: &impl GuestMemory,
        chain: &DescriptorChain,
    ) -> Result<u32, VirtQueueError> {
        let mem_size = mem.size();
        let mut header = [0u8; 8];
        let mut header_len = 0usize;
        let mut read_buf = [0u8; 4096];
        let mut samples_buf = [0f32; 1024];
        let mut samples_len = 0usize;
        let mut pending_lo: Option<u8> = None;
        let mut sample_count = 0usize;
        let mut parsed_stream = false;

        for desc in chain
            .descriptors
            .iter()
            .filter(|d| d.flags & VRING_DESC_F_WRITE == 0)
        {
            let mut addr = desc.addr;
            let mut remaining = desc.len as usize;

            if header_len < header.len() {
                let to_read = remaining.min(header.len() - header_len);
                mem.read_into(addr, &mut header[header_len..header_len + to_read])?;
                header_len += to_read;
                addr = addr
                    .checked_add(to_read as u64)
                    .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                        paddr: addr,
                        len: to_read,
                        size: mem_size,
                    }))?;
                remaining -= to_read;

                if header_len < header.len() {
                    continue;
                }

                let stream_id = u32::from_le_bytes(header[0..4].try_into().unwrap());
                if stream_id != PLAYBACK_STREAM_ID {
                    return Ok(VIRTIO_SND_S_BAD_MSG);
                }

                if self.playback.state != StreamState::Running {
                    return Ok(VIRTIO_SND_S_IO_ERR);
                }

                parsed_stream = true;
            }

            while remaining > 0 {
                let to_read = remaining.min(read_buf.len());
                mem.read_into(addr, &mut read_buf[..to_read])?;

                for &b in &read_buf[..to_read] {
                    if let Some(lo) = pending_lo.take() {
                        let sample = i16::from_le_bytes([lo, b]);
                        samples_buf[samples_len] = sample as f32 / 32768.0;
                        samples_len += 1;
                        sample_count += 1;

                        if samples_len == samples_buf.len() {
                            let expected_frames = (samples_len / 2) as u32;
                            if self.sink.push_stereo_f32(&samples_buf) != expected_frames {
                                return Ok(VIRTIO_SND_S_IO_ERR);
                            }
                            samples_len = 0;
                        }
                    } else {
                        pending_lo = Some(b);
                    }
                }

                addr = addr
                    .checked_add(to_read as u64)
                    .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                        paddr: addr,
                        len: to_read,
                        size: mem_size,
                    }))?;
                remaining -= to_read;
            }
        }

        if !parsed_stream || header_len != header.len() {
            return Ok(VIRTIO_SND_S_BAD_MSG);
        }

        if pending_lo.is_some() || !sample_count.is_multiple_of(2) {
            return Ok(VIRTIO_SND_S_BAD_MSG);
        }

        if samples_len > 0 {
            let expected_frames = (samples_len / 2) as u32;
            if self.sink.push_stereo_f32(&samples_buf[..samples_len]) != expected_frames {
                return Ok(VIRTIO_SND_S_IO_ERR);
            }
        }

        Ok(VIRTIO_SND_S_OK)
    }

    fn cmd_pcm_info(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 12 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let start_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let count = u32::from_le_bytes(request[8..12].try_into().unwrap());

        let mut resp = virtio_snd_hdr(VIRTIO_SND_S_OK);
        if count == 0 {
            return resp;
        }

        if start_id == PLAYBACK_STREAM_ID {
            resp.extend_from_slice(&virtio_snd_pcm_info());
        }

        resp
    }

    fn cmd_pcm_set_params(&mut self, request: &[u8]) -> Vec<u8> {
        if request.len() < 24 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let stream_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        if stream_id != PLAYBACK_STREAM_ID {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let buffer_bytes = u32::from_le_bytes(request[8..12].try_into().unwrap());
        let period_bytes = u32::from_le_bytes(request[12..16].try_into().unwrap());
        let channels = request[20];
        let format = request[21];
        let rate = request[22];

        if channels != 2 || format != VIRTIO_SND_PCM_FMT_S16 || rate != VIRTIO_SND_PCM_RATE_48000 {
            return virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP);
        }

        self.playback.params = Some(PcmParams {
            buffer_bytes,
            period_bytes,
            channels,
            format,
            rate,
        });
        self.playback.state = StreamState::ParamsSet;

        virtio_snd_hdr(VIRTIO_SND_S_OK)
    }

    fn cmd_pcm_simple(&mut self, request: &[u8], cmd: StreamSimpleCmd) -> Vec<u8> {
        if request.len() < 8 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let stream_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        if stream_id != PLAYBACK_STREAM_ID {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let status = match cmd {
            StreamSimpleCmd::Prepare => match self.playback.state {
                StreamState::ParamsSet | StreamState::Prepared => {
                    self.playback.state = StreamState::Prepared;
                    VIRTIO_SND_S_OK
                }
                StreamState::Running | StreamState::Idle => VIRTIO_SND_S_IO_ERR,
            },
            StreamSimpleCmd::Release => {
                self.playback.params = None;
                self.playback.state = StreamState::Idle;
                VIRTIO_SND_S_OK
            }
            StreamSimpleCmd::Start => match self.playback.state {
                StreamState::Prepared => {
                    self.playback.state = StreamState::Running;
                    VIRTIO_SND_S_OK
                }
                StreamState::Running => VIRTIO_SND_S_OK,
                StreamState::Idle | StreamState::ParamsSet => VIRTIO_SND_S_IO_ERR,
            },
            StreamSimpleCmd::Stop => match self.playback.state {
                StreamState::Running => {
                    self.playback.state = StreamState::Prepared;
                    VIRTIO_SND_S_OK
                }
                _ => VIRTIO_SND_S_IO_ERR,
            },
        };

        virtio_snd_hdr(status)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamSimpleCmd {
    Prepare,
    Release,
    Start,
    Stop,
}

fn virtio_snd_hdr(code: u32) -> Vec<u8> {
    code.to_le_bytes().to_vec()
}

fn virtio_snd_pcm_info() -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..4].copy_from_slice(&0u32.to_le_bytes());
    buf[4..8].copy_from_slice(&0u32.to_le_bytes());
    buf[8..16].copy_from_slice(&VIRTIO_SND_PCM_FMT_MASK_S16.to_le_bytes());
    buf[16..24].copy_from_slice(&VIRTIO_SND_PCM_RATE_MASK_48000.to_le_bytes());
    buf[24] = VIRTIO_SND_D_OUTPUT;
    buf[25] = 2;
    buf[26] = 2;
    buf
}

fn virtio_snd_pcm_status(status: u32, latency_bytes: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&status.to_le_bytes());
    buf[4..8].copy_from_slice(&latency_bytes.to_le_bytes());
    buf
}

fn read_out_chain(mem: &impl GuestMemory, descs: &[Descriptor]) -> Result<Vec<u8>, VirtQueueError> {
    let mut total: usize = 0;
    for desc in descs.iter().filter(|d| d.flags & VRING_DESC_F_WRITE == 0) {
        total = match total.checked_add(desc.len as usize) {
            Some(total) => total,
            None => return Ok(Vec::new()),
        };
        if total > MAX_CONTROL_MSG_BYTES {
            return Ok(Vec::new());
        }
    }
    let mut buf = Vec::new();
    if buf.try_reserve_exact(total).is_err() {
        return Ok(Vec::new());
    }
    buf.resize(total, 0);
    let mut offset = 0usize;
    for desc in descs.iter().filter(|d| d.flags & VRING_DESC_F_WRITE == 0) {
        let len = desc.len as usize;
        let end = offset + len;
        mem.read_into(desc.addr, &mut buf[offset..end])?;
        offset = end;
    }
    Ok(buf)
}

fn write_in_chain(
    mem: &mut impl GuestMemory,
    descs: &[Descriptor],
    data: &[u8],
) -> Result<usize, VirtQueueError> {
    let mut remaining = data;
    let mut written = 0usize;
    for desc in descs.iter().filter(|d| d.flags & VRING_DESC_F_WRITE != 0) {
        if remaining.is_empty() {
            break;
        }
        let to_write = usize::min(desc.len as usize, remaining.len());
        mem.write_from(desc.addr, &remaining[..to_write])?;
        remaining = &remaining[to_write..];
        written += to_write;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::worklet::AudioWorkletRingBuffer;
    use crate::io::virtio::vio_core::VRING_DESC_F_NEXT;
    use memory::DenseMemory;

    fn write_desc(mem: &mut DenseMemory, base: u64, index: u16, desc: Descriptor) {
        let off = base + (index as u64) * 16;
        mem.write_u64_le(off, desc.addr).unwrap();
        mem.write_u32_le(off + 8, desc.len).unwrap();
        mem.write_u16_le(off + 12, desc.flags).unwrap();
        mem.write_u16_le(off + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, avail: u64, flags: u16, idx: u16, heads: &[u16]) {
        mem.write_u16_le(avail, flags).unwrap();
        mem.write_u16_le(avail + 2, idx).unwrap();
        for (i, head) in heads.iter().enumerate() {
            mem.write_u16_le(avail + 4 + (i as u64) * 2, *head).unwrap();
        }
    }

    fn init_used(mem: &mut DenseMemory, used: u64) {
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();
    }

    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn assert_f32_eq(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn pcm_info_reports_single_playback_stream() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let req_addr = 0x110;
        let resp_addr = 0x200;

        let mut req = Vec::new();
        req.extend_from_slice(&le32(VIRTIO_SND_R_PCM_INFO));
        req.extend_from_slice(&le32(0));
        req.extend_from_slice(&le32(1));
        mem.write_from(req_addr, &req).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: req_addr,
                len: req.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: resp_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 1, &[0]);
        init_used(&mut mem, used);

        let control_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        let irq = dev.process_control(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut resp = vec![0u8; 4 + 32];
        mem.read_into(resp_addr, &mut resp).unwrap();
        assert_eq!(
            u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            VIRTIO_SND_S_OK
        );
    }

    #[test]
    fn pcm_set_params_transitions_stream_state() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let set_params_addr = 0x110;
        let prepare_addr = 0x200;
        let start_addr = 0x300;
        let resp_addr = 0x400;

        let mut set_params = Vec::new();
        set_params.extend_from_slice(&le32(VIRTIO_SND_R_PCM_SET_PARAMS));
        set_params.extend_from_slice(&le32(PLAYBACK_STREAM_ID));
        set_params.extend_from_slice(&le32(4096));
        set_params.extend_from_slice(&le32(1024));
        set_params.extend_from_slice(&le32(0));
        set_params.push(2);
        set_params.push(VIRTIO_SND_PCM_FMT_S16);
        set_params.push(VIRTIO_SND_PCM_RATE_48000);
        set_params.push(0);
        mem.write_from(set_params_addr, &set_params).unwrap();

        let mut prepare = Vec::new();
        prepare.extend_from_slice(&le32(VIRTIO_SND_R_PCM_PREPARE));
        prepare.extend_from_slice(&le32(PLAYBACK_STREAM_ID));
        mem.write_from(prepare_addr, &prepare).unwrap();

        let mut start = Vec::new();
        start.extend_from_slice(&le32(VIRTIO_SND_R_PCM_START));
        start.extend_from_slice(&le32(PLAYBACK_STREAM_ID));
        mem.write_from(start_addr, &start).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: set_params_addr,
                len: set_params.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: resp_addr,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: prepare_addr,
                len: prepare.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: resp_addr + 0x20,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        write_desc(
            &mut mem,
            desc_table,
            4,
            Descriptor {
                addr: start_addr,
                len: start.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 5,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            5,
            Descriptor {
                addr: resp_addr + 0x40,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 3, &[0, 2, 4]);
        init_used(&mut mem, used);

        let control_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        dev.process_control(&mut mem).unwrap();

        assert_eq!(dev.playback.state, StreamState::Running);
    }

    #[test]
    fn pcm_set_params_rejects_unsupported_format() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let req_addr = 0x110;
        let resp_addr = 0x200;

        let mut set_params = Vec::new();
        set_params.extend_from_slice(&le32(VIRTIO_SND_R_PCM_SET_PARAMS));
        set_params.extend_from_slice(&le32(PLAYBACK_STREAM_ID));
        set_params.extend_from_slice(&le32(4096));
        set_params.extend_from_slice(&le32(1024));
        set_params.extend_from_slice(&le32(0));
        set_params.push(1); // channels (unsupported)
        set_params.push(VIRTIO_SND_PCM_FMT_S16);
        set_params.push(VIRTIO_SND_PCM_RATE_48000);
        set_params.push(0);
        mem.write_from(req_addr, &set_params).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: req_addr,
                len: set_params.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: resp_addr,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 1, &[0]);
        init_used(&mut mem, used);

        let control_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        dev.process_control(&mut mem).unwrap();

        let mut status = [0u8; 4];
        mem.read_into(resp_addr, &mut status).unwrap();
        assert_eq!(u32::from_le_bytes(status), VIRTIO_SND_S_NOT_SUPP);

        assert_eq!(dev.playback.state, StreamState::Idle);
        assert!(dev.playback.params.is_none());
    }

    #[test]
    fn control_unknown_command_returns_not_supp() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let req_addr = 0x110;
        let resp_addr = 0x200;

        let req = 0xdead_beefu32.to_le_bytes();
        mem.write_from(req_addr, &req).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: req_addr,
                len: req.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: resp_addr,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 1, &[0]);
        init_used(&mut mem, used);

        let control_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        dev.process_control(&mut mem).unwrap();

        let mut status = [0u8; 4];
        mem.read_into(resp_addr, &mut status).unwrap();
        assert_eq!(u32::from_le_bytes(status), VIRTIO_SND_S_NOT_SUPP);
    }

    #[test]
    fn tx_queue_writes_pcm_samples_to_ring_buffer() {
        let mut mem = DenseMemory::new(0x10000).unwrap();

        let ctl_desc = 0x1000;
        let ctl_avail = 0x2000;
        let ctl_used = 0x3000;
        let tx_desc = 0x4000;
        let tx_avail = 0x5000;
        let tx_used = 0x6000;

        init_used(&mut mem, ctl_used);
        init_used(&mut mem, tx_used);

        let control_vq = VirtQueue::new(8, ctl_desc, ctl_avail, ctl_used);
        let tx_vq = VirtQueue::new(8, tx_desc, tx_avail, tx_used);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);

        dev.playback.state = StreamState::Running;

        let hdr_addr = 0x700;
        let pcm_addr = 0x800;
        let resp_addr = 0x900;

        mem.write_u32_le(hdr_addr, PLAYBACK_STREAM_ID).unwrap();
        mem.write_u32_le(hdr_addr + 4, 0).unwrap();

        let samples: [i16; 4] = [1000, -1000, 2000, -2000];
        let mut pcm = Vec::new();
        for s in samples {
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        mem.write_from(pcm_addr, &pcm).unwrap();

        write_desc(
            &mut mem,
            tx_desc,
            0,
            Descriptor {
                addr: hdr_addr,
                len: 8,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            1,
            Descriptor {
                addr: pcm_addr,
                len: pcm.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            2,
            Descriptor {
                addr: resp_addr,
                len: 8,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, tx_avail, 0, 1, &[0]);

        dev.process_tx(&mut mem).unwrap();

        let mut status = [0u8; 4];
        mem.read_into(resp_addr, &mut status).unwrap();
        assert_eq!(u32::from_le_bytes(status), VIRTIO_SND_S_OK);

        let mut out = [0.0f32; 4];
        assert_eq!(dev.sink.read_interleaved(&mut out), 2);
        assert_f32_eq(out[0], 1000.0 / 32768.0);
        assert_f32_eq(out[1], -1000.0 / 32768.0);
        assert_f32_eq(out[2], 2000.0 / 32768.0);
        assert_f32_eq(out[3], -2000.0 / 32768.0);
    }

    #[test]
    fn tx_queue_requires_running_stream() {
        let mut mem = DenseMemory::new(0x10000).unwrap();

        let tx_desc = 0x1000;
        let tx_avail = 0x2000;
        let tx_used = 0x3000;

        init_used(&mut mem, tx_used);

        let control_vq = VirtQueue::new(8, 0, 0, 0);
        let tx_vq = VirtQueue::new(8, tx_desc, tx_avail, tx_used);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);

        let hdr_addr = 0x700;
        let pcm_addr = 0x800;
        let resp_addr = 0x900;

        mem.write_u32_le(hdr_addr, PLAYBACK_STREAM_ID).unwrap();
        mem.write_u32_le(hdr_addr + 4, 0).unwrap();
        mem.write_from(pcm_addr, &[0u8; 4]).unwrap();

        write_desc(
            &mut mem,
            tx_desc,
            0,
            Descriptor {
                addr: hdr_addr,
                len: 8,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            1,
            Descriptor {
                addr: pcm_addr,
                len: 4,
                flags: VRING_DESC_F_NEXT,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            2,
            Descriptor {
                addr: resp_addr,
                len: 8,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, tx_avail, 0, 1, &[0]);

        dev.process_tx(&mut mem).unwrap();

        let mut resp = [0u8; 8];
        mem.read_into(resp_addr, &mut resp).unwrap();
        assert_eq!(
            u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            VIRTIO_SND_S_IO_ERR
        );
        assert_eq!(dev.sink.buffer_level_frames(), 0);
    }

    #[test]
    fn tx_queue_rejects_unknown_stream_id() {
        let mut mem = DenseMemory::new(0x10000).unwrap();

        let tx_desc = 0x1000;
        let tx_avail = 0x2000;
        let tx_used = 0x3000;

        init_used(&mut mem, tx_used);

        let control_vq = VirtQueue::new(8, 0, 0, 0);
        let tx_vq = VirtQueue::new(8, tx_desc, tx_avail, tx_used);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        dev.playback.state = StreamState::Running;

        let hdr_addr = 0x700;
        let pcm_addr = 0x800;
        let resp_addr = 0x900;

        mem.write_u32_le(hdr_addr, 1).unwrap();
        mem.write_u32_le(hdr_addr + 4, 0).unwrap();
        mem.write_from(pcm_addr, &[0u8; 4]).unwrap();

        write_desc(
            &mut mem,
            tx_desc,
            0,
            Descriptor {
                addr: hdr_addr,
                len: 8,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            1,
            Descriptor {
                addr: pcm_addr,
                len: 4,
                flags: VRING_DESC_F_NEXT,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            tx_desc,
            2,
            Descriptor {
                addr: resp_addr,
                len: 8,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, tx_avail, 0, 1, &[0]);

        dev.process_tx(&mut mem).unwrap();

        let mut resp = [0u8; 8];
        mem.read_into(resp_addr, &mut resp).unwrap();
        assert_eq!(
            u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            VIRTIO_SND_S_BAD_MSG
        );
        assert_eq!(dev.sink.buffer_level_frames(), 0);
    }

    #[test]
    fn control_oversized_request_returns_bad_msg() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let req_addr = 0x110;
        let resp_addr = 0x200;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: req_addr,
                len: (MAX_CONTROL_MSG_BYTES as u32) + 1,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: resp_addr,
                len: 16,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 1, &[0]);
        init_used(&mut mem, used);

        let control_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let rb = AudioWorkletRingBuffer::new(1024);
        let mut dev = VirtioSndDevice::new(rb, control_vq, tx_vq);
        dev.process_control(&mut mem).unwrap();

        let mut status = [0u8; 4];
        mem.read_into(resp_addr, &mut status).unwrap();
        assert_eq!(u32::from_le_bytes(status), VIRTIO_SND_S_BAD_MSG);
    }
}
