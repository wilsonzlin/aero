#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::audio::state::{AudioWorkletRingState, VirtioSndPciState};
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_EVT_JACK_DISCONNECTED,
    VIRTIO_SND_QUEUE_EVENT, VIRTIO_SND_R_PCM_INFO, VIRTIO_SND_S_OK,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use aero_wasm::VirtioSndPciBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn write_u16(mem: &mut [u8], addr: u32, value: u16) {
    let addr = addr as usize;
    mem[addr..addr + 2].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(mem: &[u8], addr: u32) -> u16 {
    let addr = addr as usize;
    u16::from_le_bytes([mem[addr], mem[addr + 1]])
}

fn read_u32(mem: &[u8], addr: u32) -> u32 {
    let addr = addr as usize;
    u32::from_le_bytes(mem[addr..addr + 4].try_into().unwrap())
}

fn write_u32(mem: &mut [u8], addr: u32, value: u32) {
    let addr = addr as usize;
    mem[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(mem: &mut [u8], addr: u32, value: u64) {
    let addr = addr as usize;
    mem[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_desc(mem: &mut [u8], table: u32, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u32::from(index) * 16;
    write_u64(mem, base, addr);
    write_u32(mem, base + 8, len);
    write_u16(mem, base + 12, flags);
    write_u16(mem, base + 14, next);
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_is_gated_on_pci_bus_master_enable() {
    // Synthetic guest RAM region outside the wasm heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    // Enable PCI memory decoding so BAR0 MMIO reads/writes reach the device, but keep Bus Master
    // Enable disabled so the device cannot DMA until the guest explicitly enables it during PCI
    // enumeration.
    bridge.set_pci_command(0x0002);

    // BAR0 layout is fixed by `aero_virtio::pci::VirtioPciDevice`.
    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;
    const ISR: u32 = 0x2000;

    // Minimal virtio feature negotiation (accept everything offered).
    bridge.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge.mmio_write(COMMON + 0x00, 4, 0); // device_feature_select
    let f0 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge.mmio_write(COMMON + 0x00, 4, 1);
    let f1 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 1);
    bridge.mmio_write(COMMON + 0x0c, 4, f1);

    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK),
    );
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        ),
    );

    // Configure control queue 0 (virtio-snd).
    bridge.mmio_write(COMMON + 0x16, 2, 0); // queue_select
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 8);

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;

    bridge.mmio_write(COMMON + 0x20, 4, desc_table);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);
    bridge.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    // Descriptor chain: [control request (out)][response buffer (in)].
    let req_addr = 0x4000u32;
    let resp_addr = 0x4100u32;
    let mut req = Vec::new();
    req.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes()); // start_id
    req.extend_from_slice(&2u32.to_le_bytes()); // count
    guest[req_addr as usize..req_addr as usize + req.len()].copy_from_slice(&req);

    let resp_len = 128u32;
    guest[resp_addr as usize..resp_addr as usize + resp_len as usize].fill(0xAA);

    write_desc(
        guest,
        desc_table,
        0,
        req_addr as u64,
        req.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        guest,
        desc_table,
        1,
        resp_addr as u64,
        resp_len,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // avail.idx = 1, ring[0] = 0
    write_u16(guest, avail, 0);
    write_u16(guest, avail + 2, 1);
    write_u16(guest, avail + 4, 0);

    // used.idx = 0
    write_u16(guest, used, 0);
    write_u16(guest, used + 2, 0);

    assert!(!bridge.irq_asserted(), "irq should start deasserted");

    // Notify queue 0 while BME is disabled. notify_mult=4, queue_notify_off=0.
    bridge.mmio_write(NOTIFY + 0, 2, 0);
    bridge.poll();

    assert!(
        !bridge.irq_asserted(),
        "irq should remain deasserted while PCI bus mastering is disabled"
    );
    assert_eq!(
        read_u16(guest, used + 2),
        0,
        "used.idx should not advance without bus mastering"
    );
    assert_eq!(
        &guest[resp_addr as usize..resp_addr as usize + 4],
        &[0xAA; 4],
        "response header should not be DMA-written while PCI bus mastering is disabled"
    );

    // Enable bus mastering and retry: the pending notify should now be processed via DMA.
    bridge.set_pci_command(0x0006);
    bridge.poll();

    assert_eq!(
        read_u16(guest, used + 2),
        1,
        "expected used.idx to advance after enabling bus mastering"
    );
    let status = u32::from_le_bytes(
        guest[resp_addr as usize..resp_addr as usize + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        status, VIRTIO_SND_S_OK,
        "unexpected control response status"
    );
    assert!(
        bridge.irq_asserted(),
        "irq should assert after control request completion"
    );

    // Reading ISR clears the asserted interrupt.
    let isr = bridge.mmio_read(ISR, 1) as u8;
    assert_ne!(
        isr & VIRTIO_PCI_LEGACY_ISR_QUEUE,
        0,
        "expected ISR queue bit"
    );
    assert!(
        !bridge.irq_asserted(),
        "irq should deassert after ISR read-to-clear"
    );
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_snapshot_roundtrip_is_deterministic() {
    // BAR0 layout is fixed by `aero_virtio::pci::VirtioPciDevice`.
    const COMMON: u32 = 0x0000;

    let (guest_base1, guest_size1) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge1 =
        VirtioSndPciBridge::new(guest_base1, guest_size1, None).expect("VirtioSndPciBridge::new");
    bridge1.set_pci_command(0x0002); // enable MMIO decode

    // Mutate virtio-pci transport state: negotiate features and set DRIVER_OK.
    bridge1.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge1.mmio_write(COMMON + 0x00, 4, 0); // device_feature_select
    let f0 = bridge1.mmio_read(COMMON + 0x04, 4);
    bridge1.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge1.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge1.mmio_write(COMMON + 0x00, 4, 1);
    let f1 = bridge1.mmio_read(COMMON + 0x04, 4);
    bridge1.mmio_write(COMMON + 0x08, 4, 1);
    bridge1.mmio_write(COMMON + 0x0c, 4, f1);

    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK),
    );
    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        ),
    );

    // Mutate virtio-snd state.
    bridge1.set_host_sample_rate_hz(44_100).unwrap();
    bridge1.set_capture_sample_rate_hz(48_000).unwrap();

    // Attach an AudioWorklet ring so snapshot includes non-trivial ring indices.
    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge1.attach_audio_ring(sab.clone(), 8, 2).unwrap();
    ring.restore_state(&AudioWorkletRingState {
        capacity_frames: 8,
        read_pos: 2,
        write_pos: 6,
    });

    let snap1 = bridge1.save_state();

    // Restore into a fresh bridge (no ring attached). The ring state should be retained as pending
    // state and re-serialized identically.
    let (guest_base2, guest_size2) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge2 =
        VirtioSndPciBridge::new(guest_base2, guest_size2, None).expect("VirtioSndPciBridge::new");
    bridge2.load_state(&snap1).unwrap();
    let snap2 = bridge2.save_state();

    assert_eq!(
        snap1, snap2,
        "save_state -> load_state -> save_state must be stable"
    );
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_emits_speaker_jack_events_on_audio_ring_attach_and_detach() {
    // Synthetic guest RAM region outside the wasm heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    // Enable MMIO decoding + bus mastering so the device can DMA.
    bridge.set_pci_command(0x0006);

    // BAR0 layout is fixed by `aero_virtio::pci::VirtioPciDevice`.
    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;

    // Minimal virtio feature negotiation (accept everything offered).
    bridge.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge.mmio_write(COMMON + 0x00, 4, 0); // device_feature_select
    let f0 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge.mmio_write(COMMON + 0x00, 4, 1);
    let f1 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 1);
    bridge.mmio_write(COMMON + 0x0c, 4, f1);

    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK),
    );
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        ),
    );

    // Configure event queue 1 (virtio-snd).
    bridge.mmio_write(COMMON + 0x16, 2, u32::from(VIRTIO_SND_QUEUE_EVENT)); // queue_select
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 2, "expected event queue size >= 2");

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let buf = 0x4000u32;

    bridge.mmio_write(COMMON + 0x20, 4, desc_table);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);
    bridge.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    // Post a single 8-byte writable event buffer (virtio-snd events are 8 bytes).
    guest[buf as usize..buf as usize + 8].fill(0xAA);
    write_desc(
        guest,
        desc_table,
        0,
        buf as u64,
        8,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_u16(guest, avail, 0);
    write_u16(guest, avail + 2, 1);
    write_u16(guest, avail + 4, 0);
    write_u16(guest, used, 0);
    write_u16(guest, used + 2, 0);

    // Attach the audio ring: should queue a speaker JACK_CONNECTED event.
    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    // Notify queue 1. notify_mult is 4 in `VirtioPciDevice`.
    let notify_off = bridge.mmio_read(COMMON + 0x1e, 2) as u32;
    bridge.mmio_write(NOTIFY + notify_off * 4, 2, u32::from(VIRTIO_SND_QUEUE_EVENT));

    assert_eq!(read_u16(guest, used + 2), 1);
    assert_eq!(read_u32(guest, used + 8), 8);
    let expected_connected = {
        let mut evt = [0u8; 8];
        evt[0..4].copy_from_slice(&VIRTIO_SND_EVT_JACK_CONNECTED.to_le_bytes());
        evt[4..8].copy_from_slice(&JACK_ID_SPEAKER.to_le_bytes());
        evt
    };
    assert_eq!(
        &guest[buf as usize..buf as usize + 8],
        &expected_connected
    );

    // Detach the audio ring: should queue a speaker JACK_DISCONNECTED event and deliver it into a
    // subsequent event buffer.
    bridge
        .set_audio_ring_buffer(None, 8, 2)
        .expect("set_audio_ring_buffer(None)");

    guest[buf as usize..buf as usize + 8].fill(0xAA);
    // Re-post the same descriptor (index 0).
    write_u16(guest, avail + 6, 0); // avail.ring[1] = desc 0
    write_u16(guest, avail + 2, 2); // avail.idx = 2
    bridge.mmio_write(NOTIFY + notify_off * 4, 2, u32::from(VIRTIO_SND_QUEUE_EVENT));

    assert_eq!(read_u16(guest, used + 2), 2);
    assert_eq!(read_u32(guest, used + 16), 8);
    let expected_disconnected = {
        let mut evt = [0u8; 8];
        evt[0..4].copy_from_slice(&VIRTIO_SND_EVT_JACK_DISCONNECTED.to_le_bytes());
        evt[4..8].copy_from_slice(&JACK_ID_SPEAKER.to_le_bytes());
        evt
    };
    assert_eq!(
        &guest[buf as usize..buf as usize + 8],
        &expected_disconnected
    );
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_snapshot_roundtrip_restores_sample_rates_and_worklet_ring_state_when_attached()
 {
    let capacity_frames = 256;
    let channel_count = 2;

    let (guest_base1, guest_size1) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge1 =
        VirtioSndPciBridge::new(guest_base1, guest_size1, None).expect("VirtioSndPciBridge::new");

    bridge1
        .set_host_sample_rate_hz(96_000)
        .expect("set_host_sample_rate_hz");
    bridge1
        .set_capture_sample_rate_hz(44_100)
        .expect("set_capture_sample_rate_hz");

    let ring1 = WorkletBridge::new(capacity_frames, channel_count).expect("WorkletBridge::new");
    let sab1 = ring1.shared_buffer();
    bridge1
        .attach_audio_ring(sab1.clone(), capacity_frames, channel_count)
        .expect("attach_audio_ring");

    let expected_ring_state = AudioWorkletRingState {
        capacity_frames,
        read_pos: 7,
        write_pos: 42,
    };
    ring1.restore_state(&expected_ring_state);
    assert_eq!(bridge1.buffer_level_frames(), 35);

    let snap1 = bridge1.save_state();

    // Restore into a fresh bridge with a ring already attached; ring state should be applied
    // immediately during `load_state`.
    let (guest_base2, guest_size2) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge2 =
        VirtioSndPciBridge::new(guest_base2, guest_size2, None).expect("VirtioSndPciBridge::new");
    let ring2 = WorkletBridge::new(capacity_frames, channel_count).expect("WorkletBridge::new");
    let sab2 = ring2.shared_buffer();
    bridge2
        .attach_audio_ring(sab2, capacity_frames, channel_count)
        .expect("attach_audio_ring");

    bridge2.load_state(&snap1).expect("load_state");

    assert_eq!(ring2.snapshot_state(), expected_ring_state);
    assert_eq!(bridge2.buffer_level_frames(), 35);

    let snap2 = bridge2.save_state();
    let mut decoded = VirtioSndPciState::default();
    decoded.load_state(&snap2).expect("decode snapshot");
    assert_eq!(decoded.snd.host_sample_rate_hz, 96_000);
    assert_eq!(decoded.snd.capture_sample_rate_hz, 44_100);
    assert_eq!(decoded.worklet_ring, expected_ring_state);
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_deferred_worklet_ring_restore_is_applied_on_attach_and_handles_capacity_mismatch()
 {
    let capacity_frames = 8;
    let channel_count = 2;

    // Build a snapshot with an attached worklet ring in a non-trivial state.
    let (guest_base1, guest_size1) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge1 =
        VirtioSndPciBridge::new(guest_base1, guest_size1, None).expect("VirtioSndPciBridge::new");

    let ring = WorkletBridge::new(capacity_frames, channel_count).expect("WorkletBridge::new");
    let sab = ring.shared_buffer();
    bridge1
        .attach_audio_ring(sab.clone(), capacity_frames, channel_count)
        .expect("attach_audio_ring");

    let expected = AudioWorkletRingState {
        capacity_frames,
        read_pos: 2,
        write_pos: 6,
    };
    ring.restore_state(&expected);

    let snap = bridge1.save_state();

    // Restore into a bridge with *no* ring attached; ring state should be deferred until the host
    // attaches the ring.
    let (guest_base2, guest_size2) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge2 =
        VirtioSndPciBridge::new(guest_base2, guest_size2, None).expect("VirtioSndPciBridge::new");
    bridge2.load_state(&snap).expect("load_state");

    // Corrupt the ring indices so we can observe them being restored on attach.
    ring.restore_state(&AudioWorkletRingState {
        capacity_frames,
        read_pos: 123,
        write_pos: 125,
    });
    assert_ne!(ring.snapshot_state(), expected);

    bridge2
        .attach_audio_ring(sab.clone(), capacity_frames, channel_count)
        .expect("attach_audio_ring");
    assert_eq!(ring.snapshot_state(), expected);
    assert_eq!(bridge2.buffer_level_frames(), 4);

    // ---- Capacity mismatch path ----
    // Restore a snapshot whose worklet_ring.capacity_frames differs from the attached ring. The
    // bridge should clear the snapshot's capacity field before calling `WorkletBridge::restore_state`
    // so indices are clamped against the attached ring capacity (best-effort restore).
    let mismatch_ring_state = AudioWorkletRingState {
        capacity_frames: 16,
        read_pos: 0,
        write_pos: 20,
    };

    let mut mismatch_snapshot = VirtioSndPciState::default();
    mismatch_snapshot
        .load_state(&bridge2.save_state())
        .expect("decode snapshot");
    mismatch_snapshot.worklet_ring = mismatch_ring_state.clone();
    let mismatch_bytes = mismatch_snapshot.save_state();

    let (guest_base3, guest_size3) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge3 =
        VirtioSndPciBridge::new(guest_base3, guest_size3, None).expect("VirtioSndPciBridge::new");
    bridge3
        .load_state(&mismatch_bytes)
        .expect("load_state (mismatch)");

    let larger_capacity = 32;
    let ring3 = WorkletBridge::new(larger_capacity, channel_count).expect("WorkletBridge::new");
    let sab3 = ring3.shared_buffer();
    bridge3
        .attach_audio_ring(sab3, larger_capacity, channel_count)
        .expect("attach_audio_ring");

    let got = ring3.snapshot_state();
    assert_eq!(got.capacity_frames, larger_capacity);
    assert_eq!(got.read_pos, 0);
    assert_eq!(got.write_pos, 20);
    assert_eq!(bridge3.buffer_level_frames(), 20);
}
