use aero_io_snapshot::io::audio::state::{
    AudioWorkletRingState, HdaCodecCaptureState, HdaCodecState, HdaControllerState,
    HdaStreamRuntimeState, HdaStreamState,
};
use aero_io_snapshot::io::network::state::{
    DhcpLease, Ipv4Addr, NatKey, NatProtocol, NatValue, NetworkStackState, TcpRestorePolicy,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::{
    DiskBackendState, DiskCacheState, DiskLayerState, DiskOverlayState, IdeControllerState,
    IdeInFlightCommandState, LocalDiskBackendKind, LocalDiskBackendState, NvmeCompletionQueueState,
    NvmeControllerState, NvmeInFlightCommandState, NvmeSubmissionQueueState,
    RemoteDiskBackendState, RemoteDiskBaseState, RemoteDiskValidator,
};

#[test]
fn disk_layer_state_roundtrip() {
    let disk = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Opfs,
            key: "disk0.aerospar".to_string(),
            overlay: Some(DiskOverlayState {
                file_name: "disk0.overlay.aerospar".to_string(),
                disk_size_bytes: 4096,
                block_size_bytes: 1024 * 1024,
            }),
        }),
        4096,
        512,
    );

    let snap = disk.save_state();
    let mut restored = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "ignored".to_string(),
            overlay: None,
        }),
        0,
        1,
    );
    restored.load_state(&snap).unwrap();
    assert_eq!(disk, restored);
}

#[test]
fn disk_layer_state_roundtrip_remote() {
    let disk = DiskLayerState::new(
        DiskBackendState::Remote(RemoteDiskBackendState {
            base: RemoteDiskBaseState {
                image_id: "win7-sp1-x64".to_string(),
                version: "sha256-deadbeef".to_string(),
                delivery_type: "range".to_string(),
                expected_validator: Some(RemoteDiskValidator::Etag("\"abc\"".to_string())),
                chunk_size: 1024 * 1024,
            },
            overlay: DiskOverlayState {
                file_name: "remote.overlay.aerospar".to_string(),
                disk_size_bytes: 4096,
                block_size_bytes: 1024 * 1024,
            },
            cache: DiskCacheState {
                file_name: "remote.cache.aerospar".to_string(),
            },
        }),
        4096,
        512,
    );

    let snap = disk.save_state();
    let mut restored = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "ignored".to_string(),
            overlay: None,
        }),
        0,
        1,
    );
    restored.load_state(&snap).unwrap();
    assert_eq!(disk, restored);
}

#[test]
fn ide_state_roundtrip() {
    let mut ide = IdeControllerState::default();
    ide.command = 0xec;
    ide.status = 0x50;
    ide.error = 0x01;
    ide.sector_count = 8;
    ide.lba = 0x1234_5678;
    ide.dma_active = true;
    ide.in_flight = Some(IdeInFlightCommandState {
        lba: 0xdead_beef,
        sector_count: 16,
        is_write: true,
    });

    let snap = ide.save_state();
    let mut restored = IdeControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(ide, restored);
}

#[test]
fn nvme_state_roundtrip() {
    let nvme = NvmeControllerState {
        cap: 0x11,
        vs: 0x22,
        intms: 0x33,
        intmc: 0x44,
        cc: 0x55,
        csts: 0x66,
        aqa: 0x77,
        asq: 0x8888,
        acq: 0x9999,
        admin_sq: Some(NvmeSubmissionQueueState {
            qid: 0,
            base: 0x1000,
            size: 16,
            head: 3,
            tail: 8,
            cqid: 0,
        }),
        admin_cq: Some(NvmeCompletionQueueState {
            qid: 0,
            base: 0x2000,
            size: 16,
            head: 1,
            tail: 2,
            phase: true,
            irq_enabled: true,
        }),
        io_sqs: vec![NvmeSubmissionQueueState {
            qid: 1,
            base: 0x3000,
            size: 64,
            head: 10,
            tail: 11,
            cqid: 1,
        }],
        io_cqs: vec![NvmeCompletionQueueState {
            qid: 1,
            base: 0x4000,
            size: 64,
            head: 12,
            tail: 13,
            phase: false,
            irq_enabled: false,
        }],
        intx_level: true,
        in_flight: vec![NvmeInFlightCommandState {
            cid: 7,
            opcode: 2,
            lba: 0xabcd,
            length: 4096,
        }],
    };

    let snap = nvme.save_state();
    let mut restored = NvmeControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(nvme, restored);
}

#[test]
fn network_stack_roundtrip_with_policy() {
    let mut net = NetworkStackState::default();
    net.mac_addr = [10, 11, 12, 13, 14, 15];
    net.dhcp_lease = Some(DhcpLease {
        ip: Ipv4Addr::new(192, 168, 0, 2),
        gateway: Ipv4Addr::new(192, 168, 0, 1),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        lease_time_secs: 3600,
        acquired_at_tick: 123,
    });
    net.nat.insert(
        NatKey {
            proto: NatProtocol::Tcp,
            inside_ip: Ipv4Addr::new(192, 168, 0, 2),
            inside_port: 1234,
            outside_port: 5555,
        },
        NatValue {
            remote_ip: Ipv4Addr::new(93, 184, 216, 34),
            remote_port: 80,
            last_seen_tick: 200,
        },
    );
    let conn_id = net.open_tcp_connection(Ipv4Addr::new(93, 184, 216, 34), 80);

    let snap = net.save_state();
    let mut restored = NetworkStackState::default();
    restored.load_state(&snap).unwrap();

    assert!(restored.tcp_proxy_conns.contains_key(&conn_id));
    assert_eq!(restored.nat.len(), 1);
    assert_eq!(restored.dhcp_lease, net.dhcp_lease);

    restored.apply_tcp_restore_policy(TcpRestorePolicy::Drop);
    assert!(restored.tcp_proxy_conns.is_empty());
}

#[test]
fn hda_state_roundtrip() {
    let hda = HdaControllerState {
        gctl: 1,
        wakeen: 0x00aa,
        statests: 0x0001,
        intctl: 2,
        intsts: 3,
        output_rate_hz: 44_100,
        capture_sample_rate_hz: 48_000,
        dplbase: 0x1000,
        dpubase: 0,
        corblbase: 0x2000,
        corbubase: 0,
        corbwp: 4,
        corbrp: 5,
        corbctl: 6,
        corbsts: 7,
        corbsize: 2,
        rirblbase: 0x3000,
        rirbubase: 0,
        rirbwp: 7,
        rirbctl: 8,
        rirbsts: 9,
        rirbsize: 2,
        rintcnt: 10,
        streams: vec![HdaStreamState {
            ctl: 0x10,
            lpib: 0x20,
            cbl: 0x30,
            lvi: 2,
            fifow: 0x12,
            fifos: 0x40,
            fmt: 0x4011,
            bdpl: 0x1000,
            bdpu: 0,
        }],
        stream_runtime: vec![HdaStreamRuntimeState {
            bdl_index: 1,
            bdl_offset: 128,
            last_fmt_raw: 0x4011,
            resampler_src_pos_bits: (0.5f64).to_bits(),
            resampler_queued_frames: 64,
        }],
        stream_capture_frame_accum: vec![3],
        codec: HdaCodecState {
            output_stream_id: 1,
            output_channel: 0,
            output_format: 0x4011,
            amp_gain_left: 0x7f,
            amp_gain_right: 0x7f,
            amp_mute_left: false,
            amp_mute_right: true,
            pin_conn_select: 0,
            pin_ctl: 0x40,
            output_pin_power_state: 2,
            afg_power_state: 0,
        },
        codec_capture: HdaCodecCaptureState {
            input_stream_id: 2,
            input_channel: 0,
            input_format: 0x0010,
            mic_pin_conn_select: 0,
            mic_pin_ctl: 0,
            mic_pin_power_state: 3,
        },
        worklet_ring: AudioWorkletRingState {
            capacity_frames: 48000,
            write_pos: 1024,
            read_pos: 512,
        },
    };

    let snap = hda.save_state();
    let mut restored = HdaControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(hda, restored);
}
