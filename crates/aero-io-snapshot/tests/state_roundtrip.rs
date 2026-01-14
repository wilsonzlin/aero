use aero_io_snapshot::io::audio::state::{
    AudioWorkletRingState, HdaCodecCaptureState, HdaCodecState, HdaControllerState,
    HdaStreamRuntimeState, HdaStreamState, VirtioSndCaptureTelemetryState, VirtioSndPciState,
    VirtioSndPcmParamsState, VirtioSndState, VirtioSndStreamState,
};
use aero_io_snapshot::io::network::state::{
    DhcpLease, Ipv4Addr, LegacyNetworkStackState, NatKey, NatProtocol, NatValue, TcpRestorePolicy,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::{
    AhciControllerState, AhciHbaState, AhciPortState, DiskBackendState, DiskCacheState,
    DiskLayerState, DiskOverlayState, IdeAtaDeviceState, IdeAtapiDeviceState,
    IdeBusMasterChannelState, IdeChannelState, IdeControllerState, IdeDataMode, IdeDmaDirection,
    IdeDmaRequestState, IdeDriveState, IdePioWriteState, IdePortMapState, IdeTaskFileState,
    IdeTransferKind, LocalDiskBackendKind, LocalDiskBackendState, NvmeCompletionQueueState,
    NvmeControllerState, NvmeInFlightCommandState, NvmeSubmissionQueueState, PciConfigSpaceState,
    RemoteDiskBackendState, RemoteDiskBaseState, RemoteDiskValidator,
};
use aero_storage::SECTOR_SIZE;

#[test]
fn hda_controller_state_roundtrip() {
    let hda = HdaControllerState {
        gctl: 0x0000_0001,
        wakeen: 0x1234,
        statests: 0x5678,
        intctl: 0x9abc_def0,
        intsts: 0x1111_2222,
        output_rate_hz: 44_100,
        capture_sample_rate_hz: 48_000,
        dplbase: 0x0000_8000,
        dpubase: 0x0000_0001,

        corblbase: 0x0010_0000,
        corbubase: 0x0000_0002,
        corbwp: 7,
        corbrp: 3,
        corbctl: 0x12,
        corbsts: 0x34,
        corbsize: 0x02,

        rirblbase: 0x0020_0000,
        rirbubase: 0x0000_0003,
        rirbwp: 9,
        rirbctl: 0x56,
        rirbsts: 0x78,
        rirbsize: 0x02,
        rintcnt: 4,

        streams: vec![
            HdaStreamState {
                ctl: 0x1111_2222,
                lpib: 0x3333_4444,
                cbl: 0x0000_2000,
                lvi: 0x55,
                fifow: 0x66,
                fifos: 0x77,
                fmt: 0x0011,
                bdpl: 0x8888_9999,
                bdpu: 0xaaaa_bbbb,
            },
            HdaStreamState {
                ctl: 0xdead_beef,
                lpib: 0x0102_0304,
                cbl: 0x0000_1000,
                lvi: 0x02,
                fifow: 0x10,
                fifos: 0x20,
                fmt: 0x0010,
                bdpl: 0xcccc_dddd,
                bdpu: 0xeeee_ffff,
            },
        ],
        stream_runtime: vec![
            HdaStreamRuntimeState {
                bdl_index: 2,
                bdl_offset: 16,
                last_fmt_raw: 0x0011,
                resampler_src_pos_bits: 0x3ff0_0000_0000_0000, // 1.0f64
                resampler_queued_frames: 128,
            },
            HdaStreamRuntimeState {
                bdl_index: 1,
                bdl_offset: 0,
                last_fmt_raw: 0x0010,
                resampler_src_pos_bits: 0x4000_0000_0000_0000, // 2.0f64
                resampler_queued_frames: 0,
            },
        ],
        stream_capture_frame_accum: vec![0, 123_456_789],
        codec: HdaCodecState {
            output_stream_id: 1,
            output_channel: 0,
            output_format: 0x1234,
            amp_gain_left: 0x12,
            amp_gain_right: 0x34,
            amp_mute_left: true,
            amp_mute_right: false,
            pin_conn_select: 2,
            pin_ctl: 0x55,
            output_pin_power_state: 3,
            afg_power_state: 1,
        },
        codec_capture: HdaCodecCaptureState {
            input_stream_id: 2,
            input_channel: 1,
            input_format: 0x2345,
            amp_gain_left: 0x12,
            amp_gain_right: 0x34,
            amp_mute_left: true,
            amp_mute_right: false,
            mic_pin_conn_select: 1,
            mic_pin_ctl: 0x66,
            mic_pin_power_state: 2,
        },
        worklet_ring: AudioWorkletRingState {
            capacity_frames: 256,
            write_pos: 1024,
            read_pos: 900,
        },
    };

    let snap = hda.save_state();
    let mut restored = HdaControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(hda, restored);
}

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
        SECTOR_SIZE,
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
        SECTOR_SIZE,
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
    let mut regs = [0u8; 256];
    regs[0..4].copy_from_slice(&0x7010_8086u32.to_le_bytes());

    let ide = IdeControllerState {
        pci: PciConfigSpaceState {
            regs,
            bar0: 0x1f1,
            bar1: 0x3f5,
            bar2: 0x171,
            bar3: 0x375,
            bar4: 0xc001,
            bar0_probe: false,
            bar1_probe: true,
            bar2_probe: false,
            bar3_probe: false,
            bar4_probe: false,
            bus_master_base: 0xc000,
        },
        primary: IdeChannelState {
            ports: IdePortMapState {
                cmd_base: 0x1f0,
                ctrl_base: 0x3f6,
                irq: 14,
            },
            tf: IdeTaskFileState {
                features: 1,
                sector_count: 2,
                lba0: 3,
                lba1: 4,
                lba2: 5,
                device: 0xe0,
                hob_features: 6,
                hob_sector_count: 7,
                hob_lba0: 8,
                hob_lba1: 9,
                hob_lba2: 10,
                pending_features_high: true,
                pending_sector_count_high: false,
                pending_lba0_high: true,
                pending_lba1_high: false,
                pending_lba2_high: true,
            },
            status: 0x50,
            error: 0x01,
            control: 0x02,
            irq_pending: true,
            data_mode: IdeDataMode::PioIn,
            transfer_kind: Some(IdeTransferKind::AtaPioRead),
            data: vec![1, 2, 3, 4, 5],
            data_index: 2,
            pending_dma: Some(IdeDmaRequestState {
                direction: IdeDmaDirection::ToMemory,
                buffer: vec![9, 8, 7],
                commit: None,
            }),
            pio_write: Some(IdePioWriteState {
                lba: 0x1234,
                sectors: 2,
            }),
            bus_master: IdeBusMasterChannelState {
                cmd: 0x09,
                status: 0x04,
                prd_addr: 0x1000,
            },
            drives: [
                IdeDriveState::Ata(IdeAtaDeviceState { udma_mode: 2 }),
                IdeDriveState::Atapi(IdeAtapiDeviceState {
                    tray_open: false,
                    media_changed: true,
                    media_present: true,
                    sense_key: 0x06,
                    asc: 0x28,
                    ascq: 0,
                }),
            ],
        },
        secondary: IdeChannelState {
            ports: IdePortMapState {
                cmd_base: 0x170,
                ctrl_base: 0x376,
                irq: 15,
            },
            data_mode: IdeDataMode::None,
            transfer_kind: None,
            data: Vec::new(),
            data_index: 0,
            pending_dma: None,
            pio_write: None,
            bus_master: IdeBusMasterChannelState {
                cmd: 0,
                status: 0,
                prd_addr: 0,
            },
            drives: [IdeDriveState::None, IdeDriveState::None],
            ..Default::default()
        },
    };

    let snap = ide.save_state();
    let mut restored = IdeControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(ide, restored);
}

#[test]
fn ahci_state_roundtrip() {
    let ahci = AhciControllerState {
        hba: AhciHbaState {
            cap: 0x11,
            ghc: 0x22,
            cap2: 0x33,
            bohc: 0x44,
            vs: 0x55,
        },
        ports: vec![
            AhciPortState {
                clb: 0x1000,
                fb: 0x2000,
                is: 0x0001,
                ie: 0x0002,
                cmd: 0x0003,
                tfd: 0x0004,
                sig: 0x0005,
                ssts: 0x0006,
                sctl: 0x0007,
                serr: 0x0008,
                sact: 0x0009,
                ci: 0x000A,
            },
            AhciPortState {
                clb: 0x1100,
                fb: 0x2200,
                ci: 0x55aa,
                ..Default::default()
            },
        ],
    };

    let snap = ahci.save_state();
    let mut restored = AhciControllerState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(ahci, restored);
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
        feature_num_io_sqs: 0x12,
        feature_num_io_cqs: 0x34,
        feature_interrupt_coalescing: 0x5678,
        feature_volatile_write_cache: true,
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
    let mut net = LegacyNetworkStackState {
        mac_addr: [10, 11, 12, 13, 14, 15],
        dhcp_lease: Some(DhcpLease {
            ip: Ipv4Addr::new(192, 168, 0, 2),
            gateway: Ipv4Addr::new(192, 168, 0, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            lease_time_secs: 3600,
            acquired_at_tick: 123,
        }),
        ..Default::default()
    };
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
    let mut restored = LegacyNetworkStackState::default();
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
            amp_gain_left: 0x55,
            amp_gain_right: 0x66,
            amp_mute_left: false,
            amp_mute_right: true,
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

#[test]
fn virtio_snd_pci_state_roundtrip() {
    let state = VirtioSndPciState {
        virtio_pci: vec![0xaa, 0xbb, 0xcc],
        snd: VirtioSndState {
            playback: VirtioSndStreamState {
                state: 3,
                params: Some(VirtioSndPcmParamsState {
                    buffer_bytes: 4096,
                    period_bytes: 1024,
                    channels: 2,
                    format: 0x05,
                    rate: 0x07,
                }),
            },
            capture: VirtioSndStreamState {
                state: 2,
                params: Some(VirtioSndPcmParamsState {
                    buffer_bytes: 2048,
                    period_bytes: 512,
                    channels: 1,
                    format: 0x05,
                    rate: 0x07,
                }),
            },
            capture_telemetry: VirtioSndCaptureTelemetryState {
                dropped_samples: 123,
                underrun_samples: 456,
                underrun_responses: 7,
            },
            host_sample_rate_hz: 48_000,
            capture_sample_rate_hz: 44_100,
        },
        worklet_ring: AudioWorkletRingState {
            capacity_frames: 256,
            write_pos: 42,
            read_pos: 7,
        },
    };

    let snap = state.save_state();
    let mut restored = VirtioSndPciState::default();
    restored.load_state(&snap).unwrap();
    assert_eq!(state, restored);
}
