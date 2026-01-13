#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::devices::VirtioDevice;
use aero_virtio::memory::{write_u16_le, GuestRam};
use aero_virtio::queue::{PoppedDescriptorChain, VirtQueue, VirtQueueConfig};

const MAX_INPUT_LEN: usize = 4096;
const MEM_SIZE: usize = 64 * 1024;

// Fixed split-virtqueue layout for determinism.
const QUEUE_SIZE: u16 = 8;
const DESC_ADDR: u64 = 0x1000;
const AVAIL_ADDR: u64 = 0x2000;
const USED_ADDR: u64 = 0x3000;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(MAX_INPUT_LEN)];
    let mut u = Unstructured::new(data);

    let queue_index: u16 = (u.arbitrary::<u8>().unwrap_or(1) % 2) as u16; // 0=RX, 1=TX
    let event_idx: bool = u.arbitrary().unwrap_or(false);
    let head: u16 = u.arbitrary::<u16>().unwrap_or(0);

    let mut mem = GuestRam::new(MEM_SIZE);
    {
        // Seed guest RAM from the fuzz input so the fuzzer can influence descriptor and payload
        // bytes directly.
        let n = data.len().min(mem.as_mut_slice().len());
        mem.as_mut_slice()[..n].copy_from_slice(&data[..n]);
    }

    // Initialize a minimal avail/used ring so `pop_descriptor_chain()` will attempt to parse.
    //
    // avail: flags(u16)=0, idx(u16)=1, ring[0]=head
    let _ = write_u16_le(&mut mem, AVAIL_ADDR, 0);
    let _ = write_u16_le(&mut mem, AVAIL_ADDR + 2, 1);
    let _ = write_u16_le(&mut mem, AVAIL_ADDR + 4, head);
    // used: flags(u16)=0, idx(u16)=0
    let _ = write_u16_le(&mut mem, USED_ADDR, 0);
    let _ = write_u16_le(&mut mem, USED_ADDR + 2, 0);

    let mut queue = match VirtQueue::new(
        VirtQueueConfig {
            size: QUEUE_SIZE,
            desc_addr: DESC_ADDR,
            avail_addr: AVAIL_ADDR,
            used_addr: USED_ADDR,
        },
        event_idx,
    ) {
        Ok(q) => q,
        Err(_) => return,
    };

    let mut dev = VirtioNet::new(LoopbackNet::default(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Keep negotiated features within what the device claims to support.
    let features = dev.device_features();
    dev.set_features(features);

    let popped = match queue.pop_descriptor_chain(&mem) {
        Ok(p) => p,
        Err(_) => return,
    };

    if let Some(popped) = popped {
        match popped {
            PoppedDescriptorChain::Chain(chain) => {
                let _ = dev.process_queue(queue_index, chain, &mut queue, &mut mem);
            }
            PoppedDescriptorChain::Invalid { head_index, .. } => {
                // Mirror transport behavior: complete invalid chains with a 0-length used entry.
                let _ = queue.add_used(&mut mem, head_index, 0);
            }
        }
    }

    // Give the device a chance to do RX-side work, even if we fuzzed TX.
    let _ = dev.poll_queue(0, &mut queue, &mut mem);

    // Drain backend state so we don't retain large buffers within a single fuzz iteration.
    dev.backend_mut().tx_packets.clear();
    dev.backend_mut().rx_packets.clear();
});

