#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_audio::ring::AudioRingBuffer;
use aero_virtio::devices::snd::{
    AudioCaptureSource, VirtioSnd, CAPTURE_STREAM_ID, PLAYBACK_STREAM_ID, VIRTIO_SND_PCM_FMT_S16,
    VIRTIO_SND_PCM_RATE_48000, VIRTIO_SND_QUEUE_CONTROL, VIRTIO_SND_QUEUE_EVENT,
    VIRTIO_SND_QUEUE_RX, VIRTIO_SND_QUEUE_TX, VIRTIO_SND_R_PCM_PREPARE, VIRTIO_SND_R_PCM_SET_PARAMS,
    VIRTIO_SND_R_PCM_START,
};
use aero_virtio::devices::VirtioDevice;
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::queue::{
    PoppedDescriptorChain, VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};

const MEM_SIZE: usize = 0x40_000; // 256KiB

const QSIZE_CONTROL: u16 = 8;
const QSIZE_EVENT: u16 = 8;
const QSIZE_TX: u16 = 16;
const QSIZE_RX: u16 = 8;

const MAX_STEPS: usize = 64;

#[derive(Clone, Copy)]
struct QueueAddrs {
    desc: u64,
    avail: u64,
    used: u64,
    size: u16,
}

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    let _ = write_u64_le(mem, base, addr);
    let _ = write_u32_le(mem, base + 8, len);
    let _ = write_u16_le(mem, base + 12, flags);
    let _ = write_u16_le(mem, base + 14, next);
}

fn submit_one(mem: &mut GuestRam, q: &mut VirtQueue, addrs: QueueAddrs, head: u16) {
    // Provide exactly one new available entry per submit to keep per-iteration work bounded even if
    // the fuzzer fills the rings with garbage.
    let next = q.next_avail();
    let slot = next % addrs.size;
    let _ = write_u16_le(mem, addrs.avail + 4 + u64::from(slot) * 2, head);
    let _ = write_u16_le(mem, addrs.avail + 2, next.wrapping_add(1));
}

// Deterministic dummy capture source. Always returns data (no underrun) but the values are
// attacker-controlled through the initial seed.
#[derive(Clone, Copy)]
struct FuzzCapture {
    state: u64,
    dropped: u64,
}

impl AudioCaptureSource for FuzzCapture {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        for s in out.iter_mut() {
            // LCG with full-period modulus 2^64.
            self.state = self
                .state
                .wrapping_mul(6364136223846793005u64)
                .wrapping_add(1442695040888963407u64);
            // Map high bits into roughly [-1.0, 1.0].
            let v = (self.state >> 32) as i32;
            *s = (v as f32) / (i32::MAX as f32);
        }
        out.len()
    }

    fn take_dropped_samples(&mut self) -> u64 {
        let d = self.dropped;
        self.dropped = 0;
        d
    }
}

fn build_pcm_set_params(stream_id: u32, channels: u8) -> [u8; 24] {
    let mut req = [0u8; 24];
    req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    req[4..8].copy_from_slice(&stream_id.to_le_bytes());
    req[8..12].copy_from_slice(&4096u32.to_le_bytes()); // buffer_bytes
    req[12..16].copy_from_slice(&1024u32.to_le_bytes()); // period_bytes
    // features [16..20] = 0
    req[20] = channels;
    req[21] = VIRTIO_SND_PCM_FMT_S16;
    req[22] = VIRTIO_SND_PCM_RATE_48000;
    req
}

fn build_simple_cmd(code: u32, stream_id: u32) -> [u8; 8] {
    let mut req = [0u8; 8];
    req[0..4].copy_from_slice(&code.to_le_bytes());
    req[4..8].copy_from_slice(&stream_id.to_le_bytes());
    req
}

fn send_control(
    snd: &mut VirtioSnd<AudioRingBuffer, FuzzCapture>,
    mem: &mut GuestRam,
    addrs: QueueAddrs,
    q: &mut VirtQueue,
    req_addr: u64,
    req_bytes: &[u8],
    resp_addr: u64,
    resp_len: u32,
) {
    let _ = mem.write(req_addr, req_bytes);
    // Response buffer is device-written; no need to init.

    // Use head=0 for setup traffic.
    write_desc(mem, addrs.desc, 0, req_addr, req_bytes.len() as u32, VIRTQ_DESC_F_NEXT, 1);
    write_desc(mem, addrs.desc, 1, resp_addr, resp_len, VIRTQ_DESC_F_WRITE, 0);

    submit_one(mem, q, addrs, 0);
    let popped = match q.pop_descriptor_chain(mem) {
        Ok(Some(p)) => p,
        _ => return,
    };

    match popped {
        PoppedDescriptorChain::Chain(chain) => {
            let _ = snd.process_queue(VIRTIO_SND_QUEUE_CONTROL, chain, q, mem);
        }
        PoppedDescriptorChain::Invalid { head_index, .. } => {
            let _ = q.add_used(mem, head_index, 0);
        }
    }
}

#[derive(Clone, Copy)]
enum StepKind {
    Control,
    Event,
    Tx,
    Rx,
}

#[derive(Clone, Copy)]
struct Step {
    kind: StepKind,
    head: u16,
    // Stream IDs / request codes are queue-specific.
    a: u32,
    b: u32,
    // Descriptor lengths (bounded at submission time).
    len0: u32,
    len1: u32,
    len2: u32,
    // Address seeds (used to place buffers).
    addr_seed0: u32,
    addr_seed1: u32,
    addr_seed2: u32,
    // Flags to allow some malformed chains (missing response, wrong ordering).
    flags: u8,
}

fn addr_from_seed(seed: u32, allow_oob: bool) -> u64 {
    // Keep addresses away from the queue structures by default so we hit deeper device logic.
    //
    // Also, libFuzzer's default `-max_len` is 4096; we seed guest RAM from the input starting at
    // address 0, so placing buffers in the first page makes it much more likely the fuzzer can
    // influence the PCM/header bytes (instead of reading zeros).
    //
    // Still allow some OOB address cases for negative testing.
    if allow_oob {
        // Large spread that will frequently exceed guest RAM.
        (seed as u64) << 8
    } else {
        (seed as u64) & 0x0fff
    }
}

fn rate_from_choice(choice: u8) -> u32 {
    match choice % 5 {
        0 => 48_000,
        1 => 44_100,
        2 => 32_000,
        3 => 96_000,
        _ => 192_000,
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    let host_rate_choice: u8 = u.arbitrary().unwrap_or(0);
    let capture_rate_choice: u8 = u.arbitrary().unwrap_or(0);
    let capture_seed: u64 = u.arbitrary().unwrap_or(0);
    let steps_count: usize = u.int_in_range(0usize..=MAX_STEPS).unwrap_or(0);
    let mut steps = Vec::with_capacity(steps_count);
    for _ in 0..steps_count {
        let kind_sel: u8 = u.int_in_range(0u8..=3).unwrap_or(0);
        let kind = match kind_sel {
            0 => StepKind::Control,
            1 => StepKind::Event,
            2 => StepKind::Tx,
            _ => StepKind::Rx,
        };
        steps.push(Step {
            kind,
            head: u.arbitrary().unwrap_or(0),
            a: u.arbitrary().unwrap_or(0),
            b: u.arbitrary().unwrap_or(0),
            len0: u.arbitrary().unwrap_or(0),
            len1: u.arbitrary().unwrap_or(0),
            len2: u.arbitrary().unwrap_or(0),
            addr_seed0: u.arbitrary().unwrap_or(0),
            addr_seed1: u.arbitrary().unwrap_or(0),
            addr_seed2: u.arbitrary().unwrap_or(0),
            flags: u.arbitrary().unwrap_or(0),
        });
    }

    // Guest memory: fixed-size and seeded from remaining bytes.
    let mut mem = GuestRam::new(MEM_SIZE);
    {
        let init_len = u.len();
        let init = u.bytes(init_len).unwrap_or(&[]);
        let ram = mem.as_mut_slice();
        let n = init.len().min(ram.len());
        ram[..n].copy_from_slice(&init[..n]);
    }

    let output = AudioRingBuffer::new_stereo(8);
    let capture = FuzzCapture {
        state: capture_seed,
        dropped: 0,
    };
    let mut snd = VirtioSnd::new_with_capture_and_host_sample_rate(
        output,
        capture,
        rate_from_choice(host_rate_choice),
    );
    snd.set_capture_sample_rate_hz(rate_from_choice(capture_rate_choice));

    let ctrl_addrs = QueueAddrs {
        desc: 0x1000,
        avail: 0x2000,
        used: 0x3000,
        size: QSIZE_CONTROL,
    };
    let event_addrs = QueueAddrs {
        desc: 0x4000,
        avail: 0x5000,
        used: 0x6000,
        size: QSIZE_EVENT,
    };
    let tx_addrs = QueueAddrs {
        desc: 0x7000,
        avail: 0x8000,
        used: 0x9000,
        size: QSIZE_TX,
    };
    let rx_addrs = QueueAddrs {
        desc: 0xA000,
        avail: 0xB000,
        used: 0xC000,
        size: QSIZE_RX,
    };

    let mut ctrl_q = match VirtQueue::new(
        VirtQueueConfig {
            size: ctrl_addrs.size,
            desc_addr: ctrl_addrs.desc,
            avail_addr: ctrl_addrs.avail,
            used_addr: ctrl_addrs.used,
        },
        false,
    ) {
        Ok(q) => q,
        Err(_) => return,
    };
    let mut event_q = match VirtQueue::new(
        VirtQueueConfig {
            size: event_addrs.size,
            desc_addr: event_addrs.desc,
            avail_addr: event_addrs.avail,
            used_addr: event_addrs.used,
        },
        false,
    ) {
        Ok(q) => q,
        Err(_) => return,
    };
    let mut tx_q = match VirtQueue::new(
        VirtQueueConfig {
            size: tx_addrs.size,
            desc_addr: tx_addrs.desc,
            avail_addr: tx_addrs.avail,
            used_addr: tx_addrs.used,
        },
        false,
    ) {
        Ok(q) => q,
        Err(_) => return,
    };
    let mut rx_q = match VirtQueue::new(
        VirtQueueConfig {
            size: rx_addrs.size,
            desc_addr: rx_addrs.desc,
            avail_addr: rx_addrs.avail,
            used_addr: rx_addrs.used,
        },
        false,
    ) {
        Ok(q) => q,
        Err(_) => return,
    };

    // Initialize ring headers to a known state (otherwise `needs_interrupt()` will read random
    // flags, which is fine, but makes debugging harder).
    for a in [ctrl_addrs, event_addrs, tx_addrs, rx_addrs] {
        let _ = write_u16_le(&mut mem, a.avail + 0, 0);
        let _ = write_u16_le(&mut mem, a.avail + 2, 0);
        let _ = write_u16_le(&mut mem, a.used + 0, 0);
        let _ = write_u16_le(&mut mem, a.used + 2, 0);
    }

    // Best-effort initial stream setup so TX/RX can reach deeper decode/resample paths.
    // This uses the control queue (exercise `process_queue` with realistic traffic).
    let resp_addr = 0x12000u64;
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11000,
        &build_pcm_set_params(PLAYBACK_STREAM_ID, 2),
        resp_addr,
        64,
    );
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11100,
        &build_simple_cmd(VIRTIO_SND_R_PCM_PREPARE, PLAYBACK_STREAM_ID),
        resp_addr,
        64,
    );
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11200,
        &build_simple_cmd(VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID),
        resp_addr,
        64,
    );
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11300,
        &build_pcm_set_params(CAPTURE_STREAM_ID, 1),
        resp_addr,
        64,
    );
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11400,
        &build_simple_cmd(VIRTIO_SND_R_PCM_PREPARE, CAPTURE_STREAM_ID),
        resp_addr,
        64,
    );
    send_control(
        &mut snd,
        &mut mem,
        ctrl_addrs,
        &mut ctrl_q,
        0x11500,
        &build_simple_cmd(VIRTIO_SND_R_PCM_START, CAPTURE_STREAM_ID),
        resp_addr,
        64,
    );

    for step in steps {
        match step.kind {
            StepKind::Control => {
                let head = step.head % ctrl_addrs.size;
                let allow_oob = (step.flags & 0x80) != 0;
                let req_addr = addr_from_seed(step.addr_seed0, allow_oob);
                let resp_addr = addr_from_seed(step.addr_seed1, allow_oob);

                // Write a small header (code + stream_id) into the request buffer so we exercise a
                // variety of control commands without allocating attacker-controlled payloads.
                let req_len = (step.len0 as usize).min(64);
                if req_len != 0 {
                    let mut tmp = [0u8; 8];
                    tmp[0..4].copy_from_slice(&step.a.to_le_bytes());
                    tmp[4..8].copy_from_slice(&step.b.to_le_bytes());
                    let _ = mem.write(req_addr, &tmp[..req_len.min(tmp.len())]);
                }

                let missing_resp = (step.flags & 1) != 0;
                if missing_resp {
                    write_desc(
                        &mut mem,
                        ctrl_addrs.desc,
                        head,
                        req_addr,
                        req_len as u32,
                        0,
                        0,
                    );
                } else {
                    let next = (head + 1) % ctrl_addrs.size;
                    write_desc(
                        &mut mem,
                        ctrl_addrs.desc,
                        head,
                        req_addr,
                        req_len as u32,
                        VIRTQ_DESC_F_NEXT,
                        next,
                    );
                    write_desc(
                        &mut mem,
                        ctrl_addrs.desc,
                        next,
                        resp_addr,
                        (step.len1).min(64),
                        VIRTQ_DESC_F_WRITE,
                        0,
                    );
                }

                submit_one(&mut mem, &mut ctrl_q, ctrl_addrs, head);
                if let Ok(Some(popped)) = ctrl_q.pop_descriptor_chain(&mem) {
                    match popped {
                        PoppedDescriptorChain::Chain(chain) => {
                            let _ = snd.process_queue(VIRTIO_SND_QUEUE_CONTROL, chain, &mut ctrl_q, &mut mem);
                        }
                        PoppedDescriptorChain::Invalid { head_index, .. } => {
                            let _ = ctrl_q.add_used(&mut mem, head_index, 0);
                        }
                    }
                }
            }
            StepKind::Event => {
                let head = step.head % event_addrs.size;
                let buf_addr = addr_from_seed(step.addr_seed0, (step.flags & 0x80) != 0);
                let buf_len = (step.len0).min(256);
                write_desc(
                    &mut mem,
                    event_addrs.desc,
                    head,
                    buf_addr,
                    buf_len,
                    VIRTQ_DESC_F_WRITE,
                    0,
                );

                submit_one(&mut mem, &mut event_q, event_addrs, head);
                if let Ok(Some(popped)) = event_q.pop_descriptor_chain(&mem) {
                    match popped {
                        PoppedDescriptorChain::Chain(chain) => {
                            let _ = snd.process_queue(VIRTIO_SND_QUEUE_EVENT, chain, &mut event_q, &mut mem);
                        }
                        PoppedDescriptorChain::Invalid { head_index, .. } => {
                            let _ = event_q.add_used(&mut mem, head_index, 0);
                        }
                    }
                }
            }
            StepKind::Tx => {
                let head = step.head % tx_addrs.size;
                let allow_oob = (step.flags & 0x80) != 0;
                let hdr_addr = addr_from_seed(step.addr_seed0, allow_oob);
                let pcm_addr = addr_from_seed(step.addr_seed1, allow_oob);
                let status_addr = addr_from_seed(step.addr_seed2, allow_oob);

                // Always write a stream_id field so we hit both OK + BAD_MSG paths depending on the
                // value (and current stream state).
                let stream_id = step.a;
                let mut hdr = [0u8; 8];
                hdr[0..4].copy_from_slice(&stream_id.to_le_bytes());
                hdr[4..8].copy_from_slice(&step.b.to_le_bytes());
                let _ = mem.write(hdr_addr, &hdr);

                let payload_len = step.len1;
                let status_len = (step.len2).min(64).max(4);
                let missing_status = (step.flags & 1) != 0;
                let misorder = (step.flags & 2) != 0;

                let idx1 = (head + 1) % tx_addrs.size;
                let idx2 = (head + 2) % tx_addrs.size;

                // Header OUT.
                write_desc(
                    &mut mem,
                    tx_addrs.desc,
                    head,
                    hdr_addr,
                    8,
                    VIRTQ_DESC_F_NEXT,
                    idx1,
                );

                if misorder {
                    // Force a writable descriptor in the middle (invalid per virtio ordering rules).
                    write_desc(
                        &mut mem,
                        tx_addrs.desc,
                        idx1,
                        status_addr,
                        status_len,
                        VIRTQ_DESC_F_WRITE,
                        0,
                    );
                } else {
                    // PCM OUT.
                    let next_flags = if missing_status { 0 } else { VIRTQ_DESC_F_NEXT };
                    write_desc(
                        &mut mem,
                        tx_addrs.desc,
                        idx1,
                        pcm_addr,
                        payload_len,
                        next_flags,
                        idx2,
                    );
                    if !missing_status {
                        write_desc(
                            &mut mem,
                            tx_addrs.desc,
                            idx2,
                            status_addr,
                            status_len,
                            VIRTQ_DESC_F_WRITE,
                            0,
                        );
                    }
                }

                submit_one(&mut mem, &mut tx_q, tx_addrs, head);
                if let Ok(Some(popped)) = tx_q.pop_descriptor_chain(&mem) {
                    match popped {
                        PoppedDescriptorChain::Chain(chain) => {
                            let _ = snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut tx_q, &mut mem);
                        }
                        PoppedDescriptorChain::Invalid { head_index, .. } => {
                            let _ = tx_q.add_used(&mut mem, head_index, 0);
                        }
                    }
                }
            }
            StepKind::Rx => {
                let head = step.head % rx_addrs.size;
                let allow_oob = (step.flags & 0x80) != 0;
                let hdr_addr = addr_from_seed(step.addr_seed0, allow_oob);
                let payload_addr = addr_from_seed(step.addr_seed1, allow_oob);
                let resp_addr = addr_from_seed(step.addr_seed2, allow_oob);

                let stream_id = step.a;
                let mut hdr = [0u8; 8];
                hdr[0..4].copy_from_slice(&stream_id.to_le_bytes());
                hdr[4..8].copy_from_slice(&step.b.to_le_bytes());
                let _ = mem.write(hdr_addr, &hdr);

                let payload_len = step.len1;
                let resp_len = (step.len2).min(64).max(4);
                let missing_resp = (step.flags & 1) != 0;

                let idx1 = (head + 1) % rx_addrs.size;
                let idx2 = (head + 2) % rx_addrs.size;

                write_desc(
                    &mut mem,
                    rx_addrs.desc,
                    head,
                    hdr_addr,
                    8,
                    VIRTQ_DESC_F_NEXT,
                    idx1,
                );

                if missing_resp {
                    write_desc(
                        &mut mem,
                        rx_addrs.desc,
                        idx1,
                        payload_addr,
                        payload_len,
                        VIRTQ_DESC_F_WRITE,
                        0,
                    );
                } else {
                    write_desc(
                        &mut mem,
                        rx_addrs.desc,
                        idx1,
                        payload_addr,
                        payload_len,
                        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
                        idx2,
                    );
                    write_desc(
                        &mut mem,
                        rx_addrs.desc,
                        idx2,
                        resp_addr,
                        resp_len,
                        VIRTQ_DESC_F_WRITE,
                        0,
                    );
                }

                submit_one(&mut mem, &mut rx_q, rx_addrs, head);
                if let Ok(Some(popped)) = rx_q.pop_descriptor_chain(&mem) {
                    match popped {
                        PoppedDescriptorChain::Chain(chain) => {
                            let _ = snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut rx_q, &mut mem);
                        }
                        PoppedDescriptorChain::Invalid { head_index, .. } => {
                            let _ = rx_q.add_used(&mut mem, head_index, 0);
                        }
                    }
                }
            }
        }
    }
});
